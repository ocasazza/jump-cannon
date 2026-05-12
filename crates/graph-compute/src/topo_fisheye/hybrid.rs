//! Hybrid graph construction (paper §5).
//!
//! Given a [`TopoHierarchy`] and a focal node, produce a horizontal slice
//! through the hierarchy tree such that nodes near the focus are
//! represented at level 0 (finest) and nodes far from the focus are
//! represented at progressively coarser levels.
//!
//! The algorithm follows the paper's three phases:
//!
//!   1. **Wish list.** BFS from the focal node on G_0; sort all nodes by
//!      distance. Fill capacities `c_0, c_1, …` in order, so the closest
//!      `c_0` nodes wish to display at level 0, the next `c_1` at level 1,
//!      etc.
//!
//!   2. **Resolve conflicts.** A pair contracted at level `i+1` may have
//!      children with incompatible wishes. The paper's case analysis
//!      (mapped to our binary-matching tree):
//!        - If any unresolved child wants level `≤ i`, then *all*
//!          unresolved children become active at level `i` (a finer-wishing
//!          sibling drags its coarser-wishing sibling down).
//!        - Else if the minimum desired level is exactly `i+1`, the parent
//!          becomes active and the subtree is done.
//!        - Else (all want `> i+1`), set the parent's desired level to that
//!          minimum and propagate upward.
//!
//!   3. **Edges.** For each active node A at level `l`, scan its neighbours
//!      in the level-`l` coarse graph. For each neighbour B at level `l`:
//!        - If B is active too: emit ⟨A, B⟩.
//!        - Else if B has an active ancestor C (at some coarser level):
//!          emit ⟨A, C⟩.
//!        - Else (B has an active *descendant*): skip — that edge will be
//!          materialized from the descendant's side.

use std::collections::{HashMap, HashSet, VecDeque};

use super::types::HybridGraph;
use graph_layouts::topo_fisheye::{Level, TopoHierarchy};

#[derive(Clone, Debug)]
pub struct HybridParams {
    /// Index of the focal node within the finest level (level 0).
    pub focal_node: u32,
    /// Per-level node capacities `c_0, c_1, …`. If empty, defaults to
    /// `c_0 = 64`, `c_{i+1} = 2 · c_i` (paper recommends `c_0 ∈ [50,100]`
    /// and `c_{i+1} = C · c_i` with `C ∈ [2,3]`).
    pub capacities: Vec<u32>,
}

impl Default for HybridParams {
    fn default() -> Self {
        Self {
            focal_node: 0,
            capacities: Vec::new(),
        }
    }
}

pub fn build_hybrid(h: &TopoHierarchy, p: &HybridParams) -> HybridGraph {
    let n_levels = h.n_levels();
    if n_levels == 0 {
        return empty();
    }
    let n0 = h.levels[0].n_nodes;
    if n0 == 0 {
        return empty();
    }

    // --- Phase 1: BFS distances from focus on G_0, then wish-list. ---
    let dist = bfs_distances(&h.levels[0], p.focal_node);
    let mut order: Vec<u32> = (0..n0 as u32).collect();
    order.sort_by_key(|&i| dist[i as usize]);

    let caps = if p.capacities.is_empty() {
        default_capacities(n_levels, n0 as u32)
    } else {
        p.capacities.clone()
    };

    let max_level = (n_levels - 1) as u32;
    let mut wish = vec![max_level; n0];
    let mut current_level = 0u32;
    let mut remaining = caps.first().copied().unwrap_or(n0 as u32);
    for &i in &order {
        while remaining == 0 && current_level < max_level {
            current_level += 1;
            remaining = caps.get(current_level as usize).copied().unwrap_or(u32::MAX);
        }
        wish[i as usize] = current_level;
        if remaining > 0 && current_level < max_level {
            remaining -= 1;
        }
    }

    // --- Phase 2: child-of-parent lists, then bottom-up resolution. ---
    let children = invert_parent_maps(h);

    let mut desired: Vec<Vec<u32>> = Vec::with_capacity(n_levels);
    desired.push(wish);
    for l in 1..n_levels {
        desired.push(vec![0u32; h.levels[l].n_nodes]);
    }

    let mut resolved: Vec<Vec<bool>> = (0..n_levels)
        .map(|l| vec![false; h.levels[l].n_nodes])
        .collect();
    let mut active: Vec<Vec<bool>> = (0..n_levels)
        .map(|l| vec![false; h.levels[l].n_nodes])
        .collect();

    for l in 0..n_levels - 1 {
        let n_parents = h.levels[l + 1].n_nodes;
        let i = l as u32; // child level
        for parent in 0..n_parents {
            let ch = &children[l + 1][parent];
            let unresolved: Vec<u32> = ch
                .iter()
                .copied()
                .filter(|&c| !resolved[l][c as usize])
                .collect();
            if unresolved.is_empty() {
                resolved[l + 1][parent] = true;
                continue;
            }
            let min_d = unresolved
                .iter()
                .map(|&c| desired[l][c as usize])
                .min()
                .unwrap();

            if min_d <= i {
                // Finer-wishing child drags any coarser-wishing sibling to level i.
                for &c in &unresolved {
                    active[l][c as usize] = true;
                    resolved[l][c as usize] = true;
                }
                resolved[l + 1][parent] = true;
            } else if min_d == i + 1 {
                active[l + 1][parent] = true;
                resolved[l + 1][parent] = true;
                for &c in &unresolved {
                    resolved[l][c as usize] = true;
                }
            } else {
                // min_d > i + 1: propagate the wish to the parent and keep going.
                desired[l + 1][parent] = min_d;
                for &c in &unresolved {
                    resolved[l][c as usize] = true;
                }
                // parent stays unresolved
            }
        }
    }

    // Any node still unresolved at the top is a root of an unfinished subtree.
    let top = n_levels - 1;
    for v in 0..h.levels[top].n_nodes {
        if !resolved[top][v] {
            active[top][v] = true;
        }
    }

    // --- Build the active-node list + index. ---
    let mut nodes: Vec<(u32, u32)> = Vec::new();
    let mut positions: Vec<f32> = Vec::new();
    let mut node_levels: Vec<u32> = Vec::new();
    let mut idx_of: HashMap<(u32, u32), u32> = HashMap::new();
    for l in 0..n_levels {
        for v in 0..h.levels[l].n_nodes {
            if active[l][v] {
                idx_of.insert((l as u32, v as u32), nodes.len() as u32);
                nodes.push((l as u32, v as u32));
                positions.push(h.levels[l].positions[3 * v]);
                positions.push(h.levels[l].positions[3 * v + 1]);
                positions.push(h.levels[l].positions[3 * v + 2]);
                node_levels.push(l as u32);
            }
        }
    }

    // --- For non-active nodes, walk upward to find their active ancestor (if any). ---
    let active_ancestor = compute_active_ancestors(h, &active);

    // --- Phase 3: edges. ---
    let mut edge_set: HashSet<u64> = HashSet::new();
    let mut edges: Vec<u32> = Vec::new();
    let mut edge_levels: Vec<u32> = Vec::new();

    for l in 0..n_levels {
        let lvl = &h.levels[l];
        // Walk every undirected edge at this level once and only emit when
        // at least one endpoint is active. The paper iterates "for each
        // active node A, scan its neighbours in coarse graph", but walking
        // the edge list once is equivalent and lets us dedup with a single
        // HashSet.
        let mut e = 0;
        while e + 1 < lvl.edges.len() {
            let s = lvl.edges[e];
            let t = lvl.edges[e + 1];
            e += 2;
            let a_active = active[l][s as usize];
            let b_active = active[l][t as usize];

            // Map each endpoint to its hybrid representative.
            let rep_a = if a_active {
                Some((l as u32, s))
            } else {
                active_ancestor.get(&(l as u32, s)).copied()
            };
            let rep_b = if b_active {
                Some((l as u32, t))
            } else {
                active_ancestor.get(&(l as u32, t)).copied()
            };

            let (ra, rb) = match (rep_a, rep_b) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };
            if ra == rb {
                continue; // self-loop after collapsing into shared ancestor
            }
            // Only emit if at least one endpoint is active *at this level*.
            // Otherwise the edge will be handled at the lowest level where
            // one of the endpoints (or their descendant) is active — this
            // mirrors paper case 3 ("skip; emit from descendant's side").
            if !a_active && !b_active {
                continue;
            }
            let ia = *idx_of
                .get(&ra)
                .expect("active node missing from hybrid index");
            let ib = *idx_of
                .get(&rb)
                .expect("active node missing from hybrid index");
            let (lo, hi) = if ia < ib { (ia, ib) } else { (ib, ia) };
            let key = ((lo as u64) << 32) | hi as u64;
            if edge_set.insert(key) {
                edges.push(ia);
                edges.push(ib);
                edge_levels.push(ra.0.max(rb.0));
            }
        }
    }

    HybridGraph {
        nodes,
        positions,
        edges,
        edge_levels,
        node_levels,
    }
}

fn empty() -> HybridGraph {
    HybridGraph {
        nodes: Vec::new(),
        positions: Vec::new(),
        edges: Vec::new(),
        edge_levels: Vec::new(),
        node_levels: Vec::new(),
    }
}

/// BFS unweighted distances from `src` on the level-0 graph. Unreachable
/// nodes get `u32::MAX` so they sort to the periphery.
fn bfs_distances(g0: &Level, src: u32) -> Vec<u32> {
    let n = g0.n_nodes;
    let mut dist = vec![u32::MAX; n];
    if (src as usize) >= n {
        return dist;
    }
    let mut adj = vec![Vec::new(); n];
    let mut e = 0;
    while e + 1 < g0.edges.len() {
        let s = g0.edges[e] as usize;
        let t = g0.edges[e + 1] as usize;
        e += 2;
        if s == t || s >= n || t >= n {
            continue;
        }
        adj[s].push(t as u32);
        adj[t].push(s as u32);
    }
    dist[src as usize] = 0;
    let mut q: VecDeque<u32> = VecDeque::new();
    q.push_back(src);
    while let Some(u) = q.pop_front() {
        let d = dist[u as usize];
        for &v in &adj[u as usize] {
            if dist[v as usize] == u32::MAX {
                dist[v as usize] = d + 1;
                q.push_back(v);
            }
        }
    }
    dist
}

fn default_capacities(n_levels: usize, n0: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(n_levels);
    let mut c = 64u32;
    let mut total = 0u32;
    for l in 0..n_levels {
        if l + 1 == n_levels {
            // Last bucket soaks up everything remaining.
            out.push(n0.saturating_sub(total).max(1));
        } else {
            out.push(c);
            total = total.saturating_add(c);
            c = c.saturating_mul(2);
        }
    }
    out
}

/// For each level `l > 0`, `out[l][parent]` lists the indices at level `l-1`
/// that contracted into `parent`.
fn invert_parent_maps(h: &TopoHierarchy) -> Vec<Vec<Vec<u32>>> {
    let n_levels = h.n_levels();
    let mut children: Vec<Vec<Vec<u32>>> = Vec::with_capacity(n_levels);
    children.push(Vec::new());
    for l in 1..n_levels {
        let pm = &h.levels[l].parent_map;
        let mut ch: Vec<Vec<u32>> = vec![Vec::new(); h.levels[l].n_nodes];
        for (child_idx, &parent_idx) in pm.iter().enumerate() {
            ch[parent_idx as usize].push(child_idx as u32);
        }
        children.push(ch);
    }
    children
}

/// For every non-active node `(l, v)`, follow the parent chain upward and
/// record the first active ancestor (if one exists). Active nodes map to
/// themselves implicitly; we leave them out of the map to keep it sparse.
fn compute_active_ancestors(
    h: &TopoHierarchy,
    active: &[Vec<bool>],
) -> HashMap<(u32, u32), (u32, u32)> {
    let n_levels = h.n_levels();
    let mut out: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    // Walk top-down: at each level we already know each node's active
    // ancestor (either the node itself if active, or what its parent points
    // to, or nothing).
    // Sentinel: level n_levels has no nodes; we just initialise from the top.
    for v in 0..h.levels[n_levels - 1].n_nodes {
        if active[n_levels - 1][v] {
            // self — leave implicit (active nodes are their own rep, callers
            // check `active[l][v]` directly)
        }
        // No active ancestor above the top.
    }
    for l in (0..n_levels - 1).rev() {
        let pm = &h.levels[l + 1].parent_map;
        for v in 0..h.levels[l].n_nodes {
            if active[l][v] {
                continue;
            }
            let parent = pm[v] as usize;
            let parent_key = (l as u32 + 1, parent as u32);
            if active[l + 1][parent] {
                out.insert((l as u32, v as u32), parent_key);
            } else if let Some(&anc) = out.get(&parent_key) {
                out.insert((l as u32, v as u32), anc);
            }
            // else: no active ancestor (active nodes live below in this
            // subtree; non-active node has no representative)
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_layouts::topo_fisheye::{build_hierarchy, CoarsenParams};

    fn ring(n: usize, r: f32) -> (Vec<u32>, Vec<f32>) {
        let mut edges = Vec::with_capacity(2 * n);
        for i in 0..n {
            edges.push(i as u32);
            edges.push(((i + 1) % n) as u32);
        }
        let mut pos = Vec::with_capacity(3 * n);
        for i in 0..n {
            let t = (i as f32) / n as f32 * std::f32::consts::TAU;
            pos.push(t.cos() * r);
            pos.push(t.sin() * r);
            pos.push(0.0);
        }
        (edges, pos)
    }

    #[test]
    fn hybrid_focus_at_zero_keeps_focus_at_finest_level() {
        let n = 32;
        let (edges, positions) = ring(n, 100.0);
        let h = build_hierarchy(
            n,
            &edges,
            &positions,
            &CoarsenParams {
                max_levels: 6,
                target_size: 2,
                gt_max_hops: 2,
                weights: Default::default(),
            },
        );
        let hg = build_hybrid(
            &h,
            &HybridParams {
                focal_node: 0,
                capacities: vec![4, 8, 16, 32],
            },
        );
        // Focal node 0 must appear as a level-0 active node.
        let has_focus = hg.nodes.iter().any(|&(l, v)| l == 0 && v == 0);
        assert!(has_focus, "focal node must be active at level 0");
        // Each level-0 node must be represented by exactly one hybrid node
        // (itself if active, else via an active ancestor reachable through
        // edges). We approximate by checking node count is non-trivial.
        assert!(!hg.nodes.is_empty());
        assert!(!hg.edges.is_empty());
    }

    #[test]
    fn hybrid_node_count_decreases_with_smaller_capacities() {
        let n = 64;
        let (edges, positions) = ring(n, 100.0);
        let h = build_hierarchy(
            n,
            &edges,
            &positions,
            &CoarsenParams {
                max_levels: 8,
                target_size: 2,
                gt_max_hops: 2,
                weights: Default::default(),
            },
        );
        let big = build_hybrid(
            &h,
            &HybridParams {
                focal_node: 0,
                capacities: vec![64],
            },
        );
        let small = build_hybrid(
            &h,
            &HybridParams {
                focal_node: 0,
                capacities: vec![4, 8],
            },
        );
        // big keeps the original at level 0 (~n nodes), small compresses
        // most of the ring into coarse super-nodes (< n).
        assert!(
            small.nodes.len() < big.nodes.len(),
            "expected fewer hybrid nodes with smaller capacities; big={}, small={}",
            big.nodes.len(),
            small.nodes.len()
        );
    }

    #[test]
    fn hybrid_edges_have_no_self_loops_and_are_dedup() {
        let n = 32;
        let (edges, positions) = ring(n, 100.0);
        let h = build_hierarchy(n, &edges, &positions, &CoarsenParams::default());
        let hg = build_hybrid(
            &h,
            &HybridParams {
                focal_node: 0,
                capacities: vec![4, 8, 16],
            },
        );
        let mut seen: HashSet<u64> = HashSet::new();
        let mut e = 0;
        while e + 1 < hg.edges.len() {
            let a = hg.edges[e];
            let b = hg.edges[e + 1];
            assert_ne!(a, b, "self-loop in hybrid edges");
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            assert!(seen.insert(((lo as u64) << 32) | hi as u64));
            e += 2;
        }
    }
}
