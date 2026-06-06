//! Multi-worker BSP superstep LOOP over the **real** `Compute::ExchangeHalo`
//! gRPC path (docs/compute-architecture.md §4, "superstep loop (BSP)").
//!
//! `exchange_halo_grpc.rs` proves a single boundary round-trip across the tonic
//! codec. This proves the distributed *loop*: `P` workers, each owning a
//! partition + its own `LayoutEngine`, run `K` supersteps and exchange boundary
//! positions via the real `TonicHaloTransport` / `HaloProvider` path every step.
//! Each worker's `ExchangeHalo` server is backed by a `LiveHaloProvider` that
//! returns that worker's *current* boundary delta for the requested frame (not a
//! fixed stub), so peers receive real, evolving positions.
//!
//! Transport: a full mesh of in-process `tokio::io::duplex` pipes (the pattern
//! proven in `exchange_halo_grpc.rs`), one per ordered peer pair. NO TCP port is
//! bound, so the whole `P`-worker mesh runs under the sandbox.
//!
//! `TODO(two-process):` a genuine two-OS-process test (real TCP/UDS, coordinator
//! brokering peer addresses) can't bind sockets under the sandbox; the
//! in-process-multi-worker-over-duplex mesh here is the accepted proof that the
//! gRPC codec is really in the loop. See `src/bsp.rs`.

use std::collections::HashMap;
use std::future::ready;
use std::sync::Arc;

use graph_compute::bsp::{run_bsp_mesh, BspWorker, LiveHaloProvider, MeshTransport};
use graph_compute::engines::{CsrShard, EngineCtx, HaloUpdate, LayoutEngine, StepOutput};
use graph_compute::partition::{partition_csr, Partition, TonicHaloTransport, Worker};
use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::service::ComputeService;
use graph_compute::sim::{CsrGraph, SimState};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use hyper_util::rt::TokioIo;
use tonic::transport::{Endpoint, Server, Uri};

/// Undirected ring 0—1—…—(n-1)—0. A ring split P ways has >1 cut, so partitions
/// carry multiple boundary/ghost nodes — a stronger exercise of the exchange
/// than a single path cut.
fn ring(n: u32) -> CsrGraph {
    let mut offsets = Vec::with_capacity((n + 1) as usize);
    let mut neighbors = Vec::new();
    for i in 0..n {
        offsets.push(neighbors.len() as u32);
        neighbors.push((i + n - 1) % n);
        neighbors.push((i + 1) % n);
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Deterministic test engine. Each owned node `g` relaxes its position
/// geometrically toward the fixed target `[g, 0, 0]` (a value that depends ONLY
/// on the global id, so the converged layout is partition-independent — the key
/// to comparing the distributed run against a single-worker reference). Received
/// peer halos are stored per global id so the test can read back exactly which
/// ghost positions the boundary exchange delivered.
struct RelaxEngine {
    descriptor: LayoutDescriptor,
    /// Owned global ids in ascending order (parallel to `pos`).
    owned: Vec<u32>,
    /// Interleaved x,y,z for owned nodes (parallel to `owned`).
    pos: Vec<f32>,
    /// Last position this worker received for each ghost global id via apply_halo.
    received: HashMap<u32, [f32; 3]>,
}

impl RelaxEngine {
    fn new(owned: Vec<u32>) -> Self {
        // Seed every owned node far from its target so convergence is observable.
        let mut pos = Vec::with_capacity(3 * owned.len());
        for _ in &owned {
            pos.extend_from_slice(&[100.0, 100.0, 100.0]);
        }
        Self {
            descriptor: LayoutDescriptor {
                id: "relax",
                kind: LayoutKind::Physics,
                display_name: "relax",
                description: "deterministic test relaxation engine",
                requirements: LayoutRequirements::default(),
            },
            owned,
            pos,
            received: HashMap::new(),
        }
    }
}

impl LayoutEngine for RelaxEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }
    fn init(&mut self, _ctx: &mut EngineCtx, _g: &CsrShard, _p: &[f32]) -> Result<(), String> {
        Ok(())
    }
    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        // Relax each owned node halfway to its global-id target [g, 0, 0].
        for (i, &g) in self.owned.iter().enumerate() {
            let target = [g as f32, 0.0, 0.0];
            for k in 0..3 {
                let p = &mut self.pos[3 * i + k];
                *p += 0.5 * (target[k] - *p);
            }
        }
        StepOutput::positions_only(self.pos.clone())
    }
    fn apply_halo(&mut self, h: &HaloUpdate) {
        for (i, &gid) in h.node_ids.iter().enumerate() {
            self.received.insert(
                gid,
                [
                    h.positions[3 * i],
                    h.positions[3 * i + 1],
                    h.positions[3 * i + 2],
                ],
            );
        }
    }
}

/// Stand up one worker's `ExchangeHalo` server (backed by `provider`) over a
/// fresh in-memory duplex pipe and return a `ComputeClient` connected through
/// the other end. No TCP bind.
async fn connect_peer(provider: Arc<LiveHaloProvider>) -> ComputeClient<tonic::transport::Channel> {
    // The SimState graph is irrelevant to ExchangeHalo; use a tiny one.
    let state = SimState::new(CsrGraph::path(2));
    let svc = ComputeService::new(state).with_halo_provider(provider);

    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    let mut client_io = Some(client_io);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex");
    ComputeClient::new(channel)
}

/// Build the full `P`-worker mesh: for every worker, one server (with its live
/// provider) + one `TonicHaloTransport` to every peer. Returns the assembled
/// `BspWorker`s plus the per-worker providers (kept alive for the run).
async fn build_mesh(parts: &[Partition]) -> Vec<BspWorker<RelaxEngine>> {
    let p = parts.len();
    let handle = tokio::runtime::Handle::current();

    // One live provider per worker — its own server reads this for outgoing halos.
    let providers: Vec<Arc<LiveHaloProvider>> =
        (0..p).map(|_| Arc::new(LiveHaloProvider::new())).collect();

    let mut bsp_workers = Vec::with_capacity(p);
    for (i, part) in parts.iter().enumerate() {
        // Worker i connects a transport to every peer j != i. The client targets
        // peer j's server, which reads peer j's provider.
        let mut peer_transports = Vec::with_capacity(p - 1);
        for (j, _peer_part) in parts.iter().enumerate() {
            if i == j {
                continue;
            }
            let client = connect_peer(providers[j].clone()).await;
            let transport = TonicHaloTransport::connect(part.partition_id, client, handle.clone())
                .expect("open TonicHaloTransport to peer");
            peer_transports.push(transport);
        }

        let engine = RelaxEngine::new(part.owned_global_ids().to_vec());
        bsp_workers.push(BspWorker {
            worker: Worker {
                partition: part.clone(),
                engine,
            },
            ctx: EngineCtx::cpu_only(),
            transport: MeshTransport::new(part.partition_id, peer_transports),
            provider: providers[i].clone(),
        });
    }
    bsp_workers
}

/// Single-worker reference: relax the whole graph (one partition) for `k` steps
/// and return global-id -> [x,y,z]. With the partition-independent target the
/// distributed run must match this.
fn reference_positions(g: &CsrGraph, k: u64) -> HashMap<u32, [f32; 3]> {
    let mut eng = RelaxEngine::new((0..g.n_nodes).collect());
    let mut ctx = EngineCtx::cpu_only();
    let mut last = Vec::new();
    for _ in 0..k {
        last = eng.step(&mut ctx).positions;
    }
    let mut out = HashMap::new();
    for (i, g) in (0..g.n_nodes).enumerate() {
        out.insert(g, [last[3 * i], last[3 * i + 1], last[3 * i + 2]]);
    }
    out
}

const TOL: f32 = 1e-3;

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() <= TOL)
}

/// Run the BSP loop over the gRPC mesh for a given graph + partition count and
/// assert (a) every worker's ghost nodes converge to the owner's positions and
/// (b) the partitioned layout matches the single-worker reference.
async fn run_and_assert(g: CsrGraph, np: u32, k: u64) {
    let parts = partition_csr(&g, None, np);
    assert_eq!(parts.len() as u32, np);
    // Precondition: there ARE ghosts to exchange (otherwise the test is vacuous).
    assert!(
        parts.iter().any(|p| !p.ghost_global_ids().is_empty()),
        "partition produced no ghosts — nothing to exchange"
    );

    let mut workers = build_mesh(&parts).await;

    // Drive K BSP supersteps over the REAL gRPC ExchangeHalo path. Run the
    // blocking driver off the async runtime so the per-peer tonic stream tasks
    // (which the blocking `collect`s wait on) keep making progress.
    let finals = tokio::task::spawn_blocking(move || {
        let finals = run_bsp_mesh(&mut workers, k);
        // Move `workers` out so we can read engine state after the run.
        (finals, workers)
    })
    .await
    .expect("BSP driver panicked");
    let (final_positions, workers) = finals;

    let reference = reference_positions(&g, k);

    // Reassemble the distributed owned positions into a global map and compare to
    // the single-worker reference (assertion b: structural match within tol).
    let mut dist: HashMap<u32, [f32; 3]> = HashMap::new();
    for w in &workers {
        let owned = w.worker.partition.owned_global_ids();
        let pos = &final_positions[w.worker.partition.partition_id as usize];
        for (i, &gid) in owned.iter().enumerate() {
            dist.insert(gid, [pos[3 * i], pos[3 * i + 1], pos[3 * i + 2]]);
        }
    }
    for (&gid, &p) in &dist {
        let r = reference[&gid];
        assert!(
            close(p, r),
            "node {gid}: distributed {p:?} != reference {r:?} (np={np}, k={k})"
        );
    }

    // Assertion (a): every worker's ghost positions converged to the owner's
    // positions for those nodes — i.e. the boundary exchange propagated real
    // positions across the loop. At least one ghost must have been exchanged.
    let mut total_ghosts = 0usize;
    for w in &workers {
        for &ghost in w.worker.partition.ghost_global_ids() {
            total_ghosts += 1;
            let got = w.worker.engine.received.get(&ghost).unwrap_or_else(|| {
                panic!(
                    "part {} never received a halo for ghost {ghost} (np={np})",
                    w.worker.partition.partition_id
                )
            });
            let owner_pos = dist[&ghost];
            assert!(
                close(*got, owner_pos),
                "part {} ghost {ghost}: received {got:?} != owner {owner_pos:?} (np={np})",
                w.worker.partition.partition_id
            );
        }
    }
    assert!(total_ghosts > 0, "no ghosts exchanged — vacuous test");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bsp_loop_two_workers_path() {
    // path(12) split in two: a single cut, one boundary/ghost per side.
    run_and_assert(CsrGraph::path(12), 2, 16).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bsp_loop_two_workers_ring() {
    // ring(12) split in two: two cuts, so each side has >1 boundary/ghost.
    run_and_assert(ring(12), 2, 16).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bsp_loop_three_workers_ring() {
    // P=3 over a ring: a full 3-way mesh of duplex pipes; multiple ghosts each.
    run_and_assert(ring(12), 3, 20).await;
}
