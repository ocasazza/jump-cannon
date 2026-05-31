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
}
