//! Distributed scaffold: CSR partitioning, ghost-node tables, and the BSP
//! superstep skeleton (`docs/compute-architecture.md` §4, Phase 6).
//!
//! This is the **least-verified, forward-looking** phase. It is a *scaffold*,
//! not a distributed runtime: it gives the coordinator the data structures it
//! needs (per-shard CSR blocks + ghost tables), a host-readable `HaloDelta`
//! wire payload that generalizes `PositionDelta`, and a single-process
//! demonstration of the bulk-synchronous-parallel (BSP) superstep loop driving
//! the [`LayoutEngine`] trait's `shard` + `apply_halo` hooks. The actual
//! cross-process network transport is left as a documented trait seam
//! ([`HaloTransport`]) — see the TODOs throughout.
//!
//! ## The model (doc §4)
//!
//! The standard pattern is **partition CSR → ghost boundary nodes → BSP
//! supersteps exchanging only boundary positions**:
//!
//! ```text
//!   coordinator: partition CSR into P blocks (edge-cut); assign block p → worker p
//!                compute each block's ghost list (boundary neighbors owned elsewhere)
//!
//!   superstep loop (BSP), per worker:
//!     a. local forces (own + ghost contributions)
//!     b. integrate own positions          ── LayoutEngine::step
//!     c. exchange boundary positions       ── StepOutput.boundary → peers,
//!                                             peer halos → LayoutEngine::apply_halo
//!     d. barrier; repeat
//! ```
//!
//! ## Partition quality
//!
//! v1 ships a cheap **BFS-region edge-cut**: grow `P` contiguous regions by
//! breadth-first frontier expansion, balancing on owned-node count. This keeps
//! each partition connected (good for locality, minimizes ghost count vs. a
//! pure range split) without a heavyweight library. METIS-quality recursive
//! bisection / k-way refinement is the documented follow-up — see
//! `TODO(metis)`. The trait/data-structure surface here does not change when a
//! better partitioner is dropped in.
//!
//! ## What this does NOT do (explicit non-goals for the scaffold)
//!
//!   - No real multi-process / multi-host transport. [`HaloTransport`] is a
//!     trait with an in-memory [`LocalTransport`] test double only.
//!   - No far-field remote centers-of-mass for Barnes-Hut/FMM (doc §4 notes
//!     these are needed for correct long-range forces under partitioning) —
//!     `TODO(far-field-com)`.
//!   - No fault tolerance / dynamic repartitioning / load rebalancing.

use std::collections::VecDeque;

use crate::engines::{HaloUpdate, ShardMeta};
use crate::sim::CsrGraph;

/// One partition's worth of data the coordinator hands to a worker.
///
/// `local` is a *self-contained* CSR over the **local node index space**
/// `[0, n_local)` where `n_local = owned.len() + ghost.len()`. Local indices
/// `[0, owned.len())` are owned nodes; `[owned.len(), n_local)` are read-only
/// ghosts. `global_ids[local_idx]` recovers the original global node id, and
/// [`Partition::meta`] packages the owned/ghost split as the foundation's
/// [`ShardMeta`] so it drops straight into `CsrShard { graph, shard: Some(..) }`.
#[derive(Clone, Debug)]
pub struct Partition {
    /// This partition's index in `[0, n_partitions)`.
    pub partition_id: u32,
    pub n_partitions: u32,
    /// Local-index CSR (owned nodes first, then ghosts). Ghost rows are
    /// included so local force kernels can read ghost adjacency, but ghosts are
    /// never integrated (their positions arrive via `apply_halo`).
    pub local: CsrGraph,
    /// `global_ids[local_idx] -> global node id`. Length `local.n_nodes`.
    pub global_ids: Vec<u32>,
    /// Number of owned (integrated) nodes; they occupy local indices
    /// `[0, n_owned)`.
    pub n_owned: u32,
}

impl Partition {
    /// Global ids of owned nodes (local indices `[0, n_owned)`).
    pub fn owned_global_ids(&self) -> &[u32] {
        &self.global_ids[..self.n_owned as usize]
    }

    /// Global ids of ghost nodes (local indices `[n_owned, n_nodes)`).
    pub fn ghost_global_ids(&self) -> &[u32] {
        &self.global_ids[self.n_owned as usize..]
    }

    /// Package the owned/ghost split as a foundation [`ShardMeta`] so this
    /// partition can be wrapped as a `CsrShard { graph: &self.local, shard:
    /// Some(part.meta()) }` and fed to [`LayoutEngine::init`].
    pub fn meta(&self) -> ShardMeta {
        ShardMeta {
            partition_id: self.partition_id,
            n_partitions: self.n_partitions,
            owned_node_ids: self.owned_global_ids().to_vec(),
            ghost_node_ids: self.ghost_global_ids().to_vec(),
        }
    }
}

/// Partition a global [`CsrGraph`] into `n_partitions` edge-cut blocks, each
/// with its ghost-node table.
///
/// Strategy (v1): **BFS-region growth**. Assign every node an owner via `P`
/// balanced breadth-first regions, then for each region build a self-contained
/// local CSR that includes (a) all owned nodes and (b) the *ghost* set — owned
/// nodes' neighbors that live in other partitions, appended read-only after the
/// owned block. This is a standard edge-cut partition: cut edges become the
/// owned↔ghost references whose far endpoint is refreshed each superstep.
///
/// `TODO(metis):` replace the BFS heuristic with METIS-style recursive
/// bisection + k-way refinement for a smaller edge cut (fewer ghosts ⇒ less
/// halo traffic). The return type is unaffected.
pub fn partition_csr(graph: &CsrGraph, n_partitions: u32) -> Vec<Partition> {
    let n = graph.n_nodes as usize;
    let p = n_partitions.max(1);
    if n == 0 {
        return (0..p)
            .map(|pid| Partition {
                partition_id: pid,
                n_partitions: p,
                local: CsrGraph {
                    n_nodes: 0,
                    offsets: vec![0],
                    neighbors: Vec::new(),
                },
                global_ids: Vec::new(),
                n_owned: 0,
            })
            .collect();
    }

    let owner = assign_owners_bfs(graph, p);

    // Bucket owned global ids per partition (ascending global order — keeps the
    // local index ↔ global id mapping deterministic).
    let mut owned: Vec<Vec<u32>> = vec![Vec::new(); p as usize];
    for (g, &o) in owner.iter().enumerate() {
        owned[o as usize].push(g as u32);
    }

    owned
        .into_iter()
        .enumerate()
        .map(|(pid, owned_ids)| {
            build_partition(graph, &owner, pid as u32, p, owned_ids)
        })
        .collect()
}

/// Assign each node an owner partition via balanced BFS regions. Returns
/// `owner[global_id] -> partition_id`.
///
/// Seeds the next region from the lowest-numbered unassigned node, then expands
/// a BFS frontier until the region reaches its target size (`ceil(n / p)`),
/// rounding into the final region. Disconnected components are absorbed by the
/// "reseed from lowest unassigned" rule.
fn assign_owners_bfs(graph: &CsrGraph, p: u32) -> Vec<u32> {
    let n = graph.n_nodes as usize;
    let target = n.div_ceil(p as usize).max(1);
    let mut owner = vec![u32::MAX; n];
    let mut assigned = 0usize;
    let mut next_seed = 0usize;

    for pid in 0..p {
        // Last partition takes whatever remains (handles rounding).
        let cap = if pid + 1 == p {
            usize::MAX
        } else {
            target
        };
        let mut count = 0usize;
        let mut queue: VecDeque<usize> = VecDeque::new();

        while count < cap && assigned < n {
            // (Re)seed from the lowest unassigned node when the frontier drains
            // — absorbs disconnected components into the current region.
            if queue.is_empty() {
                while next_seed < n && owner[next_seed] != u32::MAX {
                    next_seed += 1;
                }
                if next_seed >= n {
                    break;
                }
                queue.push_back(next_seed);
                owner[next_seed] = pid;
                assigned += 1;
                count += 1;
            }
            let Some(v) = queue.pop_front() else { break };
            let start = graph.offsets[v] as usize;
            let end = graph.offsets[v + 1] as usize;
            for &u in &graph.neighbors[start..end] {
                let u = u as usize;
                if owner[u] == u32::MAX && count < cap {
                    owner[u] = pid;
                    assigned += 1;
                    count += 1;
                    queue.push_back(u);
                }
            }
        }
    }

    // Safety net: any node the loop missed (shouldn't happen) goes to the last
    // partition.
    for o in owner.iter_mut() {
        if *o == u32::MAX {
            *o = p - 1;
        }
    }
    owner
}

/// Build one partition's self-contained local CSR + global-id map + ghost
/// table from the global graph and the owner assignment.
fn build_partition(
    graph: &CsrGraph,
    owner: &[u32],
    pid: u32,
    n_partitions: u32,
    owned_ids: Vec<u32>,
) -> Partition {
    use std::collections::HashMap;

    let n_owned = owned_ids.len() as u32;

    // local index ↔ global id. Owned nodes first.
    let mut global_ids: Vec<u32> = owned_ids.clone();
    let mut global_to_local: HashMap<u32, u32> = HashMap::with_capacity(owned_ids.len());
    for (li, &g) in owned_ids.iter().enumerate() {
        global_to_local.insert(g, li as u32);
    }

    // Discover ghosts: neighbors of owned nodes that are owned by *other*
    // partitions. Appended after the owned block, in first-seen order.
    let mut ghost_ids: Vec<u32> = Vec::new();
    for &g in &owned_ids {
        let gv = g as usize;
        let start = graph.offsets[gv] as usize;
        let end = graph.offsets[gv + 1] as usize;
        for &u in &graph.neighbors[start..end] {
            if owner[u as usize] != pid && !global_to_local.contains_key(&u) {
                let li = (n_owned as usize + ghost_ids.len()) as u32;
                global_to_local.insert(u, li);
                ghost_ids.push(u);
            }
        }
    }
    global_ids.extend_from_slice(&ghost_ids);

    let n_local = global_ids.len();

    // Build the local CSR. Owned rows carry full (remapped) adjacency — edges
    // to other owned nodes and to ghosts. Ghost rows are emitted EMPTY: ghosts
    // are read-only boundary copies, not integrated, so their outgoing
    // adjacency is irrelevant to this partition's force accumulation.
    //
    // TODO(ghost-adjacency): some kernels (e.g. overlap-prevention) may want
    // ghost↔ghost edges too; v1 omits them to keep the cut minimal.
    let mut offsets: Vec<u32> = Vec::with_capacity(n_local + 1);
    let mut neighbors: Vec<u32> = Vec::new();
    offsets.push(0);
    for li in 0..n_local {
        if li < n_owned as usize {
            let g = global_ids[li] as usize;
            let start = graph.offsets[g] as usize;
            let end = graph.offsets[g + 1] as usize;
            for &u in &graph.neighbors[start..end] {
                if let Some(&lu) = global_to_local.get(&u) {
                    neighbors.push(lu);
                }
            }
        }
        offsets.push(neighbors.len() as u32);
    }

    Partition {
        partition_id: pid,
        n_partitions,
        local: CsrGraph {
            n_nodes: n_local as u32,
            offsets,
            neighbors,
        },
        global_ids,
        n_owned,
    }
}

// ---------------------------------------------------------------------------
// HaloDelta wire payload (generalization of PositionDelta — doc §4)
// ---------------------------------------------------------------------------

/// Host-readable boundary-position payload exchanged between workers each
/// superstep. This is the in-Rust mirror of the `HaloDelta` protobuf message
/// (see the module-level wiring notes for the exact proto to add). Bulk numeric
/// fields stay **raw little-endian** per the repo wire rule; only the framing
/// (frame, owner_id, counts) is structured.
///
/// It is a thin alias-shaped wrapper around the foundation's [`HaloUpdate`]
/// (which carries the same fields as typed `Vec`s): [`from_halo`]/[`into_halo`]
/// convert, and [`encode`]/[`decode`] handle the raw-LE bytes ↔ typed form
/// that the proto `bytes` fields require.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HaloDelta {
    pub frame: u64,
    /// Partition id of the worker that owns these nodes.
    pub owner_id: u32,
    /// Global ids of the boundary nodes whose positions follow.
    pub node_ids: Vec<u32>,
    /// Interleaved `x,y,z` f32, parallel to `node_ids` (so `positions.len() ==
    /// 3 * node_ids.len()`).
    pub positions: Vec<f32>,
}

impl HaloDelta {
    /// Build from the foundation's [`HaloUpdate`] (same fields, typed).
    pub fn from_halo(h: &HaloUpdate) -> Self {
        Self {
            frame: h.frame,
            owner_id: h.owner_id,
            node_ids: h.node_ids.clone(),
            positions: h.positions.clone(),
        }
    }

    /// Convert into a foundation [`HaloUpdate`] for [`LayoutEngine::apply_halo`].
    pub fn into_halo(self) -> HaloUpdate {
        HaloUpdate {
            frame: self.frame,
            owner_id: self.owner_id,
            node_ids: self.node_ids,
            positions: self.positions,
        }
    }

    /// Encode the two bulk arrays to raw little-endian bytes, as the proto
    /// `node_ids` (LE u32) and `positions` (LE f32) `bytes` fields expect.
    /// Returns `(node_ids_le, positions_le)`.
    pub fn encode_bytes(&self) -> (Vec<u8>, Vec<u8>) {
        (
            bytemuck::cast_slice(&self.node_ids).to_vec(),
            bytemuck::cast_slice(&self.positions).to_vec(),
        )
    }

    /// Decode from the proto wire form (frame + owner_id + raw-LE byte blobs).
    /// Errors on misaligned byte lengths.
    pub fn decode_bytes(
        frame: u64,
        owner_id: u32,
        node_ids_le: &[u8],
        positions_le: &[u8],
    ) -> Result<Self, String> {
        if node_ids_le.len() % 4 != 0 {
            return Err(format!(
                "HaloDelta node_ids byte length {} not a multiple of 4",
                node_ids_le.len()
            ));
        }
        if positions_le.len() % 4 != 0 {
            return Err(format!(
                "HaloDelta positions byte length {} not a multiple of 4",
                positions_le.len()
            ));
        }
        let node_ids: Vec<u32> = bytemuck::cast_slice(node_ids_le).to_vec();
        let positions: Vec<f32> = bytemuck::cast_slice(positions_le).to_vec();
        if positions.len() != 3 * node_ids.len() {
            return Err(format!(
                "HaloDelta positions len {} != 3 * node_ids len {}",
                positions.len(),
                node_ids.len()
            ));
        }
        Ok(Self {
            frame,
            owner_id,
            node_ids,
            positions,
        })
    }
}

// ---------------------------------------------------------------------------
// Network transport seam (documented TODO — no real multi-process impl)
// ---------------------------------------------------------------------------

/// Abstract halo transport between workers. The BSP loop ([`run_superstep`])
/// talks to peers *only* through this trait, so the rest of the scaffold is
/// transport-agnostic.
///
/// `TODO(transport):` a real implementation streams `HaloDelta`s over gRPC
/// (a new `Compute::ExchangeHalo(stream HaloDelta) returns (stream HaloDelta)`
/// RPC, or a side channel) between worker processes, with the coordinator
/// brokering peer addresses. The scaffold ships only [`LocalTransport`], an
/// in-process double for tests.
pub trait HaloTransport: Send {
    /// Publish this worker's outgoing boundary positions for the given frame.
    fn publish(&mut self, frame: u64, delta: HaloDelta);

    /// Block until every peer's halo for `frame` is available, then return
    /// them. The BSP barrier (step `d`) lives here.
    ///
    /// `TODO(barrier):` the real impl waits on all `n_partitions - 1` peers
    /// (with a timeout / failure policy). The local double returns immediately.
    fn collect(&mut self, frame: u64) -> Vec<HaloDelta>;
}

/// In-process [`HaloTransport`] double: a shared per-frame mailbox. Lets the
/// single-process [`run_superstep_local`] demo and the tests exercise the BSP
/// loop without any network. NOT a distributed transport.
#[derive(Default)]
pub struct LocalTransport {
    /// `inbox[frame]` accumulates every worker's published delta for that frame.
    inbox: std::collections::HashMap<u64, Vec<HaloDelta>>,
    /// This transport's own partition id, so `collect` can exclude self.
    self_id: u32,
}

impl LocalTransport {
    pub fn new(self_id: u32) -> Self {
        Self {
            inbox: std::collections::HashMap::new(),
            self_id,
        }
    }
}

impl HaloTransport for LocalTransport {
    fn publish(&mut self, frame: u64, delta: HaloDelta) {
        self.inbox.entry(frame).or_default().push(delta);
    }

    fn collect(&mut self, frame: u64) -> Vec<HaloDelta> {
        self.inbox
            .remove(&frame)
            .map(|v| v.into_iter().filter(|d| d.owner_id != self.self_id).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// BSP superstep skeleton
// ---------------------------------------------------------------------------

/// One worker's per-superstep handle: its partition + the engine running on it.
///
/// `TODO(wire-engine):` in the real distributed worker this wraps an
/// `ActiveEngine` (`Box<dyn LayoutEngine>` + `EngineCtx`). The scaffold keeps
/// the shape generic over the trait so it compiles without constructing GPU
/// state.
pub struct Worker<E: crate::engines::LayoutEngine> {
    pub partition: Partition,
    pub engine: E,
}

/// Run ONE BSP superstep for one worker against a transport (doc §4 steps a–d):
///
///   a/b. local forces + integrate — `engine.step(ctx)` (the engine already
///        accumulates owned + ghost contributions from the local CSR).
///   c.   exchange: publish this worker's boundary positions, collect peers'.
///   d.   barrier + apply: fold each peer halo in via `engine.apply_halo`.
///
/// Returns the owned-node positions this worker produced this superstep (the
/// payload the coordinator broadcasts as a `PositionDelta`).
///
/// The boundary slice shipped to peers is taken from `StepOutput.boundary` when
/// the engine produced one; otherwise this falls back to deriving it from the
/// owned positions + the partition's ghost membership as seen by peers —
/// `TODO(boundary-derivation):` a real engine should emit `StepOutput.boundary`
/// itself (it knows which of its owned nodes are some peer's ghost). The
/// scaffold's fallback ships *all* owned positions, which is correct but
/// over-communicates.
pub fn run_superstep<E: crate::engines::LayoutEngine, T: HaloTransport>(
    frame: u64,
    worker: &mut Worker<E>,
    ctx: &mut crate::engines::EngineCtx,
    transport: &mut T,
) -> Vec<f32> {
    // a/b — local forces + integrate.
    let out = worker.engine.step(ctx);

    // c — publish boundary positions for peers.
    let outgoing = match out.boundary {
        Some(ref halo) => HaloDelta::from_halo(halo),
        None => {
            // Fallback: ship all owned positions tagged with their global ids.
            HaloDelta {
                frame,
                owner_id: worker.partition.partition_id,
                node_ids: worker.partition.owned_global_ids().to_vec(),
                positions: out.positions.clone(),
            }
        }
    };
    transport.publish(frame, outgoing);

    // d — barrier: collect peers' halos and apply them as ghost updates.
    for peer in transport.collect(frame) {
        worker.engine.apply_halo(&peer.into_halo());
    }

    out.positions
}

/// Single-process demonstration of the full BSP loop: drive `n_supersteps`
/// across all workers using a [`LocalTransport`] mailbox, returning each
/// worker's final owned positions.
///
/// This exists to prove the superstep *shape* end-to-end without a network. A
/// real deployment runs one [`run_superstep`] per worker *process*, each with
/// its own [`HaloTransport`] over gRPC, synchronized by the transport's barrier.
///
/// `TODO(barrier-ordering):` this drives publish-then-collect per worker
/// sequentially, which the in-memory mailbox tolerates; a real concurrent
/// barrier must publish ALL workers for `frame` before ANY worker collects.
pub fn run_superstep_local<E: crate::engines::LayoutEngine>(
    workers: &mut [Worker<E>],
    ctxs: &mut [crate::engines::EngineCtx],
    n_supersteps: u64,
) -> Vec<Vec<f32>> {
    assert_eq!(workers.len(), ctxs.len(), "one ctx per worker");
    let mut mailbox: std::collections::HashMap<u64, Vec<HaloDelta>> =
        std::collections::HashMap::new();
    let mut last: Vec<Vec<f32>> = vec![Vec::new(); workers.len()];

    for frame in 0..n_supersteps {
        // Phase 1: every worker steps + publishes (the BSP compute + send).
        for (w, ctx) in workers.iter_mut().zip(ctxs.iter_mut()) {
            let out = w.engine.step(ctx);
            let delta = match out.boundary {
                Some(ref halo) => HaloDelta::from_halo(halo),
                None => HaloDelta {
                    frame,
                    owner_id: w.partition.partition_id,
                    node_ids: w.partition.owned_global_ids().to_vec(),
                    positions: out.positions.clone(),
                },
            };
            mailbox.entry(frame).or_default().push(delta);
            last[w.partition.partition_id as usize] = out.positions;
        }
        // Phase 2 (barrier): every worker applies peers' halos.
        let deltas = mailbox.remove(&frame).unwrap_or_default();
        for w in workers.iter_mut() {
            for d in &deltas {
                if d.owner_id != w.partition.partition_id {
                    w.engine.apply_halo(&d.clone().into_halo());
                }
            }
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 0—1—2—3—4—5 path split into 2 partitions should produce contiguous
    /// owned blocks with exactly one ghost each at the cut.
    #[test]
    fn partition_path_two_ways() {
        let g = CsrGraph::path(6);
        let parts = partition_csr(&g, 2);
        assert_eq!(parts.len(), 2);

        // Every global node owned exactly once.
        let mut owned_all: Vec<u32> = parts
            .iter()
            .flat_map(|p| p.owned_global_ids().iter().copied())
            .collect();
        owned_all.sort_unstable();
        assert_eq!(owned_all, vec![0, 1, 2, 3, 4, 5]);

        // Each side of a path cut has at least one ghost (the neighbor across
        // the cut).
        for p in &parts {
            assert!(
                !p.ghost_global_ids().is_empty(),
                "partition {} should have a ghost at the path cut",
                p.partition_id
            );
            // Local CSR is self-contained: every neighbor index is in range.
            for &nb in &p.local.neighbors {
                assert!(nb < p.local.n_nodes, "ghost neighbor index out of range");
            }
            // owned + ghost == local node count.
            assert_eq!(
                p.n_owned as usize + p.ghost_global_ids().len(),
                p.local.n_nodes as usize
            );
        }
    }

    #[test]
    fn partition_meta_roundtrips_owned_ghost() {
        let g = CsrGraph::path(6);
        let parts = partition_csr(&g, 2);
        for p in &parts {
            let m = p.meta();
            assert_eq!(m.partition_id, p.partition_id);
            assert_eq!(m.owned_node_ids, p.owned_global_ids());
            assert_eq!(m.ghost_node_ids, p.ghost_global_ids());
        }
    }

    #[test]
    fn empty_graph_partitions_cleanly() {
        let g = CsrGraph {
            n_nodes: 0,
            offsets: vec![0],
            neighbors: vec![],
        };
        let parts = partition_csr(&g, 3);
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert_eq!(p.n_owned, 0);
            assert_eq!(p.local.n_nodes, 0);
        }
    }

    #[test]
    fn halo_delta_bytes_roundtrip() {
        let d = HaloDelta {
            frame: 7,
            owner_id: 2,
            node_ids: vec![10, 20, 30],
            positions: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        };
        let (ids_le, pos_le) = d.encode_bytes();
        let back = HaloDelta::decode_bytes(d.frame, d.owner_id, &ids_le, &pos_le).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn halo_delta_rejects_mismatched_lengths() {
        // positions has 2 floats but 1 node => expects 3.
        let pos_le: Vec<u8> = bytemuck::cast_slice(&[1.0f32, 2.0f32]).to_vec();
        let ids_le: Vec<u8> = bytemuck::cast_slice(&[5u32]).to_vec();
        assert!(HaloDelta::decode_bytes(0, 0, &ids_le, &pos_le).is_err());
    }

    #[test]
    fn halo_delta_round_trips_through_halo_update() {
        let d = HaloDelta {
            frame: 3,
            owner_id: 1,
            node_ids: vec![1, 2],
            positions: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        };
        let h = d.clone().into_halo();
        assert_eq!(HaloDelta::from_halo(&h), d);
    }

    /// Drive the BSP loop with a trivial test engine over a partitioned path to
    /// prove the superstep shape compiles + runs end-to-end (no GPU).
    #[test]
    fn bsp_superstep_local_runs() {
        use crate::engines::{CsrShard, EngineCtx, LayoutEngine, StepOutput};
        use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};

        /// Counts steps + records the last halo it received, so the test can
        /// assert exchange actually happened.
        struct Probe {
            descriptor: LayoutDescriptor,
            owned: Vec<u32>,
            steps: u64,
            halos_received: u64,
        }
        impl LayoutEngine for Probe {
            fn descriptor(&self) -> &LayoutDescriptor {
                &self.descriptor
            }
            fn init(
                &mut self,
                _ctx: &mut EngineCtx,
                _g: &CsrShard,
                _p: &[f32],
            ) -> Result<(), String> {
                Ok(())
            }
            fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
                self.steps += 1;
                // owned positions: one xyz per owned node.
                StepOutput::positions_only(vec![0.0; 3 * self.owned.len()])
            }
            fn apply_halo(&mut self, _h: &crate::engines::HaloUpdate) {
                self.halos_received += 1;
            }
        }

        let g = CsrGraph::path(6);
        let parts = partition_csr(&g, 2);
        let descriptor = LayoutDescriptor {
            id: "probe",
            kind: LayoutKind::Physics,
            display_name: "probe",
            description: "test",
            requirements: LayoutRequirements::default(),
        };

        let mut workers: Vec<Worker<Probe>> = parts
            .iter()
            .map(|p| Worker {
                partition: p.clone(),
                engine: Probe {
                    descriptor: descriptor.clone(),
                    owned: p.owned_global_ids().to_vec(),
                    steps: 0,
                    halos_received: 0,
                },
            })
            .collect();
        let mut ctxs: Vec<EngineCtx> = (0..workers.len()).map(|_| EngineCtx::cpu_only()).collect();

        let n_supersteps = 3;
        let _ = run_superstep_local(&mut workers, &mut ctxs, n_supersteps);

        for w in &workers {
            assert_eq!(w.engine.steps, n_supersteps);
            // With 2 partitions each receives 1 peer halo per superstep.
            assert_eq!(w.engine.halos_received, n_supersteps);
        }
    }
}
