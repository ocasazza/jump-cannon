//! Multilevel graph coarsening for force-directed initial placement.
//!
//! Implements the classic FM3 / sfdp pattern (Hachul & Jünger; Hu): build a
//! cascade of contracted graphs G_0, G_1, …, G_L by maximal-matching edge
//! collapse, lay out the coarsest level cheaply, then prolong (interpolate)
//! positions back down level-by-level. By the time we reach the finest
//! level the layout is already ~95% converged in a handful of frames
//! instead of the hundreds the GPU sim would otherwise need from a random
//! start.
//!
//! Refs:
//!  - Hachul & Jünger, FM3:    http://e-archive.informatik.uni-koeln.de/509/
//!  - Hu, sfdp:                http://yifanhu.net/PUB/graph_draw_small.pdf
//!
//! This module is **CPU-only and topology-only**. It operates on the same
//! flat buffers the bootstrap already has: positions as `[x,y,z, ...]` and
//! edges as `[s,t, ...]`. No `Graph` round-trip. Coarse graphs are tiny
//! (≤ a few hundred nodes), so a quick CPU Fruchterman-Reingold sim at each
//! level is ~microseconds — not worth the GPU dispatch overhead.

use std::collections::HashSet;

/// One level of the coarsening cascade.
///
/// `parent_map[i]` gives the super-node index in the *next-coarser* level
/// (i.e. level l+1) that child i (in this level, l) was contracted into.
/// For the coarsest level, `parent_map` is empty.
#[derive(Debug, Clone)]
pub struct CoarseLevel {
    pub n_nodes: usize,
    /// Flat edge list `[s,t, s,t, ...]` after contraction at this level.
    /// Self-loops are dropped. Parallel edges between the same super-node
    /// pair are collapsed (we don't need edge weights for placement).
    pub edges: Vec<u32>,
    /// Length = n_nodes of the *finer* level (l-1). Empty at level 0.
    /// `parent_map[child_idx] = super_idx_in_this_level`.
    /// Stored on the *coarser* level so prolong() reads it directly.
    pub parent_map: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Coarsening {
    /// `levels[0]` = G_0 (input topology). `levels.last()` = coarsest.
    pub levels: Vec<CoarseLevel>,
}

/// Build a coarsening cascade from a flat edge list.
///
/// Stops when |V_l| ≤ `target_size` OR coarsening ratio < 1.6 (i.e. no
/// meaningful progress on a sparse / matching-resistant graph) OR we hit
/// `max_levels`.
pub fn coarsen(
    n_nodes: usize,
    edges: &[u32],
    max_levels: usize,
    target_size: usize,
) -> Coarsening {
    // Level 0 is the input, no parent_map.
    let mut levels = Vec::with_capacity(max_levels.max(1));
    levels.push(CoarseLevel {
        n_nodes,
        edges: edges.to_vec(),
        parent_map: Vec::new(),
    });

    for _ in 1..max_levels {
        let cur = levels.last().unwrap();
        if cur.n_nodes <= target_size {
            break;
        }
        let (next_n, parent_map, next_edges) = contract_one_level(cur.n_nodes, &cur.edges);
        // Stop if matching made no real progress (e.g. an already-matched
        // graph, dense bipartite degeneracy, isolated stars). 1.6 is the
        // sfdp default — empirically, below that the cascade depth balloons
        // without speedup.
        let ratio = cur.n_nodes as f32 / next_n.max(1) as f32;
        if ratio < 1.6 && next_n == cur.n_nodes {
            break;
        }
        levels.push(CoarseLevel {
            n_nodes: next_n,
            edges: next_edges,
            parent_map,
        });
        if next_n <= target_size || ratio < 1.6 {
            break;
        }
    }

    Coarsening { levels }
}

/// One round of maximal-matching edge contraction.
///
/// Greedy: walk edges in order, match each unmatched endpoint pair, contract
/// each matched edge into a super-node. Unmatched nodes promote to their
/// own super-node. Returns `(n_super, parent_map, edges_super)`.
fn contract_one_level(n: usize, edges: &[u32]) -> (usize, Vec<u32>, Vec<u32>) {
    // parent[i] = super-node id, or u32::MAX if not yet assigned.
    let mut parent = vec![u32::MAX; n];
    let mut next_super: u32 = 0;

    // Greedy maximal matching: first-touch wins. This is O(|E|) and good
    // enough — the FM3 paper notes deeper matchings (heavy-edge, etc.)
    // aren't worth the implementation complexity for placement seeding.
    let mut i = 0;
    while i + 1 < edges.len() {
        let s = edges[i] as usize;
        let t = edges[i + 1] as usize;
        i += 2;
        if s == t || s >= n || t >= n {
            continue;
        }
        if parent[s] == u32::MAX && parent[t] == u32::MAX {
            parent[s] = next_super;
            parent[t] = next_super;
            next_super += 1;
        }
    }
    // Unmatched singletons each become their own super-node.
    for p in parent.iter_mut() {
        if *p == u32::MAX {
            *p = next_super;
            next_super += 1;
        }
    }

    let n_super = next_super as usize;

    // Build contracted edge list, dedup'd. HashSet cap is fine for the
    // sizes we hit here (top-level edges ~10× n_nodes; level-on-level it
    // shrinks ~2×).
    let mut seen: HashSet<u64> = HashSet::with_capacity(edges.len() / 2);
    let mut out = Vec::with_capacity(edges.len() / 2);
    let mut j = 0;
    while j + 1 < edges.len() {
        let s = parent[edges[j] as usize];
        let t = parent[edges[j + 1] as usize];
        j += 2;
        if s == t {
            continue; // self-loop after contraction
        }
        let (a, b) = if s < t { (s, t) } else { (t, s) };
        let key = ((a as u64) << 32) | b as u64;
        if seen.insert(key) {
            out.push(s);
            out.push(t);
        }
    }

    (n_super, parent, out)
}

/// Lift positions from a coarse level (parent) to the finer level (child)
/// by inheritance + small jitter. `parent_map[child_idx] = parent_idx`.
///
/// Without jitter, contracted-pair children land on top of each other and
/// the next sim level can't tell them apart (zero repulsion gradient).
/// `jitter` is in world units; ~0.5 × spring_len works well.
pub fn prolong(
    parent_positions: &[f32],
    parent_map: &[u32],
    n_child: usize,
    jitter: f32,
    seed: u32,
) -> Vec<f32> {
    debug_assert_eq!(parent_map.len(), n_child);
    let mut out = vec![0.0f32; n_child * 3];
    // tiny xorshift, no rand dep — must be deterministic for repro.
    let mut s = (seed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    };
    for i in 0..n_child {
        let p = parent_map[i] as usize;
        let base = p * 3;
        out[i * 3] = parent_positions[base] + next() * jitter;
        out[i * 3 + 1] = parent_positions[base + 1] + next() * jitter;
        out[i * 3 + 2] = parent_positions[base + 2] + next() * jitter;
    }
    out
}

/// Run a quick Fruchterman-Reingold sim on a (small) coarse level and write
/// positions in-place. This is intentionally a barebones FR — no Barnes-Hut,
/// no spatial grid — because the level sizes we run it on are ≤ a few
/// hundred nodes and an O(n²) inner loop on 500 nodes for 200 steps is
/// ~50ms total on CPU. Not worth GPU dispatch overhead.
///
/// `area_radius` is the rough world-space radius the sim should fill;
/// drives the FR `k` constant. `steps` is iteration count.
pub fn cpu_fr_layout(
    positions: &mut [f32],
    n: usize,
    edges: &[u32],
    area_radius: f32,
    steps: u32,
) {
    if n <= 1 || steps == 0 {
        return;
    }
    // Classic FR k = sqrt(area / n). area = π r² → k ≈ r / sqrt(n) up to
    // a constant. We want k roughly equal to spring_len (~30), so size
    // area_radius accordingly when calling.
    let k = area_radius / (n as f32).sqrt();
    let k2 = k * k;
    let mut disp = vec![0.0f32; n * 3];
    // Linear cool from 0.1 * k → near zero. Caps single-step displacement
    // so the sim doesn't explode early when nodes start randomly placed.
    for step in 0..steps {
        for d in disp.iter_mut() {
            *d = 0.0;
        }
        // Repulsion: O(n²). For coarsened sizes (≤500) this is fine.
        for i in 0..n {
            let xi = positions[i * 3];
            let yi = positions[i * 3 + 1];
            let zi = positions[i * 3 + 2];
            for j in (i + 1)..n {
                let dx = xi - positions[j * 3];
                let dy = yi - positions[j * 3 + 1];
                let dz = zi - positions[j * 3 + 2];
                let d2 = dx * dx + dy * dy + dz * dz + 1e-4;
                // FR repulsion magnitude k²/d², scattered along (dx,dy,dz).
                let f = k2 / d2;
                disp[i * 3] += dx * f;
                disp[i * 3 + 1] += dy * f;
                disp[i * 3 + 2] += dz * f;
                disp[j * 3] -= dx * f;
                disp[j * 3 + 1] -= dy * f;
                disp[j * 3 + 2] -= dz * f;
            }
        }
        // Spring attraction along edges.
        let mut e = 0;
        while e + 1 < edges.len() {
            let a = edges[e] as usize;
            let b = edges[e + 1] as usize;
            e += 2;
            if a >= n || b >= n {
                continue;
            }
            let dx = positions[a * 3] - positions[b * 3];
            let dy = positions[a * 3 + 1] - positions[b * 3 + 1];
            let dz = positions[a * 3 + 2] - positions[b * 3 + 2];
            let d = (dx * dx + dy * dy + dz * dz + 1e-4).sqrt();
            // FR attraction magnitude d²/k.
            let f = d / k;
            disp[a * 3] -= dx * f;
            disp[a * 3 + 1] -= dy * f;
            disp[a * 3 + 2] -= dz * f;
            disp[b * 3] += dx * f;
            disp[b * 3 + 1] += dy * f;
            disp[b * 3 + 2] += dz * f;
        }
        // Cool: linear schedule from k*0.1 down to ~0 over `steps`.
        let t = k * 0.1 * (1.0 - step as f32 / steps as f32).max(0.05);
        for i in 0..n {
            let dx = disp[i * 3];
            let dy = disp[i * 3 + 1];
            let dz = disp[i * 3 + 2];
            let mag = (dx * dx + dy * dy + dz * dz + 1e-12).sqrt();
            let cap = mag.min(t) / mag;
            positions[i * 3] += dx * cap;
            positions[i * 3 + 1] += dy * cap;
            positions[i * 3 + 2] += dz * cap;
        }
    }
}

/// Convenience top-level entry point: build the cascade, lay out the
/// coarsest level from a seeded random start, prolong + relax level-by-level
/// down to G_0, return the finest-level `[x,y,z, ...]` position buffer.
///
/// `spring_len` should match the live GPU sim's `spring_len` so the seed
/// scale lands in a regime the GPU sim is happy to refine instead of
/// re-explode.
pub fn warmup_positions(
    n_nodes: usize,
    edges: &[u32],
    spring_len: f32,
    seed: u32,
) -> Vec<f32> {
    // No-op for tiny graphs (e.g. the 4-node browser test). Coarsening
    // a 4-node graph is pointless and would just inject jitter into a
    // perfectly fine random init.
    if n_nodes < 64 {
        return seeded_random_positions(n_nodes, spring_len, seed);
    }

    let cascade = coarsen(n_nodes, edges, 6, 500);
    if cascade.levels.len() <= 1 {
        // Coarsening made no progress (edge-light graph). Fall back.
        return seeded_random_positions(n_nodes, spring_len, seed);
    }

    // Lay out the coarsest level from a small random ball.
    let coarsest = cascade.levels.last().unwrap();
    let radius = (coarsest.n_nodes as f32).sqrt() * spring_len;
    let mut positions = seeded_random_positions(coarsest.n_nodes, spring_len, seed);
    cpu_fr_layout(&mut positions, coarsest.n_nodes, &coarsest.edges, radius, 200);

    // Walk back down, prolonging + relaxing at each step.
    for l in (0..cascade.levels.len() - 1).rev() {
        let child = &cascade.levels[l];
        // The coarser level (l+1) holds the parent_map describing how
        // child indices fold into super-nodes.
        let parent_map = &cascade.levels[l + 1].parent_map;
        positions = prolong(
            &positions,
            parent_map,
            child.n_nodes,
            spring_len * 0.5,
            seed.wrapping_add(l as u32 + 1),
        );
        // Relax fewer steps at finer levels — the structure is already
        // close, we just need to settle the new degrees of freedom.
        let radius_l = (child.n_nodes as f32).sqrt() * spring_len;
        cpu_fr_layout(&mut positions, child.n_nodes, &child.edges, radius_l, 50);
    }

    positions
}

/// Random ball of radius proportional to sqrt(n) * spring_len.
fn seeded_random_positions(n: usize, spring_len: f32, seed: u32) -> Vec<f32> {
    let radius = (n as f32).sqrt() * spring_len;
    let mut s = (seed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    };
    let mut out = Vec::with_capacity(n * 3);
    for _ in 0..n {
        out.push(next() * radius);
        out.push(next() * radius);
        out.push(next() * radius);
    }
    out
}

#[cfg(test)]
mod coarsen_tests {
    use super::*;

    #[test]
    fn coarsen_collapses_a_path() {
        // 0-1-2-3-4-5 path. Greedy matching pairs (0,1),(2,3),(4,5) → 3 supers.
        let edges = vec![0, 1, 1, 2, 2, 3, 3, 4, 4, 5];
        let c = coarsen(6, &edges, 4, 2);
        assert_eq!(c.levels[0].n_nodes, 6);
        // Should have at least one coarser level.
        assert!(c.levels.len() >= 2);
        let l1 = &c.levels[1];
        assert_eq!(l1.parent_map.len(), 6);
        assert!(l1.n_nodes < 6);
    }

    #[test]
    fn warmup_tiny_graph_is_noop_path() {
        // 4-node test (the browser harness). Should return a position
        // buffer of length 12 without panicking and not coarsen.
        let edges = vec![0, 1, 1, 2, 2, 3, 3, 0];
        let p = warmup_positions(4, &edges, 30.0, 42);
        assert_eq!(p.len(), 12);
    }

    #[test]
    fn prolong_inherits_parent_position() {
        // 2 children → 1 parent. Both children must land near (10, 20, 30).
        let parent_pos = vec![10.0, 20.0, 30.0];
        let parent_map = vec![0u32, 0u32];
        let child = prolong(&parent_pos, &parent_map, 2, 0.0, 7);
        assert_eq!(child, vec![10.0, 20.0, 30.0, 10.0, 20.0, 30.0]);
    }
}
