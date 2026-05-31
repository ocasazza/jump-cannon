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

use crate::engines::{GraphAttributes, HaloUpdate, ShardMeta};
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
    /// Local-index attributes, parallel to `local`.
    pub attributes: Option<GraphAttributes>,
    /// `global_ids[local_idx] -> global node id`. Length `local.n_nodes`.
    pub global_ids: Vec<u32>,
    /// Number of owned (integrated) nodes; they occupy local indices
    /// `[0, n_owned)`.
    pub n_owned: u32,
    /// **Boundary** global ids: the subset of owned nodes that appear in *some
    /// other* partition's ghost table — i.e. the only owned positions a peer
    /// actually needs each superstep (doc §4: "exchange only boundary
    /// positions"). Interior owned nodes (referenced by no peer) are omitted, so
    /// the BSP halo ships strictly less than every owned position.
    ///
    /// Stored in ascending global order. By the edge-cut symmetry of
    /// [`partition_csr`], `v` is in this partition's boundary iff `v` is owned
    /// here and is some peer's ghost, which holds exactly when `v` has a neighbor
    /// owned by another partition.
    pub boundary: Vec<u32>,
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

    /// Global ids of this partition's boundary nodes (owned here, ghost on some
    /// peer). See [`Partition::boundary`].
    pub fn boundary_global_ids(&self) -> &[u32] {
        &self.boundary
    }

    /// Build the outgoing [`HaloDelta`] for this superstep from the engine's
    /// owned-position slice, shipping ONLY the boundary nodes (doc §4).
    ///
    /// `owned_positions` is interleaved `x,y,z`, parallel to
    /// [`Partition::owned_global_ids`] (so `owned_positions.len() == 3 *
    /// n_owned`). Each boundary global id is mapped to its slot in the owned
    /// block via the ascending owned order, and only that node's `x,y,z` triple
    /// is copied into the delta. Interior owned positions are never sent.
    pub(crate) fn boundary_delta(&self, frame: u64, owned_positions: &[f32]) -> HaloDelta {
        let owned = self.owned_global_ids();
        let mut node_ids = Vec::with_capacity(self.boundary.len());
        let mut positions = Vec::with_capacity(3 * self.boundary.len());

        let mut boundary_attributes = self.attributes.as_ref().map(|_| GraphAttributes::default());

        for &g in &self.boundary {
            // `owned` is ascending; boundary ids are a subset of it.
            if let Ok(idx) = owned.binary_search(&g) {
                let base = 3 * idx;
                if base + 2 < owned_positions.len() {
                    node_ids.push(g);
                    positions.push(owned_positions[base]);
                    positions.push(owned_positions[base + 1]);
                    positions.push(owned_positions[base + 2]);

                    // Slice attributes for this boundary node
                    if let (Some(la), Some(ba)) = (self.attributes.as_ref(), boundary_attributes.as_mut()) {
                        if let Some(v) = &la.node_class {
                            ba.node_class.get_or_insert_with(Vec::new).push(v[idx]);
                        }
                        if let Some(v) = &la.node_coordination {
                            ba.node_coordination.get_or_insert_with(Vec::new).push(v[idx]);
                        }
                        if let Some(v) = &la.node_mass {
                            ba.node_mass.get_or_insert_with(Vec::new).push(v[idx]);
                        }
                    }
                }
            }
        }
        HaloDelta {
            frame,
            owner_id: self.partition_id,
            node_ids,
            positions,
            attributes: boundary_attributes,
        }
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
///
/// For a cheap, drop-in *improvement* over the raw BFS cut without a heavyweight
/// library, see [`partition_csr_refined`] / [`refine_partitions`], which run a
/// Kernighan–Lin / Fiduccia–Mattheyses boundary-refinement pass on top of this.
pub fn partition_csr(
    graph: &CsrGraph,
    attributes: Option<&GraphAttributes>,
    n_partitions: u32,
) -> Vec<Partition> {
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
                attributes: None,
                global_ids: Vec::new(),
                n_owned: 0,
                boundary: Vec::new(),
            })
            .collect();
    }

    let owner = assign_owners_bfs(graph, p);
    build_partitions_from_owner(graph, attributes, &owner, p)
}

/// Materialize the full `Vec<Partition>` (local CSR + ghost table + boundary
/// set) from an owner assignment. Shared by [`partition_csr`] and the
/// FM-refinement path so the boundary/ghost derivation has exactly one source of
/// truth (the task requires boundary + ghost tables to be **recomputed** after
/// refinement — this is that recomputation).
fn build_partitions_from_owner(
    graph: &CsrGraph,
    attributes: Option<&GraphAttributes>,
    owner: &[u32],
    p: u32,
) -> Vec<Partition> {
    // Bucket owned global ids per partition (ascending global order — keeps the
    // local index ↔ global id mapping deterministic).
    let mut owned: Vec<Vec<u32>> = vec![Vec::new(); p as usize];
    for (g, &o) in owner.iter().enumerate() {
        owned[o as usize].push(g as u32);
    }

    owned
        .into_iter()
        .enumerate()
        .map(|(pid, owned_ids)| build_partition(graph, attributes, owner, pid as u32, p, owned_ids))
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
    attributes: Option<&GraphAttributes>,
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
    //
    // In the same pass derive the BOUNDARY set: an owned node is a boundary node
    // iff it has a neighbor owned elsewhere. By edge-cut symmetry that owned
    // node is then a ghost on the partition owning that neighbor, so this is
    // exactly { owned v : v is some peer's ghost } (asserted independently in
    // `boundary_equals_peers_ghosts`). `owned_ids` is ascending, so pushing in
    // iteration order keeps `boundary` ascending for `boundary_delta`'s
    // binary_search.
    let mut ghost_ids: Vec<u32> = Vec::new();
    let mut boundary: Vec<u32> = Vec::new();
    for &g in &owned_ids {
        let gv = g as usize;
        let start = graph.offsets[gv] as usize;
        let end = graph.offsets[gv + 1] as usize;
        let mut is_boundary = false;
        for &u in &graph.neighbors[start..end] {
            if owner[u as usize] != pid {
                is_boundary = true;
                if !global_to_local.contains_key(&u) {
                    let li = (n_owned as usize + ghost_ids.len()) as u32;
                    global_to_local.insert(u, li);
                    ghost_ids.push(u);
                }
            }
        }
        if is_boundary {
            boundary.push(g);
        }
    }
    global_ids.extend_from_slice(&ghost_ids);

    let n_local = global_ids.len();

    // Slice attributes if present. Local attributes are parallel to `global_ids`.
    let local_attributes = attributes.map(|ga| {
        let mut la = GraphAttributes::default();
        if let Some(v) = &ga.node_class {
            la.node_class = Some(global_ids.iter().map(|&g| v[g as usize]).collect());
        }
        if let Some(v) = &ga.node_coordination {
            la.node_coordination = Some(global_ids.iter().map(|&g| v[g as usize]).collect());
        }
        if let Some(v) = &ga.node_mass {
            la.node_mass = Some(global_ids.iter().map(|&g| v[g as usize]).collect());
        }
        // edge_len is parallel to graph.neighbors. We must re-build it for the local neighbors.
        if let Some(v) = &ga.edge_len {
            let mut local_edge_len = Vec::new();
            for li in 0..n_local {
                if li < n_owned as usize {
                    let g = global_ids[li] as usize;
                    let start = graph.offsets[g] as usize;
                    let end = graph.offsets[g + 1] as usize;
                    for &u in &graph.neighbors[start..end] {
                        if global_to_local.contains_key(&u) {
                            // Find the original edge index in the global neighbors list
                            // This is slightly inefficient but correct.
                            // Better would be to track it during neighbor iteration.
                            let global_edge_idx = start + graph.neighbors[start..end].iter().position(|&nb| nb == u).unwrap();
                            local_edge_len.push(v[global_edge_idx]);
                        }
                    }
                }
            }
            la.edge_len = Some(local_edge_len);
        }
        la
    });

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
        attributes: local_attributes,
        global_ids,
        n_owned,
        boundary,
    }
}

// ---------------------------------------------------------------------------
// Partition quality: edge-cut metric + KL/FM boundary refinement
// ---------------------------------------------------------------------------

/// Number of **cut edges**: undirected edges whose two endpoints are owned by
/// different partitions. This is the standard graph-partitioning objective
/// (Kernighan & Lin, "An Efficient Heuristic Procedure for Partitioning
/// Graphs", *Bell System Technical Journal* 49(2), 1970) and directly bounds the
/// distributed halo traffic (doc §4: each cut edge becomes one owned↔ghost
/// reference refreshed every superstep).
///
/// The graph's CSR is symmetric (an undirected edge `{u,v}` appears as both
/// `u→v` and `v→u`, cf. [`CsrGraph::path`]), so each cut edge is seen twice
/// during a full adjacency scan; we count only the `u < v` orientation to return
/// the count of *undirected* cut edges. Self-loops (`u == v`) are never cut and
/// are ignored.
pub fn edge_cut(parts: &[Partition], graph: &CsrGraph) -> usize {
    let owner = owner_from_partitions(parts, graph.n_nodes as usize);
    edge_cut_owner(graph, &owner)
}

/// Cut count straight from an owner array (the form refinement mutates).
fn edge_cut_owner(graph: &CsrGraph, owner: &[u32]) -> usize {
    let mut cut = 0usize;
    let n = graph.n_nodes as usize;
    for u in 0..n {
        let start = graph.offsets[u] as usize;
        let end = graph.offsets[u + 1] as usize;
        for &v in &graph.neighbors[start..end] {
            let v = v as usize;
            // Count each undirected edge once; skip self-loops.
            if u < v && owner[u] != owner[v] {
                cut += 1;
            }
        }
    }
    cut
}

/// Recover `owner[global_id] -> partition_id` from a built partition set. Defined
/// for every global id (nodes appear as the owned block of exactly one
/// partition, by [`partition_csr`]'s construction).
fn owner_from_partitions(parts: &[Partition], n: usize) -> Vec<u32> {
    let mut owner = vec![0u32; n];
    for p in parts {
        for &g in p.owned_global_ids() {
            owner[g as usize] = p.partition_id;
        }
    }
    owner
}

/// Balance tolerance for refinement: a partition may grow to at most
/// `ceil((1 + tol) * n / P)` owned nodes. 0.10 ⇒ ±10% of the ideal `n/P`.
const REFINE_BALANCE_TOL: f64 = 0.10;

/// Cap on refinement passes. Each pass is a full single-node FM sweep; the cut
/// is monotone non-increasing across passes and converges quickly, so a small
/// bound suffices and keeps the work deterministic + cheap.
const REFINE_MAX_PASSES: usize = 8;

/// Partition `graph` into `n_partitions` blocks **with** a Kernighan–Lin /
/// Fiduccia–Mattheyses boundary-refinement pass applied on top of the v1 BFS cut
/// ([`partition_csr`]). Opt-in: callers that want the cheap raw BFS cut keep
/// using [`partition_csr`]; this is the lower-edge-cut variant.
///
/// The returned partitions are fully rebuilt from the refined owner assignment,
/// so every `Partition`'s `local` CSR, ghost table, and `boundary` set reflect
/// the post-refinement ownership (the boundary/ghost invariants asserted in the
/// tests still hold).
pub fn partition_csr_refined(graph: &CsrGraph, n_partitions: u32) -> Vec<Partition> {
    // Attribute sharding across refined partitions is Phase F (see
    // docs/geometric-engine-plan.md §8); the FM-refinement path carries no
    // injected attributes yet.
    let parts = partition_csr(graph, None, n_partitions);
    refine_partitions(graph, parts)
}

/// Kernighan–Lin / Fiduccia–Mattheyses boundary refinement.
///
/// Given an existing partition (e.g. the BFS-region cut from [`partition_csr`]),
/// reduce the [`edge_cut`] by repeatedly **moving boundary nodes to a neighboring
/// partition** when the move lowers the cut and keeps sizes within
/// [`REFINE_BALANCE_TOL`] of the ideal `n/P`. This is the single-node-move,
/// reduced-neighborhood form of FM (Fiduccia & Mattheyses, "A Linear-Time
/// Heuristic for Improving Network Partitions", *19th Design Automation
/// Conference*, 1982), which generalizes the pairwise-swap KL procedure (Kernighan
/// & Lin, 1970) to k-way partitions:
///
///   * **Gain.** Moving node `v` from its partition `a` to partition `b` changes
///     the cut by `gain = external_b(v) - internal(v)`, where `internal(v)` is
///     the number of `v`'s neighbors owned by `a` and `external_b(v)` is the
///     number owned by `b`. A positive gain strictly lowers the cut.
///   * **Reduced neighborhood.** Only *boundary* nodes (some neighbor owned
///     elsewhere) can have positive gain, so each pass scans the boundary only —
///     the linear-time idea behind FM.
///   * **Balance.** A move is rejected if it would push the destination above the
///     size cap or drain the source below the matching floor, keeping the cut
///     reduction balanced (the KL/FM balance constraint).
///
/// **Determinism.** No RNG. Nodes are scanned in ascending global id; among
/// candidate destinations the one with the largest gain wins, ties broken by the
/// smallest destination partition id. Passes repeat until a pass makes no move or
/// [`REFINE_MAX_PASSES`] is hit; the cut is monotone non-increasing (only
/// strictly-positive-gain moves are applied), so the result never exceeds the
/// input cut.
///
/// The input `parts` are consumed only to recover the owner assignment; the
/// returned partitions are freshly built from the refined owners, so boundary +
/// ghost tables are recomputed correctly.
pub fn refine_partitions(graph: &CsrGraph, parts: Vec<Partition>) -> Vec<Partition> {
    let n = graph.n_nodes as usize;
    let p = if let Some(first) = parts.first() {
        first.n_partitions
    } else {
        return parts;
    };
    if n == 0 || p <= 1 {
        return parts;
    }

    let mut owner = owner_from_partitions(&parts, n);

    // Per-partition owned-node counts, kept in sync as nodes move.
    let mut sizes = vec![0usize; p as usize];
    for &o in &owner {
        sizes[o as usize] += 1;
    }

    // Balance window around the ideal n/P (KL/FM balance constraint).
    let ideal = n as f64 / p as f64;
    let max_size = ((1.0 + REFINE_BALANCE_TOL) * ideal).ceil() as usize;
    // Floor mirrors the cap so draining a partition can't unbalance the other side.
    let min_size = ((1.0 - REFINE_BALANCE_TOL) * ideal).floor() as usize;

    for _pass in 0..REFINE_MAX_PASSES {
        let mut moved = false;

        // Scan candidate movers in ascending global id (deterministic order).
        for v in 0..n {
            let a = owner[v];
            let start = graph.offsets[v] as usize;
            let end = graph.offsets[v + 1] as usize;

            // Tally neighbor counts per partition: internal (own a) vs external.
            let mut internal = 0usize;
            let mut ext = vec![0usize; p as usize];
            for &u in &graph.neighbors[start..end] {
                let u = u as usize;
                if u == v {
                    continue; // ignore self-loops
                }
                let o = owner[u];
                if o == a {
                    internal += 1;
                } else {
                    ext[o as usize] += 1;
                }
            }

            // Interior node (no external neighbor) can never improve the cut.
            // Best destination = max external degree; gain = ext_b - internal.
            // Tie-break by smallest partition id for determinism.
            let mut best_dest: Option<u32> = None;
            let mut best_ext = 0usize;
            for (b, &e) in ext.iter().enumerate() {
                if b as u32 == a || e == 0 {
                    continue;
                }
                if e > best_ext {
                    best_ext = e;
                    best_dest = Some(b as u32);
                }
            }
            let Some(b) = best_dest else { continue };

            let gain = best_ext as isize - internal as isize;
            if gain <= 0 {
                continue; // only strictly-cut-reducing moves (monotone)
            }

            // Balance: don't overfill the destination or drain the source.
            if sizes[b as usize] + 1 > max_size {
                continue;
            }
            if sizes[a as usize].saturating_sub(1) < min_size {
                continue;
            }

            // Apply the move.
            owner[v] = b;
            sizes[a as usize] -= 1;
            sizes[b as usize] += 1;
            moved = true;
        }

        if !moved {
            break; // converged: no positive-gain, balance-feasible move remains
        }
    }

    // Refinement path carries no injected attributes yet (Phase F).
    build_partitions_from_owner(graph, None, &owner, p)
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
    /// Optional attributes for the boundary nodes.
    pub attributes: Option<GraphAttributes>,
}

impl HaloDelta {
    /// Build from the foundation's [`HaloUpdate`] (same fields, typed).
    pub fn from_halo(h: &HaloUpdate) -> Self {
        Self {
            frame: h.frame,
            owner_id: h.owner_id,
            node_ids: h.node_ids.clone(),
            positions: h.positions.clone(),
            attributes: h.attributes.clone(),
        }
    }

    /// Convert into a foundation [`HaloUpdate`] for [`LayoutEngine::apply_halo`].
    pub fn into_halo(self) -> HaloUpdate {
        HaloUpdate {
            frame: self.frame,
            owner_id: self.owner_id,
            node_ids: self.node_ids,
            positions: self.positions,
            attributes: self.attributes,
        }
    }

    /// Encode the bulk arrays and optional attributes to proto wire form.
    pub fn encode_proto(&self) -> crate::proto::HaloDelta {
        let (node_ids, positions) = self.encode_bytes();
        let attributes = self.attributes.as_ref().map(|ga| {
            crate::proto::GraphAttributes {
                node_class: ga.node_class.as_ref().map(|v| bytemuck::cast_slice::<u32, u8>(v).to_vec()).unwrap_or_default(),
                node_coordination: ga.node_coordination.as_ref().map(|v| bytemuck::cast_slice::<u32, u8>(v).to_vec()).unwrap_or_default(),
                node_mass: ga.node_mass.as_ref().map(|v| bytemuck::cast_slice::<f32, u8>(v).to_vec()).unwrap_or_default(),
                edge_len: ga.edge_len.as_ref().map(|v| bytemuck::cast_slice::<f32, u8>(v).to_vec()).unwrap_or_default(),
            }
        });
        crate::proto::HaloDelta {
            frame: self.frame,
            owner_id: self.owner_id,
            node_ids,
            positions,
            attributes,
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
        attributes: Option<crate::proto::GraphAttributes>,
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

        let host_attributes = attributes.map(|pa| {
            let mut ga = GraphAttributes::default();
            if !pa.node_class.is_empty() {
                ga.node_class = Some(bytemuck::cast_slice::<u8, u32>(&pa.node_class).to_vec());
            }
            if !pa.node_coordination.is_empty() {
                ga.node_coordination = Some(bytemuck::cast_slice::<u8, u32>(&pa.node_coordination).to_vec());
            }
            if !pa.node_mass.is_empty() {
                ga.node_mass = Some(bytemuck::cast_slice::<u8, f32>(&pa.node_mass).to_vec());
            }
            if !pa.edge_len.is_empty() {
                ga.edge_len = Some(bytemuck::cast_slice::<u8, f32>(&pa.edge_len).to_vec());
            }
            ga
        });

        Ok(Self {
            frame,
            owner_id,
            node_ids,
            positions,
            attributes: host_attributes,
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
// gRPC halo transport (real cross-process exchange — doc §4 step c)
// ---------------------------------------------------------------------------

/// A real [`HaloTransport`] that exchanges boundary positions with a peer
/// worker over the `Compute::ExchangeHalo` bidirectional stream
/// (docs/compute-architecture.md §4). This is the production counterpart to the
/// in-process [`LocalTransport`] double: every published delta is encoded to the
/// proto wire form, streamed to the peer's `ExchangeHalo` handler, and the
/// peer's reply deltas are decoded back into [`HaloDelta`]s for `collect`.
///
/// ## Sync trait over an async stream
///
/// The [`HaloTransport`] trait is synchronous (the BSP loop in
/// [`run_superstep`] is plain blocking code), but a tonic client is async. We
/// bridge by spawning a driver task on a supplied [`tokio::runtime::Handle`]
/// that owns the bidi stream: `publish` pushes onto a request `mpsc` (the driver
/// forwards it to the peer), and `collect` blocks the calling thread on a
/// response `mpsc` until the peer's deltas for the requested frame arrive,
/// buffering any out-of-order frames. This keeps `run_superstep` transport
/// agnostic — it never sees the runtime.
///
/// `TODO(barrier-policy):` `collect` currently blocks indefinitely for the
/// frame's reply. A production deployment wants a timeout + peer-failure policy
/// (doc §4's "barrier with a failure policy").
pub struct TonicHaloTransport {
    self_id: u32,
    /// Outbound proto deltas → the driver task → the peer's stream.
    tx: tokio::sync::mpsc::UnboundedSender<crate::proto::HaloDelta>,
    /// Inbound peer deltas, decoded back to host form by the driver task.
    rx: tokio::sync::mpsc::UnboundedReceiver<HaloDelta>,
    /// Frames received from the peer but not yet handed out by `collect`
    /// (out-of-order buffering across the BSP barrier).
    pending: std::collections::HashMap<u64, Vec<HaloDelta>>,
}

impl TonicHaloTransport {
    /// Connect this worker (`self_id`) to a peer's `ExchangeHalo` stream over an
    /// existing tonic [`Channel`](tonic::transport::Channel) (or any compatible
    /// gRPC channel), driving the stream on `handle`.
    ///
    /// The bidi stream is opened immediately; published deltas flow to the peer
    /// and the peer's replies are pumped into the inbound queue by a background
    /// task. `collect(frame)` blocks the BSP thread until that frame's replies
    /// land.
    pub fn connect<T>(
        self_id: u32,
        mut client: crate::proto::compute_client::ComputeClient<T>,
        handle: tokio::runtime::Handle,
    ) -> Result<Self, String>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static,
        T::Error: Into<tonic::codegen::StdError>,
        T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
        <T::ResponseBody as tonic::codegen::Body>::Error: Into<tonic::codegen::StdError> + Send,
        T::Future: Send,
    {
        let (out_tx, out_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::proto::HaloDelta>();
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<HaloDelta>();

        // Driver task: own the bidi stream. Forward outbound deltas to the peer
        // and decode the peer's replies into the inbound queue. Decode errors
        // and stream end simply close the inbound channel (collect then drains
        // whatever was buffered and returns empty for missing frames).
        handle.spawn(async move {
            let req_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(out_rx);
            let mut resp = match client.exchange_halo(req_stream).await {
                Ok(r) => r.into_inner(),
                Err(e) => {
                    tracing::warn!(error = %e, "ExchangeHalo open failed");
                    return;
                }
            };
            loop {
                match resp.message().await {
                    Ok(Some(proto)) => {
                        match HaloDelta::decode_bytes(
                            proto.frame,
                            proto.owner_id,
                            &proto.node_ids,
                            &proto.positions,
                            proto.attributes,
                        ) {
                            Ok(d) => {
                                if in_tx.send(d).is_err() {
                                    break; // receiver (transport) dropped
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "dropping malformed peer HaloDelta");
                            }
                        }
                    }
                    Ok(None) => break, // peer closed the stream
                    Err(e) => {
                        tracing::warn!(error = %e, "ExchangeHalo recv error");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            self_id,
            tx: out_tx,
            rx: in_rx,
            pending: std::collections::HashMap::new(),
        })
    }

    /// This transport's partition id.
    pub fn self_id(&self) -> u32 {
        self.self_id
    }
}

impl HaloTransport for TonicHaloTransport {
    fn publish(&mut self, frame: u64, mut delta: HaloDelta) {
        // Stamp the frame so the peer keys its reply to the same superstep.
        delta.frame = frame;
        let proto = delta.encode_proto();
        // Best-effort: a closed channel means the peer/driver is gone; the BSP
        // barrier will then collect nothing for this frame.
        let _ = self.tx.send(proto);
    }

    fn collect(&mut self, frame: u64) -> Vec<HaloDelta> {
        // Hand out anything already buffered for this frame first.
        if let Some(v) = self.pending.remove(&frame) {
            if !v.is_empty() {
                return v.into_iter().filter(|d| d.owner_id != self.self_id).collect();
            }
        }
        // Block the BSP thread until the peer's reply(ies) for `frame` arrive,
        // buffering any other frames we see in the meantime. Returns empty once
        // the inbound channel closes (peer gone / stream ended).
        loop {
            match self.rx.blocking_recv() {
                Some(d) if d.frame == frame => {
                    let mut out = vec![d];
                    // Greedily drain any further replies already queued for this
                    // frame without blocking.
                    while let Ok(extra) = self.rx.try_recv() {
                        if extra.frame == frame {
                            out.push(extra);
                        } else {
                            self.pending.entry(extra.frame).or_default().push(extra);
                        }
                    }
                    return out
                        .into_iter()
                        .filter(|d| d.owner_id != self.self_id)
                        .collect();
                }
                Some(other) => {
                    self.pending.entry(other.frame).or_default().push(other);
                }
                None => return Vec::new(), // stream closed
            }
        }
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
/// owned positions + the partition's precomputed boundary set
/// ([`Partition::boundary`]). The fallback ships ONLY the boundary nodes — the
/// owned nodes that are some peer's ghost — not every owned position, matching
/// doc §4's "exchange only boundary positions". A real engine may still emit
/// `StepOutput.boundary` itself; `TODO(boundary-derivation)` if an engine wants
/// to override the partition-derived set.
pub fn run_superstep<E: crate::engines::LayoutEngine, T: HaloTransport>(
    frame: u64,
    worker: &mut Worker<E>,
    ctx: &mut crate::engines::EngineCtx,
    transport: &mut T,
) -> Vec<f32> {
    // a/b — local forces + integrate.
    let out = worker.engine.step(ctx);

    // c — publish boundary positions for peers. Fallback ships ONLY the
    // boundary subset of owned positions (doc §4), not every owned node.
    let outgoing = match out.boundary {
        Some(ref halo) => HaloDelta::from_halo(halo),
        None => worker.partition.boundary_delta(frame, &out.positions),
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
            // Fallback ships ONLY the boundary subset (doc §4), not all owned.
            let delta = match out.boundary {
                Some(ref halo) => HaloDelta::from_halo(halo),
                None => w.partition.boundary_delta(frame, &out.positions),
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
        let parts = partition_csr(&g, None, 2);
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
        let parts = partition_csr(&g, None, 2);
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
        let parts = partition_csr(&g, None, 3);
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
            attributes: None,
        };
        let (ids_le, pos_le) = d.encode_bytes();
        let back = HaloDelta::decode_bytes(d.frame, d.owner_id, &ids_le, &pos_le, None).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn halo_delta_rejects_mismatched_lengths() {
        // positions has 2 floats but 1 node => expects 3.
        let pos_le: Vec<u8> = bytemuck::cast_slice(&[1.0f32, 2.0f32]).to_vec();
        let ids_le: Vec<u8> = bytemuck::cast_slice(&[5u32]).to_vec();
        assert!(HaloDelta::decode_bytes(0, 0, &ids_le, &pos_le, None).is_err());
    }

    #[test]
    fn halo_delta_round_trips_through_halo_update() {
        let d = HaloDelta {
            frame: 3,
            owner_id: 1,
            node_ids: vec![1, 2],
            positions: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            attributes: None,
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
        let parts = partition_csr(&g, None, 2);
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

    /// (a) The precomputed boundary set equals EXACTLY the owned nodes that are
    /// some peer's ghost — computed here independently from the per-partition
    /// ghost lists, never from `Partition::boundary`.
    #[test]
    fn boundary_equals_peers_ghosts() {
        use std::collections::HashSet;

        // Undirected ring 0—1—…—(n-1)—0: more than one cut per multi-way split,
        // exercising partitions with >1 boundary node (no `grid` ctor exists).
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

        // A few graphs to exercise the property over different cut shapes.
        for g in [CsrGraph::path(6), CsrGraph::path(11), ring(8)] {
            for np in [2u32, 3, 4] {
                let parts = partition_csr(&g, None, np);

                // Independent ground truth: a global id is "wanted" if it shows
                // up in ANY partition's ghost list. For partition p, the boundary
                // is { v owned by p : v is wanted } — i.e. v is some peer's ghost.
                // (A ghost in partition q is by construction owned by some p != q.)
                let mut all_ghosts: HashSet<u32> = HashSet::new();
                for p in &parts {
                    for &gid in p.ghost_global_ids() {
                        all_ghosts.insert(gid);
                    }
                }

                for p in &parts {
                    let expected: HashSet<u32> = p
                        .owned_global_ids()
                        .iter()
                        .copied()
                        .filter(|v| all_ghosts.contains(v))
                        .collect();
                    let got: HashSet<u32> = p.boundary_global_ids().iter().copied().collect();
                    assert_eq!(
                        got, expected,
                        "graph n={} np={} part {}: boundary != owned-that-are-peer-ghosts",
                        g.n_nodes, np, p.partition_id
                    );

                    // Boundary is a subset of owned, kept in ascending order.
                    let owned: HashSet<u32> = p.owned_global_ids().iter().copied().collect();
                    assert!(p.boundary.iter().all(|b| owned.contains(b)));
                    assert!(
                        p.boundary.windows(2).all(|w| w[0] < w[1]),
                        "boundary must be ascending for binary_search"
                    );
                }
            }
        }
    }

    /// (b) A superstep round on a small graph with interior nodes ships strictly
    /// FEWER node ids than the owned count: the boundary delta omits interior
    /// owned nodes.
    #[test]
    fn superstep_ships_fewer_than_owned_when_interior_exists() {
        // path(6) split in two => each side owns 3 nodes (0,1,2 | 3,4,5) with a
        // single boundary node at the cut (2 | 3). Interior nodes 0,1 / 4,5 must
        // NOT be shipped.
        let g = CsrGraph::path(6);
        let parts = partition_csr(&g, None, 2);

        // Sanity: at least one partition has interior (non-boundary) owned nodes.
        assert!(
            parts
                .iter()
                .any(|p| (p.boundary.len() as u32) < p.n_owned),
            "test graph must have interior owned nodes to be meaningful"
        );

        for p in &parts {
            // Fake owned positions: one distinct xyz per owned node.
            let owned = p.owned_global_ids();
            let mut pos = Vec::with_capacity(3 * owned.len());
            for (i, _) in owned.iter().enumerate() {
                pos.extend_from_slice(&[i as f32, i as f32 + 0.5, i as f32 + 0.25]);
            }
            let delta = p.boundary_delta(0, &pos);

            // Strictly fewer ids than owned (because interior nodes exist here).
            if (p.boundary.len() as u32) < p.n_owned {
                assert!(
                    (delta.node_ids.len() as u32) < p.n_owned,
                    "part {} shipped {} ids >= owned {}",
                    p.partition_id,
                    delta.node_ids.len(),
                    p.n_owned
                );
            }
            // Shipped ids are exactly the boundary set, and positions stay
            // parallel + correctly mapped from the owned slice.
            assert_eq!(delta.node_ids, p.boundary);
            assert_eq!(delta.positions.len(), 3 * delta.node_ids.len());
            for (k, &gid) in delta.node_ids.iter().enumerate() {
                let oi = owned.iter().position(|&o| o == gid).unwrap();
                assert_eq!(delta.positions[3 * k], pos[3 * oi]);
                assert_eq!(delta.positions[3 * k + 1], pos[3 * oi + 1]);
                assert_eq!(delta.positions[3 * k + 2], pos[3 * oi + 2]);
            }
        }
    }

    /// (c) Positions still propagate correctly across the boundary: an engine
    /// that integrates its owned nodes and folds in received halos must, after a
    /// superstep, hold the peer's boundary positions for exactly its ghost nodes
    /// — proving the boundary-only delta still carries everything peers need.
    #[test]
    fn boundary_only_halo_propagates_positions() {
        use crate::engines::{CsrShard, EngineCtx, HaloUpdate, LayoutEngine, StepOutput};
        use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
        use std::collections::HashMap;

        /// Sets each owned node's x to its global id (constant per step) and
        /// records, per received halo, the (global id -> x) it was handed. This
        /// lets the test confirm the boundary delta delivered real positions for
        /// the right nodes.
        struct PosProbe {
            descriptor: LayoutDescriptor,
            owned: Vec<u32>,
            // global id -> x received via apply_halo.
            received: HashMap<u32, f32>,
        }
        impl LayoutEngine for PosProbe {
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
                // x = global id, y = z = 0, in owned (ascending) order.
                let mut pos = Vec::with_capacity(3 * self.owned.len());
                for &g in &self.owned {
                    pos.extend_from_slice(&[g as f32, 0.0, 0.0]);
                }
                StepOutput::positions_only(pos)
            }
            fn apply_halo(&mut self, h: &HaloUpdate) {
                for (i, &gid) in h.node_ids.iter().enumerate() {
                    self.received.insert(gid, h.positions[3 * i]);
                }
            }
        }

        let g = CsrGraph::path(6);
        let parts = partition_csr(&g, None, 2);
        let descriptor = LayoutDescriptor {
            id: "posprobe",
            kind: LayoutKind::Physics,
            display_name: "posprobe",
            description: "test",
            requirements: LayoutRequirements::default(),
        };

        let mut workers: Vec<Worker<PosProbe>> = parts
            .iter()
            .map(|p| Worker {
                partition: p.clone(),
                engine: PosProbe {
                    descriptor: descriptor.clone(),
                    owned: p.owned_global_ids().to_vec(),
                    received: HashMap::new(),
                },
            })
            .collect();
        let mut ctxs: Vec<EngineCtx> = (0..workers.len()).map(|_| EngineCtx::cpu_only()).collect();

        run_superstep_local(&mut workers, &mut ctxs, 1);

        // Each worker must have received x == global id for EXACTLY its ghost
        // nodes (and nothing else): the boundary-only delta carried precisely
        // the peer-owned boundary positions this worker's ghosts mirror.
        for w in &workers {
            let ghosts: std::collections::HashSet<u32> =
                w.partition.ghost_global_ids().iter().copied().collect();
            let got: std::collections::HashSet<u32> =
                w.engine.received.keys().copied().collect();
            assert_eq!(
                got, ghosts,
                "part {} received halo ids != its ghost set",
                w.partition.partition_id
            );
            for (&gid, &x) in &w.engine.received {
                assert_eq!(x, gid as f32, "ghost {gid} position not propagated");
            }
            // And there is at least one ghost, so this actually exercised a hop.
            assert!(!ghosts.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // KL/FM edge-cut refinement
    // -----------------------------------------------------------------------

    /// Undirected ring 0—1—…—(n-1)—0 (mirrors the helper in
    /// `boundary_equals_peers_ghosts`).
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

    /// Build a symmetric (undirected) CSR from an edge list.
    fn undirected(n: u32, edges: &[(u32, u32)]) -> CsrGraph {
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
        for &(u, v) in edges {
            adj[u as usize].push(v);
            adj[v as usize].push(u);
        }
        let mut offsets = Vec::with_capacity((n + 1) as usize);
        let mut neighbors = Vec::new();
        for a in &mut adj {
            a.sort_unstable();
            offsets.push(neighbors.len() as u32);
            neighbors.extend_from_slice(a);
        }
        offsets.push(neighbors.len() as u32);
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// Independent edge-cut over an owner map derived straight from the built
    /// partitions, computed without touching `edge_cut`/`edge_cut_owner` (ground
    /// truth: a global id's owner is the partition whose owned block contains it).
    fn ground_truth_cut(parts: &[Partition], g: &CsrGraph) -> usize {
        use std::collections::HashMap;
        let mut owner: HashMap<u32, u32> = HashMap::new();
        for p in parts {
            for &gid in p.owned_global_ids() {
                owner.insert(gid, p.partition_id);
            }
        }
        let mut cut = 0usize;
        for u in 0..g.n_nodes {
            let s = g.offsets[u as usize] as usize;
            let e = g.offsets[u as usize + 1] as usize;
            for &v in &g.neighbors[s..e] {
                if u < v && owner[&u] != owner[&v] {
                    cut += 1;
                }
            }
        }
        cut
    }

    /// Reusable post-refine invariant check: every node owned once, local CSR
    /// self-contained, owned+ghost == local, and the boundary set equals exactly
    /// the owned nodes that are some peer's ghost (the `boundary_equals_peers_ghosts`
    /// property recomputed independently).
    fn assert_partition_invariants(parts: &[Partition], g: &CsrGraph) {
        use std::collections::HashSet;

        // Every global node owned exactly once.
        let mut owned_all: Vec<u32> = parts
            .iter()
            .flat_map(|p| p.owned_global_ids().iter().copied())
            .collect();
        owned_all.sort_unstable();
        let expect: Vec<u32> = (0..g.n_nodes).collect();
        assert_eq!(owned_all, expect, "every node owned exactly once");

        // Local CSR self-contained + owned/ghost accounting.
        for p in parts {
            for &nb in &p.local.neighbors {
                assert!(nb < p.local.n_nodes, "ghost neighbor index out of range");
            }
            assert_eq!(
                p.n_owned as usize + p.ghost_global_ids().len(),
                p.local.n_nodes as usize
            );
        }

        // boundary == owned-that-are-some-peer's-ghost, recomputed independently.
        let mut all_ghosts: HashSet<u32> = HashSet::new();
        for p in parts {
            for &gid in p.ghost_global_ids() {
                all_ghosts.insert(gid);
            }
        }
        for p in parts {
            let expected: HashSet<u32> = p
                .owned_global_ids()
                .iter()
                .copied()
                .filter(|v| all_ghosts.contains(v))
                .collect();
            let got: HashSet<u32> = p.boundary_global_ids().iter().copied().collect();
            assert_eq!(
                got, expected,
                "part {}: post-refine boundary != owned-that-are-peer-ghosts",
                p.partition_id
            );
            // Boundary stays ascending (binary_search precondition).
            assert!(
                p.boundary.windows(2).all(|w| w[0] < w[1]),
                "post-refine boundary must be ascending"
            );
        }
    }

    /// Owned-node counts per partition, indexed by partition id.
    fn sizes_of(parts: &[Partition]) -> Vec<usize> {
        let mut s = vec![0usize; parts.len()];
        for p in parts {
            s[p.partition_id as usize] = p.n_owned as usize;
        }
        s
    }

    /// `edge_cut` agrees with an independent owner-map count, and matches
    /// `ground_truth_cut`.
    #[test]
    fn edge_cut_matches_ground_truth() {
        for g in [CsrGraph::path(6), CsrGraph::path(11), ring(8)] {
            for np in [2u32, 3, 4] {
                let parts = partition_csr(&g, None, np);
                assert_eq!(
                    edge_cut(&parts, &g),
                    ground_truth_cut(&parts, &g),
                    "edge_cut disagrees with ground truth (n={}, np={})",
                    g.n_nodes,
                    np
                );
            }
        }
    }

    /// (a) Refinement never increases the cut on path(N) and a ring (cut is
    /// monotone non-increasing — only positive-gain moves are applied).
    #[test]
    fn refine_does_not_increase_cut() {
        for g in [CsrGraph::path(6), CsrGraph::path(12), ring(8), ring(12)] {
            for np in [2u32, 3, 4] {
                let before = partition_csr(&g, None, np);
                let cut_before = edge_cut(&before, &g);
                let after = refine_partitions(&g, before);
                let cut_after = edge_cut(&after, &g);
                assert!(
                    cut_after <= cut_before,
                    "refine increased cut on n={} np={}: {} -> {}",
                    g.n_nodes,
                    np,
                    cut_before,
                    cut_after
                );
            }
        }
    }

    /// (b) On a graph where BFS-region growth produces a suboptimal cut,
    /// refinement strictly reduces it.
    ///
    /// This 8-node graph is constructed so the balanced BFS regions strand node 7
    /// on the wrong side: BFS yields owner [0,0,1,0,0,1,1,1] (balanced 4|4, cut
    /// 3), but node 7's only same-partition neighbor is 6 while it has two
    /// neighbors (3, 4) owned by partition 0. FM moves 7 into partition 0,
    /// dropping the cut to 1 with a still-in-tolerance 5|3 split (ideal 4, ±10%
    /// ⇒ [3, 5]). (Found by exhaustive search over the BFS/FM behavior; values
    /// are pinned so a regression in either heuristic is caught.)
    #[test]
    fn refine_strictly_reduces_when_bfs_suboptimal() {
        let g = undirected(
            8,
            &[
                (0, 3),
                (0, 4),
                (1, 3),
                (2, 4),
                (2, 5),
                (3, 4),
                (3, 7),
                (4, 7),
                (5, 6),
            ],
        );

        let before = partition_csr(&g, None, 2);
        let cut_before = edge_cut(&before, &g);
        assert_eq!(cut_before, 3, "BFS cut precondition changed");

        let after = refine_partitions(&g, before);
        let cut_after = edge_cut(&after, &g);

        assert!(
            cut_after < cut_before,
            "FM failed to improve a BFS-suboptimal cut: {cut_before} -> {cut_after}"
        );
        assert_eq!(cut_after, 1, "expected FM to reach the optimal 1-edge cut");

        // Balance stays within ±10% of the ideal n/P (= 4): sizes in [3, 5].
        for &s in &sizes_of(&after) {
            assert!((3..=5).contains(&s), "post-refine size {s} out of [3,5]");
        }
        assert_partition_invariants(&after, &g);
    }

    /// (c) Refinement keeps partition sizes within the balance tolerance.
    #[test]
    fn refine_stays_balanced() {
        for g in [CsrGraph::path(12), ring(12), ring(16)] {
            for np in [2u32, 3, 4] {
                let n = g.n_nodes as usize;
                let after = refine_partitions(&g, partition_csr(&g, None, np));
                let ideal = n as f64 / np as f64;
                let max_size = ((1.0 + REFINE_BALANCE_TOL) * ideal).ceil() as usize;
                let min_size = ((1.0 - REFINE_BALANCE_TOL) * ideal).floor() as usize;
                for (pid, &s) in sizes_of(&after).iter().enumerate() {
                    assert!(
                        s <= max_size,
                        "part {pid} oversize: {s} > {max_size} (n={n}, np={np})"
                    );
                    assert!(
                        s >= min_size,
                        "part {pid} undersize: {s} < {min_size} (n={n}, np={np})"
                    );
                }
            }
        }
    }

    /// (d) All boundary/ghost invariants still hold after refinement (reuses the
    /// `boundary_equals_peers_ghosts` style independent recomputation).
    #[test]
    fn refine_preserves_boundary_ghost_invariants() {
        for g in [CsrGraph::path(6), CsrGraph::path(13), ring(8), ring(12)] {
            for np in [2u32, 3, 4] {
                let after = refine_partitions(&g, partition_csr(&g, None, np));
                assert_partition_invariants(&after, &g);
            }
        }
    }

    /// `partition_csr_refined` is the opt-in convenience: same partition count,
    /// invariants intact, cut no worse than the raw BFS cut.
    #[test]
    fn partition_csr_refined_is_opt_in_and_no_worse() {
        let g = ring(12);
        let raw = partition_csr(&g, None, 3);
        let refined = partition_csr_refined(&g, 3);
        assert_eq!(refined.len(), 3);
        assert!(edge_cut(&refined, &g) <= edge_cut(&raw, &g));
        assert_partition_invariants(&refined, &g);
    }

    /// Refinement is a no-op (cleanly) for degenerate P=1 / empty graphs.
    #[test]
    fn refine_handles_degenerate_inputs() {
        let g = CsrGraph::path(5);
        let one = refine_partitions(&g, partition_csr(&g, None, 1));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].n_owned, 5);

        let empty = CsrGraph {
            n_nodes: 0,
            offsets: vec![0],
            neighbors: vec![],
        };
        let parts = refine_partitions(&empty, partition_csr(&empty, None, 3));
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert_eq!(p.n_owned, 0);
        }
    }
}
