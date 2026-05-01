//! App-level data state: bootstrap result + lazy node-meta cache.
//!
//! The fetch task on App::new populates `LoadState::Ready` with everything
//! the GPU pipelines need to upload buffers. The next App::update sees
//! `Ready` and hands the data to GraphPipelines::load.

use crate::proto;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Bootstrap {
    pub init: Option<proto::Init>,
    pub ids: Vec<String>,
    /// 3D positions, [x,y,z, ...]. Length = 3 * n_nodes.
    pub positions: Vec<f32>,
    /// Edge index pairs, [s,t, ...]. Length = 2 * n_edges.
    pub edges: Vec<u32>,
    /// Per-node metric f32 buffers, keyed by metric name.
    pub metrics: std::collections::HashMap<String, Vec<f32>>,
}

pub enum LoadState {
    Pending,
    Loading(String), // status text
    Ready(Bootstrap),
    /// Permanent error after retries — render falls through to placeholder.
    Failed(String),
}

impl Default for LoadState {
    fn default() -> Self {
        LoadState::Pending
    }
}

pub type SharedLoad = Arc<Mutex<LoadState>>;

/// Promote the server's flat 2D positions ([x,y,x,y,...]) into 3D with a
/// small random Z spread so the force sim doesn't trap nodes on a ring.
/// Returns [x,y,z, x,y,z, ...] of length 3 * n_nodes.
pub fn promote_2d_to_3d(positions_2d: &[f32], seed: u64) -> Vec<f32> {
    // xorshift64* — tiny deterministic PRNG, no rand dep.
    let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // map to [-1, 1)
        ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    };
    let n = positions_2d.len() / 2;
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n {
        out.push(positions_2d[i * 2]);
        out.push(positions_2d[i * 2 + 1]);
        // Spread across a few hundred world units in Z.
        out.push(next() * 200.0);
    }
    out
}

/// Default colour for every node before we apply metric-driven palette
/// choices (Phase D will introduce metric → palette wiring).
pub fn default_colors(n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n * 4);
    for _ in 0..n {
        // Pale white-blue with full alpha — readable on the dark egui panel.
        out.extend_from_slice(&[0.85, 0.90, 1.00, 1.0]);
    }
    out
}

/// Per-node screen-space radius in pixels.
pub fn default_sizes(n: usize) -> Vec<f32> {
    vec![3.0; n]
}

/// 12-entry palette used to cycle through community / wcc ids. RGB
/// triples in 0..1; alpha is appended downstream.
const PALETTE: [[f32; 3]; 12] = [
    [0.85, 0.90, 1.00],
    [0.95, 0.62, 0.40],
    [0.55, 0.80, 0.50],
    [0.95, 0.85, 0.40],
    [0.65, 0.55, 0.95],
    [0.40, 0.85, 0.85],
    [0.95, 0.55, 0.75],
    [0.80, 0.40, 0.40],
    [0.50, 0.65, 0.95],
    [0.95, 0.95, 0.95],
    [0.45, 0.95, 0.65],
    [0.95, 0.40, 0.95],
];

fn palette_color(idx: u32) -> [f32; 3] {
    PALETTE[(idx as usize) % PALETTE.len()]
}

/// Build an RGBA buffer of length `n*4` from a per-node metric. Sequential
/// metrics (degree, pagerank, …) get a viridis-ish gradient; categorical
/// metrics (community, wcc) get a palette cycle. Falls back to default if
/// the metric is missing.
pub fn colors_from_metric(
    metric_key: &str,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    n: usize,
) -> Vec<f32> {
    let Some(v) = metrics.get(metric_key) else {
        return default_colors(n);
    };
    if v.len() < n {
        return default_colors(n);
    }
    let categorical = matches!(metric_key, "community" | "wcc" | "doctype" | "folder");
    let mut out = Vec::with_capacity(n * 4);
    if categorical {
        for i in 0..n {
            let bucket = v[i].max(0.0) as u32;
            let c = palette_color(bucket);
            out.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    } else {
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for &x in &v[..n] {
            mn = mn.min(x);
            mx = mx.max(x);
        }
        let range = (mx - mn).max(1e-6);
        for i in 0..n {
            let t = ((v[i] - mn) / range).clamp(0.0, 1.0);
            // simple 3-stop gradient: deep blue -> teal -> warm white
            let r = 0.20 + 0.75 * t;
            let g = 0.30 + 0.55 * t;
            let b = 0.95 - 0.55 * t;
            out.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    out
}

/// Build a per-node sizes buffer from a metric, scaled by `multiplier`.
///
/// Sizes are computed via per-graph min/max normalization followed by a
/// sqrt curve so hubs are visually distinct from leaves without dwarfing
/// them. Pixel radius range at mul=1: 2..=10 px.
///
/// "uniform" and "recency" (no server metric yet) both fall back to a flat
/// 4 px × multiplier.
pub fn sizes_from_metric(
    metric_key: &str,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    n: usize,
    multiplier: f32,
) -> Vec<f32> {
    let mul = multiplier.max(0.0);
    // Uniform sentinel and recency (no metric available yet) → flat size.
    if metric_key == "uniform" || metric_key == "recency" {
        return vec![4.0 * mul; n];
    }
    let Some(v) = metrics.get(metric_key) else {
        return vec![4.0 * mul; n];
    };
    if v.len() < n {
        return vec![4.0 * mul; n];
    }
    // Per-graph min/max over the raw metric values.
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &x in &v[..n] {
        if x.is_finite() {
            if x < min { min = x; }
            if x > max { max = x; }
        }
    }
    if !min.is_finite() || min == max {
        return vec![4.0 * mul; n];
    }
    let span = max - min;
    let mut out = Vec::with_capacity(n);
    for &x in v.iter().take(n) {
        let x = if x.is_finite() { x } else { min };
        // Normalize to [0, 1].
        let t = ((x - min) / span).clamp(0.0, 1.0);
        // Sqrt scaling: compresses the high end so hubs aren't 100× the
        // size of leaves while still being visibly larger.
        let scaled = t.sqrt();
        // Pixel radius: 2..=10 at mul=1.
        let r = (2.0 + scaled * 8.0) * mul;
        out.push(r);
    }
    out
}
