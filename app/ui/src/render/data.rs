//! Node style buffers + initial-position seeding, ported from
//! `crates/graph-renderer/src/data.rs`.
//!
//! Only the pieces the Dioxus frontend actually drives are here: the
//! Fibonacci sphere spawn, the default Tableau20 palette (kept in sync with
//! `vault-data::color::PALETTE`, i.e. what the server announces in
//! `/graph/init.palette`), and the metric → color/size buffer builders. The
//! full PaletteId catalogue stays in the egui app until a Style panel lands
//! here.

use std::collections::HashMap;

/// Spawn `n` nodes on the surface of a centred sphere of `radius`, using
/// the Fibonacci lattice (golden-angle spiral). Returns
/// [x,y,z, x,y,z, ...] of length `3 * n`.
///
/// Why Fibonacci over rejection-sampled uniform: deterministic (no PRNG
/// → identical output for identical n), O(n) with no acceptance/rejection,
/// and visually clean (no pole clusters, no axis-aligned artifacts). The
/// sim takes over within a few frames so the only thing this seed has to
/// do is give the force kernels a non-degenerate, isotropic starting set.
pub fn spawn_on_unit_sphere(n: usize, radius: f32) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    // Golden angle in radians: π * (3 - √5).
    let phi_golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let mut out = Vec::with_capacity(n * 3);
    let n_f = n as f32;
    for i in 0..n {
        let i_f = i as f32;
        // Polar angle: equally-spaced cosines on [-1, 1] → uniform in z.
        let cos_phi = 1.0 - 2.0 * (i_f + 0.5) / n_f;
        let sin_phi = (1.0 - cos_phi * cos_phi).max(0.0).sqrt();
        let theta = phi_golden * i_f;
        out.push(sin_phi * theta.cos() * radius);
        out.push(sin_phi * theta.sin() * radius);
        out.push(cos_phi * radius);
    }
    out
}

/// Default colour for every node when the metric buffer is missing.
pub fn default_colors(n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n * 4);
    for _ in 0..n {
        // Pale white-blue with full alpha — readable on the dark panel.
        out.extend_from_slice(&[0.85, 0.90, 1.00, 1.0]);
    }
    out
}

/// d3 / Tableau20 categorical — 20 stops. Matches the server-sent
/// `Init.palette` so canvas swatches line up with the egui renderer's
/// default `PaletteId::Tableau20`.
const PAL_TABLEAU20: [[f32; 3]; 20] = [
    [0.122, 0.471, 0.706],
    [0.682, 0.780, 0.910],
    [1.000, 0.498, 0.055],
    [1.000, 0.733, 0.471],
    [0.173, 0.627, 0.173],
    [0.596, 0.875, 0.541],
    [0.839, 0.153, 0.157],
    [1.000, 0.596, 0.588],
    [0.580, 0.404, 0.741],
    [0.773, 0.690, 0.835],
    [0.549, 0.337, 0.294],
    [0.769, 0.612, 0.580],
    [0.890, 0.467, 0.761],
    [0.969, 0.714, 0.824],
    [0.498, 0.498, 0.498],
    [0.780, 0.780, 0.780],
    [0.737, 0.741, 0.133],
    [0.859, 0.859, 0.553],
    [0.090, 0.745, 0.812],
    [0.620, 0.855, 0.898],
];

pub fn palette_color(idx: u32) -> [f32; 3] {
    PAL_TABLEAU20[(idx as usize) % PAL_TABLEAU20.len()]
}

/// Build an RGBA buffer of length `n*4` from a per-node metric. Sequential
/// metrics (degree, pagerank, …) get a viridis-ish gradient; categorical
/// metrics (community, wcc) get a palette cycle. Falls back to default if
/// the metric is missing.
pub fn colors_from_metric(
    metric_key: &str,
    metrics: &HashMap<String, Vec<f32>>,
    n: usize,
) -> Vec<f32> {
    let Some(v) = metrics.get(metric_key) else {
        return default_colors(n);
    };
    if v.len() < n {
        return default_colors(n);
    }
    let categorical = matches!(metric_key, "community" | "wcc" | "doctype" | "folder" | "tag");
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
/// them. Pixel radius range at mul=1: 2..=10 px. Mirrors the egui app's
/// defaults: SizeBy::PageRank at `size_mul = 0.5`.
pub fn sizes_from_metric(
    metric_key: &str,
    metrics: &HashMap<String, Vec<f32>>,
    n: usize,
    multiplier: f32,
) -> Vec<f32> {
    let mul = multiplier.max(0.0);
    if metric_key == "uniform" || metric_key == "recency" {
        return vec![4.0 * mul; n];
    }
    let Some(v) = metrics.get(metric_key) else {
        return vec![4.0 * mul; n];
    };
    if v.len() < n {
        return vec![4.0 * mul; n];
    }
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &x in &v[..n] {
        if x.is_finite() {
            min = min.min(x);
            max = max.max(x);
        }
    }
    if !min.is_finite() || min == max {
        return vec![4.0 * mul; n];
    }
    let span = max - min;
    let mut out = Vec::with_capacity(n);
    for &x in v.iter().take(n) {
        let x = if x.is_finite() { x } else { min };
        let t = ((x - min) / span).clamp(0.0, 1.0);
        // Sqrt scaling: compresses the high end so hubs aren't 100× the
        // size of leaves while still being visibly larger.
        let scaled = t.sqrt();
        // Pixel radius: 2..=10 at mul=1.
        out.push((2.0 + scaled * 8.0) * mul);
    }
    out
}
