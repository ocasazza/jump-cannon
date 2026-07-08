//! End-to-end GPU proof: a **tvix-generated seed shape** laid out by each
//! **GPU compute engine**, observed over the **real gRPC `Subscribe` stream** —
//! the exact `PositionDelta` frames graph-api relays to the frontend WS
//! (`/graph/layout/stream`), byte-for-byte.
//!
//! For every GPU engine (`fa2-bh`, `geometric-gpu`, `sgd-stress-gpu`) this:
//!   1. generates a grid lattice via the shared Nix graph library (tvix-eval),
//!   2. asserts `init_engine` returns that engine id — i.e. the GPU engine
//!      really initialized on a wgpu adapter and did NOT silently fall back to
//!      the CPU spring engine (the discriminator between "ran on the GPU" and
//!      "no adapter"),
//!   3. subscribes to the gRPC stream and pulls frames, asserting the frame
//!      counter advances AND positions move — i.e. iterations are being
//!      processed and streamed.
//!
//! Needs a real wgpu adapter (Metal locally; software-Vulkan/lavapipe in CI).
//! Under the default command sandbox no adapter is visible, so the test SKIPS
//! (returns early, stays green). Run it for real with the sandbox disabled:
//!
//! ```text
//! GPU_PAGERANK_REQUIRE_ADAPTER=1 cargo test -p graph-compute \
//!     --test gpu_e2e_tvix_stream -- --nocapture
//! ```
//! (`GPU_PAGERANK_REQUIRE_ADAPTER=1` turns the no-adapter skip into a hard
//! failure, so a real run can't pass by accident.)

use std::collections::{BTreeMap, BTreeSet};
use std::future::ready;
use std::time::Duration;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::SubscribeRequest;
use graph_compute::service::{run_sim_loop, ComputeService};
use graph_compute::sim::{CsrGraph, SimState};
use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tvix_wasm::eval_graph;

mod common;

/// The GPU layout engines under test (registry ids). Each requires a wgpu
/// adapter; without one, `init_engine` falls back to `cpu-spring`.
const GPU_ENGINES: &[&str] = &["fa2-bh", "geometric-gpu", "sgd-stress-gpu"];

/// Wrap a Nix generator body in the standard preamble that makes the shared
/// embedded graph library importable, then render to `{ nodes, links }`.
fn gen_expr(body: &str) -> String {
    format!(
        "let g = import /jc/src/graph.nix {{}}; \
             gcl = import /jc/src/graph-combinators.nix {{ graph = g; }}; \
         in g.toGraphJSON ({body})"
    )
}

/// Evaluate a tvix generator → a symmetric [`CsrGraph`] via the canonical CSR
/// binary (`[u32 n][u32 n_edges][u32×(n+1) offsets][u32×n_edges neighbors]`,
/// LE) — the exact format graph-api pushes to the worker over `LoadGraph`.
fn nix_csr(body: &str) -> CsrGraph {
    let g = eval_graph(&gen_expr(body)).expect("tvix eval graph");

    // Stable, id-sorted node indexing (deterministic).
    let mut idx: BTreeMap<String, u32> = BTreeMap::new();
    for node in &g.nodes {
        let next = idx.len() as u32;
        idx.entry(node.id.clone()).or_insert(next);
    }
    let n = idx.len() as u32;

    // Symmetrize directed generator edges; drop self-loops; dedup.
    let mut undirected: BTreeSet<(u32, u32)> = BTreeSet::new();
    for e in &g.edges {
        let (a, b) = (idx[&e.source], idx[&e.target]);
        if a != b {
            undirected.insert((a.min(b), a.max(b)));
        }
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
    for (a, b) in undirected {
        adj[a as usize].push(b);
        adj[b as usize].push(a);
    }

    let mut offsets: Vec<u32> = Vec::with_capacity(n as usize + 1);
    let mut neighbors: Vec<u32> = Vec::new();
    for a in &adj {
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(a);
    }
    offsets.push(neighbors.len() as u32);
    let n_edges = neighbors.len() as u32;

    let mut bin = Vec::new();
    bin.extend_from_slice(&n.to_le_bytes());
    bin.extend_from_slice(&n_edges.to_le_bytes());
    for o in &offsets {
        bin.extend_from_slice(&o.to_le_bytes());
    }
    for nb in &neighbors {
        bin.extend_from_slice(&nb.to_le_bytes());
    }
    CsrGraph::from_bin_bytes(&bin).expect("build CsrGraph from CSR bin")
}

/// Serve `svc` over a single in-memory duplex pipe (no TCP bind) and return a
/// `Channel` connected through the other end — the standard tonic in-process
/// pattern (mirrors `tests/grpc_stream.rs`).
async fn connect_in_process(svc: ComputeService) -> Channel {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    let mut client_io = Some(client_io);
    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex")
}

/// What one engine's run produced, for the printed report.
struct EngineReport {
    engine: String,
    n_nodes: u32,
    frames: u32,
    first_frame: u64,
    last_frame: u64,
    /// Mean absolute per-coordinate displacement between the first and last
    /// observed frame — non-zero ⇒ the engine is actually iterating.
    mean_disp: f32,
    all_finite: bool,
}

/// Drive one GPU engine over the real service path with a tvix-generated graph.
async fn run_engine(engine_id: &str, graph: CsrGraph) -> EngineReport {
    let n_nodes = graph.n_nodes;
    let state = SimState::new(graph);

    // Init the requested engine. `init_engine` brings up its own wgpu context
    // (Metal here) and returns the engine that actually initialized — if the
    // GPU engine couldn't start it would come back as `cpu-spring`. Requiring
    // an exact match is what proves the layout ran on the GPU.
    let chosen = state
        .init_engine(engine_id, serde_json::Value::Null, None)
        .await
        .unwrap_or_else(|e| panic!("init_engine({engine_id:?}) errored: {e}"));
    assert_eq!(
        chosen, engine_id,
        "engine {engine_id:?} fell back to {chosen:?} — no GPU adapter reached \
         (run with the sandbox disabled so Metal is visible)"
    );

    // Spawn the sim loop (the sole PositionDelta producer) and the service.
    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });
    let channel = connect_in_process(ComputeService::new(state)).await;
    let mut client = ComputeClient::new(channel);

    let mut stream = client
        .subscribe(SubscribeRequest {
            graph_id: "gpu-e2e".into(),
            layout_id: engine_id.into(),
            ..Default::default()
        })
        .await
        .expect("subscribe")
        .into_inner();

    // Pull up to ~1.5s of frames.
    let mut first: Option<Vec<f32>> = None;
    let mut last: Vec<f32> = Vec::new();
    let mut first_frame = 0u64;
    let mut last_frame = 0u64;
    let mut frames = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && frames < 90 {
        let msg = match tokio::time::timeout(Duration::from_secs(2), stream.message()).await {
            Ok(Ok(Some(m))) => m,
            _ => break,
        };
        let pos: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&msg.positions).to_vec();
        assert_eq!(pos.len(), n_nodes as usize * 3, "frame arity mismatch");
        frames += 1;
        if first.is_none() {
            first = Some(pos.clone());
            first_frame = msg.frame;
        }
        last = pos;
        last_frame = msg.frame;
    }

    let first = first.expect("engine produced no frames");
    let mean_disp = first
        .iter()
        .zip(&last)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / first.len().max(1) as f32;
    let all_finite = last.iter().all(|v| v.is_finite());

    // The two E2E assertions: iterations advanced, and real work happened.
    assert!(
        last_frame > first_frame,
        "{engine_id}: frame counter did not advance ({first_frame} → {last_frame})"
    );
    assert!(
        mean_disp > 0.0 && all_finite,
        "{engine_id}: positions did not move (mean_disp={mean_disp}, finite={all_finite})"
    );

    EngineReport {
        engine: engine_id.to_string(),
        n_nodes,
        frames,
        first_frame,
        last_frame,
        mean_disp,
        all_finite,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gpu_engines_lay_out_tvix_seed_and_stream_frames() {
    // Skip (green) on hosts with no adapter; hard-fail if REQUIRE_ADAPTER is set.
    if common::gpu_ctx_or_skip("gpu_e2e_tvix_stream").is_none() {
        return;
    }

    // The seed shape: a 24×24 grid lattice, generated by the shared Nix graph
    // library via tvix-eval — 576 nodes, ~1104 edges. Big enough that the
    // layout keeps moving across the observation window (no early convergence).
    let graph = nix_csr("gcl.gridGen { rows = 24; cols = 24; prefix = \"n\"; }");
    let n = graph.n_nodes;
    eprintln!("\n=== GPU E2E: tvix gridGen seed = {n} nodes ===");

    let mut reports = Vec::new();
    for engine in GPU_ENGINES {
        // Fresh graph clone per engine (each gets its own SimState + service).
        let g = CsrGraph::from_bin_bytes(&graph.to_bin()).unwrap();
        reports.push(run_engine(engine, g).await);
    }

    eprintln!("\n{:<16} {:>7} {:>7} {:>13} {:>10} {:>7}", "engine", "nodes", "frames", "frame#(a→b)", "mean_disp", "finite");
    for r in &reports {
        eprintln!(
            "{:<16} {:>7} {:>7} {:>6}→{:<6} {:>10.4} {:>7}",
            r.engine, r.n_nodes, r.frames, r.first_frame, r.last_frame, r.mean_disp, r.all_finite
        );
    }
    eprintln!("=== all {} GPU engines processed the tvix seed and streamed advancing frames ===\n", reports.len());
}
