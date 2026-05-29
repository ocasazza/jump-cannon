//! Multi-worker BSP superstep driver over the **real** `Compute::ExchangeHalo`
//! gRPC transport (docs/compute-architecture.md §4, "superstep loop (BSP)").
//!
//! [`partition`](crate::partition) ships the data structures (per-shard CSR +
//! ghost tables + [`HaloDelta`]) and the single-worker [`run_superstep_local`]
//! demonstration over the in-memory [`LocalTransport`]. The
//! `exchange_halo_grpc` integration test proves a *single* boundary round-trip
//! across the tonic codec. This module closes the loop between them: it drives
//! **P workers for K supersteps**, each owning a partition + its own
//! [`LayoutEngine`], exchanging boundary positions every step over
//! [`TonicHaloTransport`] / [`HaloProvider`] — the real gRPC path (doc §4 step
//! c), with the BSP barrier (step d) realized by the transport's per-frame
//! `collect`.
//!
//! ## What is "real" here
//!
//! The boundary `HaloDelta`s cross the tonic codec both ways every superstep:
//! each worker runs a [`ComputeService`] whose [`HaloProvider`] returns *that
//! worker's live boundary delta* for the requested frame (not a fixed stub), and
//! connects a [`TonicHaloTransport`] to every peer's `ExchangeHalo` stream. The
//! [`run_superstep`] body — `engine.step` → derive boundary delta → publish →
//! barrier-collect → `apply_halo` — is reused verbatim; only the transport is
//! the network one instead of [`LocalTransport`].
//!
//! ## Topology: a full mesh of in-process duplex pipes (NO TCP)
//!
//! For `P` workers we stand up `P` `ExchangeHalo` servers and, for every ordered
//! peer pair `(i, j)`, an in-memory `tokio::io::duplex` pipe whose server end is
//! served by worker `j` and whose client end backs worker `i`'s
//! [`TonicHaloTransport`] to `j`. No port is bound, so the whole mesh runs under
//! the sandbox. A worker's per-step exchange fans its boundary delta out to all
//! `P-1` peers and gathers all `P-1` replies via [`MeshTransport`], which adapts
//! the one-peer [`TonicHaloTransport`] to the many-peer BSP barrier.
//!
//! `TODO(two-process):` this proves the codec + loop in one process over duplex
//! pipes. A genuine *two-process* deployment (separate OS processes, real TCP /
//! UDS, coordinator brokering peer addresses) is the remaining gap — infeasible
//! to bind sockets under the sandbox, so it is left as a documented follow-up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::engines::{EngineCtx, LayoutEngine};
use crate::partition::{HaloDelta, HaloTransport, TonicHaloTransport, Worker};
use crate::service::HaloProvider;

/// A [`HaloProvider`] backed by a worker's **live** per-frame boundary delta.
///
/// Each superstep the driver publishes this worker's freshly-derived boundary
/// `HaloDelta` for frame `f` via [`publish`](LiveHaloProvider::publish) *before*
/// any peer can request it (the BSP compute phase precedes the exchange phase).
/// When a peer then streams its frame-`f` request into this worker's
/// `ExchangeHalo` handler, [`outgoing_for`](HaloProvider::outgoing_for) returns
/// the stored delta — so peers receive this worker's real, current boundary
/// positions, not a fixed stub.
///
/// This is the production-shaped seam from `docs/compute-architecture.md` §4: a
/// real worker installs a provider reading its [`Partition`] + live owned
/// positions; here that "live owned positions → boundary delta" projection is
/// done by the driver (via [`Partition::boundary_delta`], indirectly through
/// [`run_superstep`]) and handed to the provider per frame.
#[derive(Default)]
pub struct LiveHaloProvider {
    /// `frame -> this worker's boundary delta for that frame`. Populated by the
    /// driver each superstep before the exchange phase; drained lazily (kept so
    /// a peer that lags by a frame still finds it).
    frames: Mutex<HashMap<u64, HaloDelta>>,
}

impl LiveHaloProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record this worker's boundary delta for `frame` so peers requesting that
    /// frame get the live positions. Overwrites any prior entry for the frame.
    pub fn publish(&self, frame: u64, delta: HaloDelta) {
        self.frames.lock().unwrap().insert(frame, delta);
    }
}

impl HaloProvider for LiveHaloProvider {
    fn outgoing_for(&self, frame: u64, _inbound: &HaloDelta) -> Vec<HaloDelta> {
        // Return the stored live boundary delta for the requested frame. Empty
        // if not yet recorded (a peer that raced ahead) — the peer's `collect`
        // then simply gathers nothing from us this frame.
        match self.frames.lock().unwrap().get(&frame) {
            Some(d) => vec![d.clone()],
            None => Vec::new(),
        }
    }
}

/// Fan a single worker's boundary exchange across **all** its peers by wrapping
/// one [`TonicHaloTransport`] per peer.
///
/// [`run_superstep`] is generic over [`HaloTransport`] and was written for the
/// one-mailbox model; a real worker in a `P`-way partition must publish its
/// boundary to every peer and collect from every peer to complete the BSP
/// barrier. `MeshTransport` is that adapter: [`publish`] forwards the delta to
/// each per-peer tonic stream, and [`collect`] concatenates every peer's reply
/// for the frame (each per-peer `collect` already blocks until that peer's
/// frame-`f` reply lands, so their conjunction IS the all-peers barrier).
pub struct MeshTransport {
    self_id: u32,
    peers: Vec<TonicHaloTransport>,
}

impl MeshTransport {
    pub fn new(self_id: u32, peers: Vec<TonicHaloTransport>) -> Self {
        Self { self_id, peers }
    }

    pub fn self_id(&self) -> u32 {
        self.self_id
    }
}

impl HaloTransport for MeshTransport {
    fn publish(&mut self, frame: u64, delta: HaloDelta) {
        // Same outgoing boundary delta to every peer; each peer's server folds
        // it (or ignores it) and replies with its own boundary.
        for peer in &mut self.peers {
            peer.publish(frame, delta.clone());
        }
    }

    fn collect(&mut self, frame: u64) -> Vec<HaloDelta> {
        // Gather each peer's frame-`f` reply. Each per-peer `collect` blocks the
        // BSP thread until that peer answers, so completing this loop is exactly
        // the "barrier on all P-1 peers" of doc §4 step d.
        let mut out = Vec::new();
        for peer in &mut self.peers {
            out.extend(peer.collect(frame));
        }
        out
    }
}

/// One worker's full BSP context for the distributed driver: its
/// [`Worker`] (partition + engine), its engine [`EngineCtx`], the
/// [`MeshTransport`] to its peers, and the [`LiveHaloProvider`] the worker's own
/// `ExchangeHalo` server reads when peers ask for its boundary.
pub struct BspWorker<E: LayoutEngine> {
    pub worker: Worker<E>,
    pub ctx: EngineCtx,
    pub transport: MeshTransport,
    pub provider: Arc<LiveHaloProvider>,
}

impl<E: LayoutEngine> BspWorker<E> {
    /// Run one BSP superstep's **compute + publish-snapshot** phase: step the
    /// engine, derive this worker's boundary delta, and record it in the live
    /// provider so peers requesting `frame` get fresh positions. Returns the
    /// owned positions produced this step (kept by the caller as the latest
    /// broadcastable layout) and the boundary delta that was snapshotted.
    ///
    /// Split from the exchange phase so the driver can store ALL workers'
    /// frame-`f` snapshots before ANY worker triggers a peer request — the BSP
    /// invariant that makes a live (non-stub) provider safe across async server
    /// tasks.
    fn compute_phase(&mut self, frame: u64) -> (Vec<f32>, HaloDelta) {
        let out = self.worker.engine.step(&mut self.ctx);
        let delta = match out.boundary {
            Some(ref halo) => HaloDelta::from_halo(halo),
            None => self.worker.partition.boundary_delta(frame, &out.positions),
        };
        self.provider.publish(frame, delta.clone());
        (out.positions, delta)
    }
}

/// Drive `n_supersteps` BSP rounds across `workers`, exchanging boundary
/// positions every step over the **real** gRPC `ExchangeHalo` transport
/// (docs/compute-architecture.md §4 steps a–d). Returns each worker's final
/// owned positions, indexed by `partition_id`.
///
/// Per superstep:
///   1. **Compute phase (BSP step a/b).** Every worker steps its engine and
///      snapshots its boundary delta into its [`LiveHaloProvider`]. Doing this
///      for *all* workers first guarantees every worker's frame-`f` boundary is
///      available before any peer's request reaches its server (the requests are
///      generated in step 2's publish).
///   2. **Exchange + barrier (BSP step c/d).** Every worker publishes its
///      boundary to all peers over its [`MeshTransport`] and blocks collecting
///      every peer's reply, then folds each peer halo in via `apply_halo`.
///
/// This reuses [`run_superstep`]'s exact publish→collect→apply body for step 2;
/// the only thing this driver adds is the two-phase ordering that a live
/// provider needs and the `MeshTransport` fan-out.
///
/// `TODO(barrier-concurrency):` step 1 and step 2 run each worker sequentially
/// on the calling thread; the per-peer tonic streams are driven on the tokio
/// runtime, so the blocking `collect`s still make progress, but a truly
/// concurrent barrier would step all workers in parallel. Sequential is correct
/// here because the compute phase fully precedes the exchange phase.
pub fn run_bsp_mesh<E: LayoutEngine>(
    workers: &mut [BspWorker<E>],
    n_supersteps: u64,
) -> Vec<Vec<f32>> {
    let mut last: Vec<Vec<f32>> = vec![Vec::new(); workers.len()];

    for frame in 0..n_supersteps {
        // Phase 1 (BSP a/b): every worker computes + snapshots its boundary so
        // the live providers are populated before any peer can ask.
        let mut snapshots: Vec<(Vec<f32>, HaloDelta)> = Vec::with_capacity(workers.len());
        for w in workers.iter_mut() {
            snapshots.push(w.compute_phase(frame));
        }

        // Phase 2 (BSP c/d): every worker publishes to all peers, blocks on the
        // barrier, and applies peer halos. We re-derive the published delta from
        // the snapshot so the engine is NOT stepped a second time — publish the
        // already-computed boundary, then collect + apply.
        for (w, (positions, delta)) in workers.iter_mut().zip(snapshots.into_iter()) {
            w.transport.publish(frame, delta);
            for peer in w.transport.collect(frame) {
                w.worker.engine.apply_halo(&peer.into_halo());
            }
            last[w.worker.partition.partition_id as usize] = positions;
        }
    }

    last
}

/// Re-export the single-worker per-step entry point so callers that already hold
/// a [`Worker`] + [`HaloTransport`] (e.g. a future real worker process) can run
/// one superstep without the mesh driver. Kept here so the BSP surface is
/// discoverable from one module.
pub use crate::partition::run_superstep as run_superstep_one;
