//! Topological-fisheye coarsening (Gansner, Koren, North — IEEE InfoVis
//! 2004), the §4 multilevel pipeline only.
//!
//! This module is the home of the paper-faithful coarsening algorithm
//! used by the force-directed sim as the `SeedMode::TopoFisheye` seed.
//! It is a strict upgrade over the simpler [`crate::layout::coarsen`]
//! pipeline used historically:
//!
//!   - Candidate set = graph edges **∪** filtered Delaunay edges (the
//!     "proximity graph" from the layout). Each DT edge is dropped if its
//!     graph-theoretic distance exceeds [`CoarsenParams::gt_max_hops`].
//!   - Pair selection scores five normalised measures (geometric
//!     proximity, cluster-size penalty, normalised connection strength,
//!     neighborhood Jaccard, inverse degree) per [`MatchWeights`].
//!   - Cluster sizes accumulate, coarse-edge weights sum across parallel
//!     edges, super-node positions are size-weighted centroids.
//!
//! The §5 (hybrid graph) and §6 (radial distortion) parts of the paper
//! are an interactive viewing technique and live separately in
//! `graph-compute::topo_fisheye`. They are not relevant to seeding a
//! force-directed sim.
//!
//! [`seed_positions`] is the high-level entry point used by the force
//! layout: it bootstraps a random ball, coarsens with this algorithm, lays
//! out the coarsest level with a tiny CPU Fruchterman-Reingold sim, then
//! prolongs + relaxes back down to the finest level and returns the
//! resulting interleaved `[x,y,z, …]` position buffer.

use std::collections::{HashMap, HashSet, VecDeque};

// ---------------- Public types ---------------------------------------------

/// Weights for the five pairwise contraction-score measures (paper §4).
/// Each measure is min-max normalised to `[0,1]` over the candidate set at
/// the current level, then linearly combined with these weights.
#[derive(Clone, Copy, Debug)]
pub struct MatchWeights {
    pub w_proximity: f32,
    pub w_size: f32,
    pub w_connection: f32,
    pub w_neighborhood: f32,
    pub w_degree: f32,
}

impl Default for MatchWeights {
    fn default() -> Self {
        Self {
            w_proximity: 1.0,
            w_size: 0.5,
            w_connection: 1.0,
            w_neighborhood: 0.5,
            w_degree: 0.25,
        }
    }
}

/// Tunables for the multilevel coarsen-and-seed pipeline.
#[derive(Clone, Debug)]
pub struct CoarsenParams {
    pub max_levels: usize,
    pub target_size: usize,
    /// Graph-theoretic hop budget for keeping a DT edge in the candidate
    /// set. Paper recommends 2 or 3; 2 guarantees no new cycles appear.
    pub gt_max_hops: u32,
    pub weights: MatchWeights,
}

impl Default for CoarsenParams {
    fn default() -> Self {
        Self {
            max_levels: 20,
            target_size: 20,
            gt_max_hops: 2,
            weights: MatchWeights::default(),
        }
    }
}

/// One level in the coarsening hierarchy.
#[derive(Clone, Debug)]
pub struct Level {
    pub n_nodes: usize,
    pub edges: Vec<u32>,
    pub edge_weights: Vec<f32>,
    /// Interleaved x,y,z, length `3 * n_nodes`.
    pub positions: Vec<f32>,
    pub sizes: Vec<u32>,
    /// Map from this level's nodes into the *next-coarser* level. Empty on
    /// the coarsest level.
    pub parent_map: Vec<u32>,
}

/// Full coarsening hierarchy. `levels[0]` is the input graph.
#[derive(Clone, Debug)]
pub struct TopoHierarchy {
    pub levels: Vec<Level>,
}

impl TopoHierarchy {
    pub fn n_levels(&self) -> usize {
        self.levels.len()
    }
}

// ---------------- Hierarchy builder ----------------------------------------

/// Build the hierarchy. `positions` is interleaved x,y,z (length `3 * n`);
/// only x,y is used for proximity. `edges` is a flat undirected `[s,t,...]`
/// list (no self-loops, deduped recommended).
pub fn build_hierarchy(
    n: usize,
    edges: &[u32],
    positions: &[f32],
    params: &CoarsenParams,
) -> TopoHierarchy {
    assert_eq!(
        positions.len(),
        n * 3,
        "positions must be interleaved xyz of length 3*n"
    );

    let edge_pairs = edges.len() / 2;
    let level0 = Level {
        n_nodes: n,
        edges: edges.to_vec(),
        edge_weights: vec![1.0; edge_pairs],
        positions: positions.to_vec(),
        sizes: vec![1; n],
        parent_map: Vec::new(),
    };

    let mut levels = Vec::with_capacity(params.max_levels);
    levels.push(level0);

    while levels.len() < params.max_levels {
        let cur = levels.last().unwrap();
        if cur.n_nodes <= params.target_size {
            break;
        }
        let (next, parent_map) = match coarsen_one_level(cur, params) {
            Some(x) => x,
            None => break,
        };
        let ratio = cur.n_nodes as f32 / next.n_nodes.max(1) as f32;
        let progressed = ratio >= 1.6;
        let mut next = next;
        next.parent_map = parent_map;
        levels.push(next);
        if !progressed {
            break;
        }
    }

    TopoHierarchy { levels }
}

fn coarsen_one_level(cur: &Level, params: &CoarsenParams) -> Option<(Level, Vec<u32>)> {
    let n = cur.n_nodes;
    if n < 2 {
        return None;
    }
    let candidates = build_candidate_set(cur, params.gt_max_hops);
    if candidates.is_empty() {
        return None;
    }
    let scores = score_candidates(cur, &candidates, &params.weights);
    let parent_map = greedy_match(n, &candidates, &scores);
    let n_super = (*parent_map.iter().max().unwrap_or(&0) as usize) + 1;
    if n_super == n {
        return None;
    }
    let (sizes, positions) = aggregate_geometry(cur, &parent_map, n_super);
    let (edges, edge_weights) = aggregate_edges(cur, &parent_map);
    Some((
        Level {
            n_nodes: n_super,
            edges,
            edge_weights,
            positions,
            sizes,
            parent_map: Vec::new(),
        },
        parent_map,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Pair {
    a: u32,
    b: u32,
}
impl Pair {
    fn new(i: u32, j: u32) -> Self {
        if i < j {
            Pair { a: i, b: j }
        } else {
            Pair { a: j, b: i }
        }
    }
}

fn build_candidate_set(cur: &Level, gt_max_hops: u32) -> Vec<Pair> {
    let mut set: HashSet<Pair> = HashSet::new();
    let mut e = 0;
    while e + 1 < cur.edges.len() {
        let s = cur.edges[e];
        let t = cur.edges[e + 1];
        e += 2;
        if s != t {
            set.insert(Pair::new(s, t));
        }
    }
    let dt_pairs = delaunay_edges(&cur.positions, cur.n_nodes);
    let adj = build_adjacency(cur);
    let mut dt_endpoints: HashSet<u32> = HashSet::new();
    for p in &dt_pairs {
        dt_endpoints.insert(p.a);
        dt_endpoints.insert(p.b);
    }
    let mut reach: HashMap<u32, HashSet<u32>> = HashMap::with_capacity(dt_endpoints.len());
    for &src in &dt_endpoints {
        reach.insert(src, bfs_within(&adj, src, gt_max_hops));
    }
    for p in dt_pairs {
        if reach.get(&p.a).map_or(false, |r| r.contains(&p.b)) {
            set.insert(p);
        }
    }
    set.into_iter().collect()
}

fn build_adjacency(cur: &Level) -> Vec<Vec<u32>> {
    let mut adj = vec![Vec::new(); cur.n_nodes];
    let mut e = 0;
    while e + 1 < cur.edges.len() {
        let s = cur.edges[e] as usize;
        let t = cur.edges[e + 1] as usize;
        e += 2;
        if s == t || s >= cur.n_nodes || t >= cur.n_nodes {
            continue;
        }
        adj[s].push(t as u32);
        adj[t].push(s as u32);
    }
    adj
}

fn bfs_within(adj: &[Vec<u32>], src: u32, max_hops: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    if max_hops == 0 {
        return out;
    }
    let mut visited: HashSet<u32> = HashSet::new();
    visited.insert(src);
    let mut q: VecDeque<(u32, u32)> = VecDeque::new();
    q.push_back((src, 0));
    while let Some((u, d)) = q.pop_front() {
        if d == max_hops {
            continue;
        }
        for &v in &adj[u as usize] {
            if visited.insert(v) {
                out.insert(v);
                q.push_back((v, d + 1));
            }
        }
    }
    out
}

/// Delaunay edges of the **xy-projection** of the (3-D) positions.
///
/// This codebase is 3-D but a full 3-D Delaunay tetrahedralisation needs
/// a different crate; `delaunator` is 2-D only. Projecting to xy is a
/// principled approximation here because the candidate set is *only used
/// for ranking pairs to contract*: any pair that's a true nearest neighbour
/// in 3-D will project to a near-pair in some 2-D view, and we additionally
/// keep the graph's own edge set unconditionally — so the worst case is
/// missing a candidate, never proposing a wrong one. The proximity score
/// itself uses the full 3-D distance (`score_candidates`).
fn delaunay_edges(positions: &[f32], n: usize) -> Vec<Pair> {
    if n < 3 {
        return Vec::new();
    }
    let pts: Vec<delaunator::Point> = (0..n)
        .map(|i| delaunator::Point {
            x: positions[3 * i] as f64,
            y: positions[3 * i + 1] as f64,
        })
        .collect();
    let tri = delaunator::triangulate(&pts);
    let triangles = tri.triangles;
    let mut set: HashSet<Pair> = HashSet::new();
    let mut i = 0;
    while i + 2 < triangles.len() {
        let a = triangles[i] as u32;
        let b = triangles[i + 1] as u32;
        let c = triangles[i + 2] as u32;
        i += 3;
        set.insert(Pair::new(a, b));
        set.insert(Pair::new(b, c));
        set.insert(Pair::new(a, c));
    }
    set.into_iter().collect()
}

fn score_candidates(cur: &Level, cands: &[Pair], w: &MatchWeights) -> Vec<f32> {
    let n = cur.n_nodes;
    let adj = build_adjacency(cur);
    let deg: Vec<f32> = adj.iter().map(|a| a.len() as f32).collect();
    let mut ew: HashMap<u64, f32> = HashMap::with_capacity(cur.edge_weights.len());
    let mut e = 0;
    let mut k = 0;
    while e + 1 < cur.edges.len() {
        let s = cur.edges[e];
        let t = cur.edges[e + 1];
        let key = pair_key(s, t);
        *ew.entry(key).or_insert(0.0) += cur.edge_weights.get(k).copied().unwrap_or(1.0);
        e += 2;
        k += 1;
    }
    let mut m1 = Vec::with_capacity(cands.len());
    let mut m2 = Vec::with_capacity(cands.len());
    let mut m3 = Vec::with_capacity(cands.len());
    let mut m4 = Vec::with_capacity(cands.len());
    let mut m5 = Vec::with_capacity(cands.len());
    for p in cands {
        let i = p.a as usize;
        let j = p.b as usize;
        debug_assert!(i < n && j < n);
        // 3-D proximity: this codebase renders in 3-D (wgpu + egui via
        // eframe). The paper is 2-D-only; we extend the proximity term to
        // the full xyz vector so coarsening in a 3-D seed doesn't collapse
        // pairs that look close in the xy projection but live far apart in z.
        let dx = cur.positions[3 * i] - cur.positions[3 * j];
        let dy = cur.positions[3 * i + 1] - cur.positions[3 * j + 1];
        let dz = cur.positions[3 * i + 2] - cur.positions[3 * j + 2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
        m1.push(1.0 / dist);
        let s = (cur.sizes[i] + cur.sizes[j]) as f32;
        m2.push(1.0 / s.max(1.0));
        let key = pair_key(p.a, p.b);
        let wij = ew.get(&key).copied().unwrap_or(0.0);
        let denom = ((cur.sizes[i] as f32) * (cur.sizes[j] as f32)).sqrt().max(1.0);
        m3.push(wij / denom);
        let ni: HashSet<u32> = adj[i].iter().copied().chain(std::iter::once(p.a)).collect();
        let nj: HashSet<u32> = adj[j].iter().copied().chain(std::iter::once(p.b)).collect();
        let inter = ni.intersection(&nj).count() as f32;
        let union = ni.union(&nj).count() as f32;
        m4.push(if union > 0.0 { inter / union } else { 0.0 });
        let dd = (deg[i] * deg[j]).max(1.0);
        m5.push(1.0 / dd);
    }
    normalize_inplace(&mut m1);
    normalize_inplace(&mut m2);
    normalize_inplace(&mut m3);
    normalize_inplace(&mut m4);
    normalize_inplace(&mut m5);
    cands
        .iter()
        .enumerate()
        .map(|(i, _)| {
            w.w_proximity * m1[i]
                + w.w_size * m2[i]
                + w.w_connection * m3[i]
                + w.w_neighborhood * m4[i]
                + w.w_degree * m5[i]
        })
        .collect()
}

fn pair_key(s: u32, t: u32) -> u64 {
    let (a, b) = if s < t { (s, t) } else { (t, s) };
    ((a as u64) << 32) | b as u64
}

fn normalize_inplace(v: &mut [f32]) {
    if v.is_empty() {
        return;
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &x in v.iter() {
        if x.is_finite() {
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    let span = hi - lo;
    if !span.is_finite() || span <= 0.0 {
        for x in v.iter_mut() {
            *x = 0.0;
        }
        return;
    }
    for x in v.iter_mut() {
        *x = ((*x - lo) / span).clamp(0.0, 1.0);
    }
}

fn greedy_match(n: usize, cands: &[Pair], scores: &[f32]) -> Vec<u32> {
    debug_assert_eq!(cands.len(), scores.len());
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut parent = vec![u32::MAX; n];
    let mut next: u32 = 0;
    for idx in order {
        let p = cands[idx];
        if parent[p.a as usize] == u32::MAX && parent[p.b as usize] == u32::MAX {
            parent[p.a as usize] = next;
            parent[p.b as usize] = next;
            next += 1;
        }
    }
    for slot in parent.iter_mut() {
        if *slot == u32::MAX {
            *slot = next;
            next += 1;
        }
    }
    parent
}

fn aggregate_geometry(cur: &Level, parent_map: &[u32], n_super: usize) -> (Vec<u32>, Vec<f32>) {
    let mut sizes = vec![0u32; n_super];
    let mut acc = vec![0.0f32; n_super * 3];
    for (i, &p) in parent_map.iter().enumerate() {
        let p = p as usize;
        let s = cur.sizes[i] as f32;
        sizes[p] += cur.sizes[i];
        acc[3 * p] += cur.positions[3 * i] * s;
        acc[3 * p + 1] += cur.positions[3 * i + 1] * s;
        acc[3 * p + 2] += cur.positions[3 * i + 2] * s;
    }
    for p in 0..n_super {
        let s = sizes[p].max(1) as f32;
        acc[3 * p] /= s;
        acc[3 * p + 1] /= s;
        acc[3 * p + 2] /= s;
    }
    (sizes, acc)
}

fn aggregate_edges(cur: &Level, parent_map: &[u32]) -> (Vec<u32>, Vec<f32>) {
    let mut acc: HashMap<u64, f32> = HashMap::with_capacity(cur.edge_weights.len());
    let mut e = 0;
    let mut k = 0;
    while e + 1 < cur.edges.len() {
        let s = parent_map[cur.edges[e] as usize];
        let t = parent_map[cur.edges[e + 1] as usize];
        let w = cur.edge_weights.get(k).copied().unwrap_or(1.0);
        e += 2;
        k += 1;
        if s == t {
            continue;
        }
        let key = pair_key(s, t);
        *acc.entry(key).or_insert(0.0) += w;
    }
    let mut edges = Vec::with_capacity(acc.len() * 2);
    let mut weights = Vec::with_capacity(acc.len());
    for (key, w) in acc {
        let a = (key >> 32) as u32;
        let b = (key & 0xFFFF_FFFF) as u32;
        edges.push(a);
        edges.push(b);
        weights.push(w);
    }
    (edges, weights)
}

// ---------------- High-level seed pipeline ---------------------------------

/// Produce initial positions for a force-directed sim using the paper's
/// §4 multilevel coarsening + prolong-and-relax pipeline.
///
/// 1. Bootstrap with a deterministic random ball of radius proportional
///    to `sqrt(n) * spring_len`.
/// 2. Build a hierarchy with [`build_hierarchy`] (DT-augmented candidates
///    + 5-measure matching, weighted-centroid super-nodes).
/// 3. Lay out the coarsest level from a fresh seeded random with a small
///    CPU Fruchterman-Reingold sim (≤ a few hundred nodes; O(n²) inner
///    loop is microseconds at that size).
/// 4. Walk back down level-by-level, prolonging positions through
///    `parent_map` + tiny jitter, and relaxing with another FR pass.
///
/// Returns the finest level's interleaved `[x,y,z, …]` buffer. Each `xyz`
/// triple is in world space.
pub fn seed_positions(
    n_nodes: usize,
    edges: &[u32],
    spring_len: f32,
    seed: u32,
    params: &CoarsenParams,
) -> Vec<f32> {
    if n_nodes == 0 {
        return Vec::new();
    }
    // Bootstrap random ball — coarsening uses these positions to evaluate
    // its proximity terms. Even a noisy bootstrap is fine because the FR
    // relax passes at each level dominate the final shape.
    let bootstrap = seeded_random_positions(n_nodes, spring_len, seed);

    // Coarsening below ~64 nodes is pointless overhead — fall back to FR
    // on the bootstrap directly.
    if n_nodes < 64 {
        let mut p = bootstrap;
        let radius = (n_nodes as f32).sqrt() * spring_len;
        cpu_fr_layout(&mut p, n_nodes, edges, radius, 200);
        return p;
    }

    let h = build_hierarchy(n_nodes, edges, &bootstrap, params);
    if h.levels.len() <= 1 {
        // Coarsening made no progress — relax the bootstrap and return.
        let mut p = bootstrap;
        let radius = (n_nodes as f32).sqrt() * spring_len;
        cpu_fr_layout(&mut p, n_nodes, edges, radius, 200);
        return p;
    }

    let coarsest = h.levels.last().unwrap();
    let radius = (coarsest.n_nodes as f32).sqrt() * spring_len;
    let mut positions = seeded_random_positions(coarsest.n_nodes, spring_len, seed);
    cpu_fr_layout(
        &mut positions,
        coarsest.n_nodes,
        &coarsest.edges,
        radius,
        200,
    );

    for l in (0..h.levels.len() - 1).rev() {
        let child = &h.levels[l];
        let parent_map = &h.levels[l + 1].parent_map;
        positions = prolong(
            &positions,
            parent_map,
            child.n_nodes,
            spring_len * 0.5,
            seed.wrapping_add(l as u32 + 1),
        );
        let radius_l = (child.n_nodes as f32).sqrt() * spring_len;
        cpu_fr_layout(&mut positions, child.n_nodes, &child.edges, radius_l, 50);
    }

    positions
}

// ---------------- Small helpers shared with the FR seed --------------------

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

/// Lift positions from a coarse level to the next finer one by inheriting
/// the parent's position + a tiny jitter so contracted siblings don't land
/// exactly on top of each other (zero repulsion gradient otherwise).
pub fn prolong(
    parent_positions: &[f32],
    parent_map: &[u32],
    n_child: usize,
    jitter: f32,
    seed: u32,
) -> Vec<f32> {
    debug_assert_eq!(parent_map.len(), n_child);
    let mut out = vec![0.0f32; n_child * 3];
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

/// Barebones Fruchterman-Reingold for small coarse levels. Not meant to
/// scale; O(n²) inner repulsion loop. For levels ≤ a few hundred nodes a
/// couple hundred steps is microseconds on a single core.
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
    let k = area_radius / (n as f32).sqrt();
    let k2 = k * k;
    let mut disp = vec![0.0f32; n * 3];
    for step in 0..steps {
        for d in disp.iter_mut() {
            *d = 0.0;
        }
        for i in 0..n {
            let xi = positions[i * 3];
            let yi = positions[i * 3 + 1];
            let zi = positions[i * 3 + 2];
            for j in (i + 1)..n {
                let dx = xi - positions[j * 3];
                let dy = yi - positions[j * 3 + 1];
                let dz = zi - positions[j * 3 + 2];
                let d2 = dx * dx + dy * dy + dz * dz + 1e-4;
                let f = k2 / d2;
                disp[i * 3] += dx * f;
                disp[i * 3 + 1] += dy * f;
                disp[i * 3 + 2] += dz * f;
                disp[j * 3] -= dx * f;
                disp[j * 3 + 1] -= dy * f;
                disp[j * 3 + 2] -= dz * f;
            }
        }
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
            let f = d / k;
            disp[a * 3] -= dx * f;
            disp[a * 3 + 1] -= dy * f;
            disp[a * 3 + 2] -= dz * f;
            disp[b * 3] += dx * f;
            disp[b * 3 + 1] += dy * f;
            disp[b * 3 + 2] += dz * f;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_edges(n: usize) -> Vec<u32> {
        (0..n as u32)
            .flat_map(|i| [i, (i + 1) % n as u32])
            .collect()
    }

    #[test]
    fn hierarchy_shrinks_ring() {
        let n = 64;
        let edges = ring_edges(n);
        let mut pos = vec![0.0f32; 3 * n];
        for i in 0..n {
            let t = (i as f32) / n as f32 * std::f32::consts::TAU;
            pos[3 * i] = t.cos() * 100.0;
            pos[3 * i + 1] = t.sin() * 100.0;
        }
        let h = build_hierarchy(n, &edges, &pos, &CoarsenParams::default());
        for w in h.levels.windows(2) {
            assert!(w[1].n_nodes < w[0].n_nodes);
        }
    }

    #[test]
    fn seed_positions_returns_finest_buffer() {
        let n = 128;
        let edges = ring_edges(n);
        let p = seed_positions(n, &edges, 30.0, 7, &CoarsenParams::default());
        assert_eq!(p.len(), 3 * n);
        // Not all zero / NaN.
        assert!(p.iter().any(|x| x.abs() > 0.0 && x.is_finite()));
    }

    #[test]
    fn seed_positions_spread_in_three_dimensions() {
        // Regression guard: this codebase renders in 3-D. The seed pipeline
        // must not collapse to the xy plane — that would happen if anywhere
        // in coarsen / prolong / FR we dropped the z coordinate. We require
        // the std-dev of z to be within an order of magnitude of x and y.
        let n = 256;
        let edges = ring_edges(n);
        let p = seed_positions(n, &edges, 30.0, 1234, &CoarsenParams::default());
        let stddev = |comp: usize| -> f32 {
            let xs: Vec<f32> = (0..n).map(|i| p[3 * i + comp]).collect();
            let mean: f32 = xs.iter().sum::<f32>() / n as f32;
            (xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n as f32).sqrt()
        };
        let sx = stddev(0);
        let sy = stddev(1);
        let sz = stddev(2);
        assert!(sx > 0.0 && sy > 0.0 && sz > 0.0);
        // z must not be collapsed: at least 10% of the larger of sx/sy.
        let xy_max = sx.max(sy);
        assert!(
            sz > 0.1 * xy_max,
            "z appears flattened: sx={sx} sy={sy} sz={sz}"
        );
    }

    #[test]
    fn seed_positions_tiny_graph_skips_coarsening() {
        let n = 4;
        let edges = vec![0, 1, 1, 2, 2, 3, 3, 0];
        let p = seed_positions(n, &edges, 30.0, 42, &CoarsenParams::default());
        assert_eq!(p.len(), 12);
        assert!(p.iter().all(|x| x.is_finite()));
    }
}
