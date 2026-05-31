//! Layout-quality metrics — representation-agnostic.
//!
//! Every function takes **packed positions** (`[x0,y0,z0, x1,y1,z1, …]`, the
//! same layout the engines emit) plus caller-supplied terms, so the SAME code
//! serves the native solver tests, the WASM renderer (a live metrics panel),
//! and the benchmark harness. No graph type is baked in: the caller decides
//! which pairs/edges to include and what the target distances are.
//!
//! Why scale-normalized stress: standard stress `Σ w_ij(‖x_i−x_j‖−d_ij)²` is
//! *scale-sensitive* — uniformly scaling a layout changes it — so it is not a
//! fair quality measure or cross-layout comparison on its own. The accepted fix
//! (Kobourov et al., graph-drawing metrics; arXiv:1201.3011) is to minimize over
//! a global scale `α` first (closed form) and normalize, giving a dimensionless
//! value in `[0, ~1]` that is invariant to rotation, translation, and uniform
//! scale. `0` ⇒ the layout reproduces the target distances exactly up to scale.

/// Euclidean distance between packed nodes `i` and `j`.
#[inline]
fn euclid(positions: &[f32], i: usize, j: usize) -> f64 {
    let dx = (positions[3 * i] - positions[3 * j]) as f64;
    let dy = (positions[3 * i + 1] - positions[3 * j + 1]) as f64;
    let dz = (positions[3 * i + 2] - positions[3 * j + 2]) as f64;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Scale-normalized stress over the given `(i, j, d_ij)` terms (`d_ij > 0`,
/// `w_ij = 1/d_ij²`). Minimizes over a global scale `α* = Σ w·d·D / Σ w·D²`
/// then normalizes by `Σ w·d²`. Dimensionless, scale/rotation/translation
/// invariant, in `[0, ~1]`; `0` = exact distance reproduction up to scale.
/// Returns `0.0` for an empty/degenerate term set.
pub fn scale_normalized_stress(positions: &[f32], terms: &[(u32, u32, f32)]) -> f32 {
    let (mut num, mut den, mut wd2) = (0.0f64, 0.0f64, 0.0f64);
    for &(i, j, d) in terms {
        if d <= 0.0 {
            continue;
        }
        let d = d as f64;
        let w = 1.0 / (d * d);
        let big = euclid(positions, i as usize, j as usize);
        num += w * d * big;
        den += w * big * big;
        wd2 += w * d * d;
    }
    if den <= 0.0 || wd2 <= 0.0 {
        return 0.0;
    }
    let alpha = num / den;
    let mut s = 0.0f64;
    for &(i, j, d) in terms {
        if d <= 0.0 {
            continue;
        }
        let d = d as f64;
        let w = 1.0 / (d * d);
        let r = alpha * euclid(positions, i as usize, j as usize) - d;
        s += w * r * r;
    }
    (s / wd2) as f32
}

/// Raw (un-normalized) stress `Σ w_ij(‖x_i−x_j‖−d_ij)²`, `w_ij = 1/d_ij²`.
/// Scale-sensitive — prefer [`scale_normalized_stress`] for comparisons; this is
/// here for diagnostics and for asserting absolute convergence on fixed scales.
pub fn stress(positions: &[f32], terms: &[(u32, u32, f32)]) -> f32 {
    let mut s = 0.0f64;
    for &(i, j, d) in terms {
        if d <= 0.0 {
            continue;
        }
        let d = d as f64;
        let w = 1.0 / (d * d);
        let r = euclid(positions, i as usize, j as usize) - d;
        s += w * r * r;
    }
    s as f32
}

/// Scale-normalized stress over `edges` with a UNIFORM target distance of 1
/// (every edge "wants" the same length). Identical to [`scale_normalized_stress`]
/// with all `d_ij = 1`, but without materializing a terms vector — for the cheap
/// per-frame edge-stress readout.
pub fn scale_normalized_stress_uniform(positions: &[f32], edges: &[(u32, u32)]) -> f32 {
    // d = 1, w = 1/d² = 1 for every edge ⇒ sum w*d² = edge count.
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &(i, j) in edges {
        let big = euclid(positions, i as usize, j as usize);
        num += big; // Σ w·d·D = Σ D
        den += big * big; // Σ w·D² = Σ D²
    }
    let m = edges.len() as f64;
    if den <= 0.0 || m <= 0.0 {
        return 0.0;
    }
    let alpha = num / den;
    let mut s = 0.0f64;
    for &(i, j) in edges {
        let r = alpha * euclid(positions, i as usize, j as usize) - 1.0;
        s += r * r;
    }
    (s / m) as f32
}

/// Scale-normalized stress over ALL node pairs, with target distances = graph
/// (hop) distances found by BFS from every node over `edges`. O(n²); the caller
/// gates by size. Unreachable / self pairs are skipped. Shared by the renderer's
/// on-demand "full stress" readout and the solver tests.
pub fn all_pairs_normalized_stress(positions: &[f32], edges: &[(u32, u32)], n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        let (a, b) = (a as usize, b as usize);
        if a < n && b < n {
            adj[a].push(b as u32);
            adj[b].push(a as u32);
        }
    }
    let mut terms: Vec<(u32, u32, f32)> = Vec::new();
    let mut dist = vec![u32::MAX; n];
    let mut q: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    for s in 0..n {
        for d in dist.iter_mut() {
            *d = u32::MAX;
        }
        dist[s] = 0;
        q.clear();
        q.push_back(s as u32);
        while let Some(v) = q.pop_front() {
            let dv = dist[v as usize];
            for &u in &adj[v as usize] {
                if dist[u as usize] == u32::MAX {
                    dist[u as usize] = dv + 1;
                    q.push_back(u);
                }
            }
        }
        for (t, &dst) in dist.iter().enumerate().skip(s + 1) {
            if dst != u32::MAX && dst != 0 {
                terms.push((s as u32, t as u32, dst as f32));
            }
        }
    }
    scale_normalized_stress(positions, &terms)
}

/// Coefficient of variation of edge lengths (`stddev / mean`) over `edges`.
/// `0` = all edges identical length (a common drawing-aesthetic goal); scale
/// invariant. Returns `0.0` for fewer than two edges or a degenerate mean.
pub fn edge_length_cv(positions: &[f32], edges: &[(u32, u32)]) -> f32 {
    if edges.len() < 2 {
        return 0.0;
    }
    let lens: Vec<f64> = edges
        .iter()
        .map(|&(i, j)| euclid(positions, i as usize, j as usize))
        .collect();
    let mean = lens.iter().sum::<f64>() / lens.len() as f64;
    if mean <= 1e-12 {
        return 0.0;
    }
    let var = lens.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lens.len() as f64;
    (var.sqrt() / mean) as f32
}

/// Number of pairs of edges whose 2D (xy) segments properly cross. Edges that
/// share an endpoint are never counted. O(E²) — intended for small/medium
/// graphs or on-demand use. Fewer crossings is the canonical readability
/// aesthetic (Purchase). Collinear/touching-only cases are treated as
/// non-crossing (strict proper-intersection test).
pub fn edge_crossings(positions: &[f32], edges: &[(u32, u32)]) -> u32 {
    fn pt(positions: &[f32], i: u32) -> (f64, f64) {
        (positions[3 * i as usize] as f64, positions[3 * i as usize + 1] as f64)
    }
    // Signed area of triangle (a, b, c); sign = orientation.
    fn ccw(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    }
    let mut count = 0u32;
    for i in 0..edges.len() {
        let (a0, a1) = edges[i];
        let (p1, p2) = (pt(positions, a0), pt(positions, a1));
        for &(b0, b1) in edges.iter().skip(i + 1) {
            if a0 == b0 || a0 == b1 || a1 == b0 || a1 == b1 {
                continue; // shared endpoint — adjacent, not a crossing
            }
            let (p3, p4) = (pt(positions, b0), pt(positions, b1));
            let d1 = ccw(p3, p4, p1);
            let d2 = ccw(p3, p4, p2);
            let d3 = ccw(p1, p2, p3);
            let d4 = ccw(p1, p2, p4);
            // Proper crossing: each segment straddles the other's line.
            if (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colinear_path_has_zero_normalized_stress() {
        // Path P_4 placed on the line at x = i: every Euclidean distance equals
        // the graph distance, so normalized stress is exactly 0.
        let pos = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0, 0.0, 0.0];
        let mut terms = Vec::new();
        for i in 0..4u32 {
            for j in (i + 1)..4 {
                terms.push((i, j, (j - i) as f32));
            }
        }
        assert!(scale_normalized_stress(&pos, &terms) < 1e-5);
    }

    #[test]
    fn normalized_stress_is_scale_invariant() {
        let pos = [0.0, 0.0, 0.0, 1.3, 0.4, 0.0, 2.1, -0.2, 0.0];
        let terms = [(0u32, 1u32, 1.0f32), (1, 2, 1.0), (0, 2, 2.0)];
        let base = scale_normalized_stress(&pos, &terms);
        // Scale every coordinate by 10× — normalized stress must not change.
        let scaled: Vec<f32> = pos.iter().map(|v| v * 10.0).collect();
        let big = scale_normalized_stress(&scaled, &terms);
        assert!((base - big).abs() < 1e-5, "not scale invariant: {base} vs {big}");
        // Raw stress, by contrast, IS scale-sensitive (sanity on the distinction).
        assert!((stress(&pos, &terms) - stress(&scaled, &terms)).abs() > 1e-3);
    }

    #[test]
    fn edge_length_cv_zero_for_uniform_edges() {
        // Unit triangle-ish: three edges of equal length → CV 0.
        let pos = [0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 1.0, 3.0f32.sqrt(), 0.0];
        let edges = [(0u32, 1u32), (1, 2), (2, 0)];
        assert!(edge_length_cv(&pos, &edges) < 1e-4);
    }

    #[test]
    fn crossing_diagonals_count_one() {
        // A(0,0) B(2,2) C(2,0) D(0,2): edges A-B and C-D are the two diagonals
        // of a square → they cross once, sharing no endpoint.
        let pos = [0.0, 0.0, 0.0, 2.0, 2.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0];
        let edges = [(0u32, 1u32), (2, 3)];
        assert_eq!(edge_crossings(&pos, &edges), 1);
    }

    #[test]
    fn parallel_edges_no_crossing() {
        // Two horizontal, vertically-offset segments — never cross.
        let pos = [0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 2.0, 1.0, 0.0];
        let edges = [(0u32, 1u32), (2, 3)];
        assert_eq!(edge_crossings(&pos, &edges), 0);
    }

    #[test]
    fn adjacent_edges_not_counted() {
        // Two edges sharing endpoint 0 — adjacent, never a crossing.
        let pos = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let edges = [(0u32, 1u32), (0, 2)];
        assert_eq!(edge_crossings(&pos, &edges), 0);
    }

    #[test]
    fn path_all_pairs_stress_is_zero() {
        // Colinear path 0-1-2-3: every Euclidean distance equals the hop
        // distance ⇒ all-pairs normalized stress is exactly 0.
        let pos = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0, 0.0, 0.0];
        let edges = [(0u32, 1u32), (1, 2), (2, 3)];
        assert!(all_pairs_normalized_stress(&pos, &edges, 4) < 1e-5);
    }

    #[test]
    fn uniform_matches_terms_with_unit_distance() {
        let pos = [0.0, 0.0, 0.0, 1.3, 0.4, 0.0, 2.1, -0.2, 0.0];
        let edges = [(0u32, 1u32), (1, 2)];
        let terms: Vec<(u32, u32, f32)> = edges.iter().map(|&(a, b)| (a, b, 1.0)).collect();
        let uniform = scale_normalized_stress_uniform(&pos, &edges);
        let viaterms = scale_normalized_stress(&pos, &terms);
        assert!((uniform - viaterms).abs() < 1e-5, "{uniform} vs {viaterms}");
    }
}
