//! Radial distortion around the focal node (paper §6).
//!
//! The hybrid graph integrates several scales: the focus region is dense
//! (taken from the finest level) and the periphery is sparse (taken from
//! coarsened levels). Paper §6 fixes this by **radial distortion**: every
//! node is re-placed in polar coordinates centred on the focus, with the
//! angle preserved and the radius re-mapped through a monotone function
//! `F(r)` chosen so the resulting layout has uniform local density.
//!
//! The density at radius `r_i` is estimated as the average length `d_i`
//! of edges adjacent to node `i` in the RNG-approximation of the hybrid
//! layout (paper §6: "in dense regions the RNG edges will be shorter"),
//! locally smoothed over a sliding window of `2p` nodes (paper uses
//! `p = 20`).
//!
//! The recurrence is
//!
//! ```text
//!   F(r_0) = 0
//!   F(r_i) = F(r_{i-1}) + (|Δ_i| / d_{Δ_i})^α
//! ```
//!
//! where `|Δ_i| = r_i − r_{i-1}`. `α = 1` gives uniform density; `α > 1`
//! over-emphasises the focal region (geometric-fisheye behaviour).

use std::cmp::Ordering;
use std::collections::HashSet;

use super::types::HybridGraph;

#[derive(Clone, Copy, Debug)]
pub struct DistortParams {
    /// Paper's α. `0.0` disables distortion; `1.0` targets uniform density;
    /// values > 1 over-emphasise the focal region (geometric-fisheye look).
    pub alpha: f32,
    /// World-space XY of the focus (typically the position of the focal
    /// node before any distortion).
    pub focus_xy: [f32; 2],
    /// Smoothing half-window `p` (paper uses 20). Clamped to `n/2` so the
    /// distortion still works on small graphs.
    pub smoothing_window: usize,
}

impl Default for DistortParams {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            focus_xy: [0.0, 0.0],
            smoothing_window: 20,
        }
    }
}

pub fn distort_radial(g: &mut HybridGraph, p: &DistortParams) {
    let n = g.nodes.len();
    if n < 2 || p.alpha <= 0.0 {
        return;
    }
    let cx = p.focus_xy[0];
    let cy = p.focus_xy[1];

    // Polar form per node.
    struct Polar {
        idx: usize,
        r: f32,
        theta: f32,
        z: f32,
    }
    let polars_unsorted: Vec<Polar> = (0..n)
        .map(|i| {
            let x = g.positions[3 * i] - cx;
            let y = g.positions[3 * i + 1] - cy;
            Polar {
                idx: i,
                r: (x * x + y * y).sqrt(),
                theta: y.atan2(x),
                z: g.positions[3 * i + 2],
            }
        })
        .collect();

    // RNG-approximation edges over the hybrid layout.
    let rng = rng_approximation_edges(&g.positions, n);

    // Per-node average RNG edge length d_i; fall back to the global median
    // when a node has no RNG neighbours (paper doesn't specify; this keeps
    // F(r) finite).
    let mut adj_sum = vec![0.0f32; n];
    let mut adj_cnt = vec![0u32; n];
    let mut all_lens = Vec::with_capacity(rng.len());
    for &(a, b) in &rng {
        let dx = g.positions[3 * a as usize] - g.positions[3 * b as usize];
        let dy = g.positions[3 * a as usize + 1] - g.positions[3 * b as usize + 1];
        let len = (dx * dx + dy * dy).sqrt();
        all_lens.push(len);
        adj_sum[a as usize] += len;
        adj_cnt[a as usize] += 1;
        adj_sum[b as usize] += len;
        adj_cnt[b as usize] += 1;
    }
    let median = if all_lens.is_empty() {
        1.0
    } else {
        all_lens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        all_lens[all_lens.len() / 2].max(1e-6)
    };
    let d_i: Vec<f32> = (0..n)
        .map(|i| {
            if adj_cnt[i] > 0 {
                adj_sum[i] / adj_cnt[i] as f32
            } else {
                median
            }
        })
        .collect();

    // Sort by ascending radius and gather d_i in the same order.
    let mut polars = polars_unsorted;
    polars.sort_by(|a, b| a.r.partial_cmp(&b.r).unwrap_or(Ordering::Equal));
    let d_sorted: Vec<f32> = polars.iter().map(|p| d_i[p.idx]).collect();
    let r_sorted: Vec<f32> = polars.iter().map(|p| p.r).collect();

    // Windowed-averaged density d_{Δ_i}.
    let p_win = p.smoothing_window.min(n / 2).max(1);
    let d_delta = |i: usize| -> f32 {
        let lo = i.saturating_sub(p_win);
        let hi = (i + p_win).min(n);
        let count = (hi - lo) as f32;
        let sum: f32 = d_sorted[lo..hi].iter().sum();
        (sum / count.max(1.0)).max(1e-6)
    };

    // F(r) recurrence. The paper takes `r_0 = 0` (the focal node coincides
    // with the origin); we generalise by using a virtual r=0 / F=0 anchor
    // so the closest node can have a non-zero radius even when no hybrid
    // node sits exactly on the focus.
    let mut f_r = vec![0.0f32; n];
    let alpha = p.alpha;
    let mut prev_r = 0.0f32;
    let mut prev_f = 0.0f32;
    for i in 0..n {
        let delta = (r_sorted[i] - prev_r).max(0.0);
        let term = (delta / d_delta(i)).powf(alpha);
        prev_f += term;
        prev_r = r_sorted[i];
        f_r[i] = prev_f;
    }

    // Rescale F so the *outermost* node keeps its original radius. This
    // avoids the layout collapsing or exploding depending on the units of
    // `d_i`. Angle is preserved; only radii are remapped.
    let scale = if f_r[n - 1] > 0.0 {
        r_sorted[n - 1] / f_r[n - 1]
    } else {
        1.0
    };

    for (rank, p) in polars.iter().enumerate() {
        let r = f_r[rank] * scale;
        g.positions[3 * p.idx] = cx + r * p.theta.cos();
        g.positions[3 * p.idx + 1] = cy + r * p.theta.sin();
        g.positions[3 * p.idx + 2] = p.z;
    }
}

/// Paper §4's RNG-approximation: build the Delaunay triangulation, then
/// drop any edge `⟨i,j⟩` for which some DT-neighbour `k` of `i` or `j`
/// satisfies `‖p_i − p_j‖ > min(‖p_i − p_k‖, ‖p_j − p_k‖)`. The result is
/// contained in the DT and contains the true RNG.
pub(crate) fn rng_approximation_edges(positions: &[f32], n: usize) -> Vec<(u32, u32)> {
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
    let mut dt_adj: Vec<HashSet<u32>> = vec![HashSet::new(); n];
    let mut i = 0;
    while i + 2 < triangles.len() {
        let a = triangles[i] as u32;
        let b = triangles[i + 1] as u32;
        let c = triangles[i + 2] as u32;
        i += 3;
        dt_adj[a as usize].insert(b);
        dt_adj[b as usize].insert(a);
        dt_adj[b as usize].insert(c);
        dt_adj[c as usize].insert(b);
        dt_adj[a as usize].insert(c);
        dt_adj[c as usize].insert(a);
    }
    let dist = |a: u32, b: u32| -> f32 {
        let dx = positions[3 * a as usize] - positions[3 * b as usize];
        let dy = positions[3 * a as usize + 1] - positions[3 * b as usize + 1];
        (dx * dx + dy * dy).sqrt()
    };
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (ii, neigh) in dt_adj.iter().enumerate() {
        for &j in neigh {
            if (ii as u32) >= j {
                continue;
            }
            let dij = dist(ii as u32, j);
            let mut keep = true;
            // Paper: "some k adjacent to i or j (in the DT)"
            for &k in dt_adj[ii].iter().chain(dt_adj[j as usize].iter()) {
                if k == ii as u32 || k == j {
                    continue;
                }
                let dik = dist(ii as u32, k);
                let djk = dist(j, k);
                if dij > dik.min(djk) {
                    keep = false;
                    break;
                }
            }
            if keep {
                out.push((ii as u32, j));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topo_fisheye::types::HybridGraph;

    fn synthetic_dense_center() -> HybridGraph {
        // 9 nodes: 8 clustered tightly near the focus + 1 far out. Distortion
        // should spread out the cluster.
        let mut positions = Vec::new();
        for i in 0..8 {
            let t = (i as f32) / 8.0 * std::f32::consts::TAU;
            positions.extend_from_slice(&[0.5 * t.cos(), 0.5 * t.sin(), 0.0]);
        }
        positions.extend_from_slice(&[100.0, 0.0, 0.0]);
        HybridGraph {
            nodes: (0..9).map(|i| (0u32, i as u32)).collect(),
            positions,
            edges: Vec::new(),
            edge_levels: Vec::new(),
            node_levels: vec![0; 9],
        }
    }

    #[test]
    fn distort_preserves_radial_order() {
        let mut g = synthetic_dense_center();
        let original_radii: Vec<f32> = (0..g.nodes.len())
            .map(|i| {
                let x = g.positions[3 * i];
                let y = g.positions[3 * i + 1];
                (x * x + y * y).sqrt()
            })
            .collect();
        let mut order: Vec<usize> = (0..g.nodes.len()).collect();
        order.sort_by(|&a, &b| original_radii[a].partial_cmp(&original_radii[b]).unwrap());
        distort_radial(&mut g, &DistortParams::default());
        let new_radii: Vec<f32> = (0..g.nodes.len())
            .map(|i| {
                let x = g.positions[3 * i];
                let y = g.positions[3 * i + 1];
                (x * x + y * y).sqrt()
            })
            .collect();
        // Radial ranks must be preserved (monotone F).
        for w in order.windows(2) {
            let a = w[0];
            let b = w[1];
            assert!(
                new_radii[a] <= new_radii[b] + 1e-3,
                "radial order changed: orig={:?} new={:?}",
                original_radii,
                new_radii
            );
        }
    }

    #[test]
    fn distort_preserves_angles() {
        let mut g = synthetic_dense_center();
        let pre_angles: Vec<f32> = (0..g.nodes.len())
            .map(|i| g.positions[3 * i + 1].atan2(g.positions[3 * i]))
            .collect();
        distort_radial(&mut g, &DistortParams::default());
        let post_angles: Vec<f32> = (0..g.nodes.len())
            .map(|i| g.positions[3 * i + 1].atan2(g.positions[3 * i]))
            .collect();
        for i in 0..g.nodes.len() {
            // Angles within a tiny ULP — only float error on cos/sin round-trip.
            let diff = (pre_angles[i] - post_angles[i]).abs();
            assert!(
                diff < 1e-3,
                "angle drift at {i}: {} vs {}",
                pre_angles[i],
                post_angles[i]
            );
        }
    }

    #[test]
    fn alpha_zero_is_noop() {
        let mut g = synthetic_dense_center();
        let before = g.positions.clone();
        distort_radial(
            &mut g,
            &DistortParams {
                alpha: 0.0,
                ..Default::default()
            },
        );
        assert_eq!(before, g.positions);
    }
}
