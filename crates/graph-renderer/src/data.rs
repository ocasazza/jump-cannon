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

/// Spawn `n` nodes on the surface of a centred sphere of `radius`, using
/// the Fibonacci lattice (golden-angle spiral). Returns
/// [x,y,z, x,y,z, ...] of length `3 * n`.
///
/// Why Fibonacci over rejection-sampled uniform: deterministic (no PRNG
/// → identical output for identical n, important for screenshot
/// regression tests), O(n) with no acceptance/rejection, and visually
/// clean (no pole clusters, no axis-aligned artifacts). The sim takes
/// over within a few frames so the only thing this seed has to do is
/// give the force kernels a non-degenerate, isotropic starting set.
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
        let x = sin_phi * theta.cos() * radius;
        let y = sin_phi * theta.sin() * radius;
        let z = cos_phi * radius;
        out.push(x);
        out.push(y);
        out.push(z);
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
///
/// 2.25 px tracks the 25%-smaller default node size (was 3.0; the
/// `StyleState::size_mul` default dropped 0.67 → 0.5 in the same change).
pub fn default_sizes(n: usize) -> Vec<f32> {
    vec![2.25; n]
}

/// Categorical palette identifier. Every option is at least 10 stops
/// long so it can cycle visibly without short-period collisions on
/// graphs with many communities.
///
/// Default is `Tableau20` — kept in sync with `vault-data::color::PALETTE`
/// so renderer-side colours match what the server announces in
/// `/graph/init.palette`. The other entries are categorical or
/// CVD-safe scientific palettes the Style sidebar can switch between.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum PaletteId {
    /// d3 / Tableau20 categorical — 20 stops. Matches the server-sent
    /// `Init.palette` so canvas swatches line up with badge tints
    /// pre-style-pick. Default.
    #[default]
    Tableau20,
    /// Okabe-Ito 8-color universally CVD-safe palette, padded with two
    /// neutral muted greys to reach 10 stops.
    OkabeIto,
    /// ColorBrewer Set1 (9 stops) padded to 10. Qualitative; not
    /// fully CVD-safe but a long-standing categorical default.
    ColorBrewerSet1,
    /// ColorBrewer Dark2 (8 stops) padded to 10. Qualitative and
    /// listed by Brewer as colour-blind-friendly.
    ColorBrewerDark2,
    /// Viridis sequential, 12 evenly-spaced stops. CVD-safe and
    /// perceptually uniform.
    Viridis,
    /// Plasma sequential, 12 evenly-spaced stops. CVD-safe and
    /// perceptually uniform.
    Plasma,
    /// Inspired by published figures from Schrödinger Inc. marketing
    /// material and JACS-style supplementary figures (muted scientific
    /// blues / corporate teal); not an official brand asset. 10 stops.
    SchrodingerCorporate,
    /// Inspired by published figures from Schrödinger Inc. JACS-style
    /// supplementary figures (earthy greens / muted oranges / clay
    /// browns); not an official brand asset. 10 stops.
    SchrodingerScientific,
    /// 12-stop near-monochrome cycle from #1A1A1A to #F0F0F0 with
    /// small hue shifts so adjacent buckets remain distinguishable on
    /// a B&W display.
    Monochrome,
    /// 10-stop single-hue desaturated blue cycle for monotone
    /// presentation contexts.
    MonoSingleHue,
    /// 10-stop ink-on-paper grayscale: off-white #E8E0D0 down to
    /// charcoal #2A2520.
    PaperGrayscale,
}

impl PaletteId {
    pub const ALL: &'static [PaletteId] = &[
        PaletteId::Tableau20,
        PaletteId::OkabeIto,
        PaletteId::ColorBrewerSet1,
        PaletteId::ColorBrewerDark2,
        PaletteId::Viridis,
        PaletteId::Plasma,
        PaletteId::SchrodingerCorporate,
        PaletteId::SchrodingerScientific,
        PaletteId::Monochrome,
        PaletteId::MonoSingleHue,
        PaletteId::PaperGrayscale,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaletteId::Tableau20 => "Tableau 20",
            PaletteId::OkabeIto => "Okabe-Ito (CVD)",
            PaletteId::ColorBrewerSet1 => "ColorBrewer Set1",
            PaletteId::ColorBrewerDark2 => "ColorBrewer Dark2 (CVD)",
            PaletteId::Viridis => "Viridis (CVD)",
            PaletteId::Plasma => "Plasma (CVD)",
            PaletteId::SchrodingerCorporate => "Schrodinger Corporate",
            PaletteId::SchrodingerScientific => "Schrodinger Scientific",
            PaletteId::Monochrome => "Monochrome",
            PaletteId::MonoSingleHue => "Mono Single-Hue",
            PaletteId::PaperGrayscale => "Paper Grayscale",
        }
    }
}

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

// Okabe-Ito 8 + 2 muted neutral pads.
const PAL_OKABE_ITO: [[f32; 3]; 10] = [
    [0.000, 0.000, 0.000],
    [0.902, 0.624, 0.000],
    [0.337, 0.706, 0.914],
    [0.000, 0.620, 0.451],
    [0.941, 0.894, 0.259],
    [0.000, 0.447, 0.698],
    [0.835, 0.369, 0.000],
    [0.800, 0.475, 0.655],
    [0.498, 0.498, 0.498],
    [0.733, 0.733, 0.733],
];

// ColorBrewer Set1 9 + 1 neutral pad.
const PAL_BREWER_SET1: [[f32; 3]; 10] = [
    [0.894, 0.102, 0.110],
    [0.216, 0.494, 0.722],
    [0.302, 0.686, 0.290],
    [0.596, 0.306, 0.639],
    [1.000, 0.498, 0.000],
    [1.000, 1.000, 0.200],
    [0.651, 0.337, 0.157],
    [0.969, 0.506, 0.749],
    [0.600, 0.600, 0.600],
    [0.337, 0.337, 0.337],
];

// ColorBrewer Dark2 8 + 2 neutral pads.
const PAL_BREWER_DARK2: [[f32; 3]; 10] = [
    [0.106, 0.620, 0.467],
    [0.851, 0.373, 0.008],
    [0.459, 0.439, 0.702],
    [0.906, 0.161, 0.541],
    [0.400, 0.651, 0.118],
    [0.902, 0.671, 0.008],
    [0.651, 0.463, 0.114],
    [0.400, 0.400, 0.400],
    [0.700, 0.700, 0.700],
    [0.250, 0.250, 0.250],
];

// Viridis 12 evenly-spaced samples (matplotlib reference).
const PAL_VIRIDIS: [[f32; 3]; 12] = [
    [0.267, 0.005, 0.329],
    [0.282, 0.100, 0.421],
    [0.254, 0.265, 0.530],
    [0.207, 0.372, 0.553],
    [0.164, 0.471, 0.558],
    [0.128, 0.567, 0.551],
    [0.135, 0.659, 0.518],
    [0.267, 0.749, 0.441],
    [0.478, 0.821, 0.318],
    [0.741, 0.873, 0.150],
    [0.993, 0.906, 0.144],
    [0.993, 0.906, 0.144],
];

// Plasma 12 evenly-spaced samples.
const PAL_PLASMA: [[f32; 3]; 12] = [
    [0.050, 0.029, 0.528],
    [0.226, 0.005, 0.616],
    [0.397, 0.001, 0.652],
    [0.554, 0.047, 0.645],
    [0.690, 0.165, 0.564],
    [0.798, 0.281, 0.469],
    [0.881, 0.392, 0.383],
    [0.945, 0.514, 0.298],
    [0.987, 0.652, 0.211],
    [0.992, 0.804, 0.146],
    [0.940, 0.975, 0.131],
    [0.940, 0.975, 0.131],
];

// Schrödinger Corporate — muted scientific blues / corporate teal.
const PAL_SCHRO_CORPORATE: [[f32; 3]; 10] = [
    [0.106, 0.255, 0.404], // deep navy
    [0.176, 0.388, 0.553], // corporate blue
    [0.282, 0.541, 0.682], // mid blue
    [0.420, 0.659, 0.745], // sky
    [0.169, 0.475, 0.467], // teal
    [0.318, 0.604, 0.553], // muted teal
    [0.494, 0.706, 0.671], // pale teal
    [0.624, 0.624, 0.643], // cool grey
    [0.376, 0.420, 0.482], // slate
    [0.165, 0.220, 0.290], // ink
];

// Schrödinger Scientific — earthy greens, muted oranges, clay browns.
const PAL_SCHRO_SCIENTIFIC: [[f32; 3]; 10] = [
    [0.314, 0.451, 0.243], // moss
    [0.490, 0.604, 0.349], // sage
    [0.663, 0.722, 0.420], // olive
    [0.769, 0.604, 0.345], // wheat
    [0.804, 0.451, 0.224], // muted orange
    [0.722, 0.345, 0.196], // burnt sienna
    [0.561, 0.318, 0.227], // clay
    [0.400, 0.275, 0.220], // umber
    [0.275, 0.224, 0.176], // dark earth
    [0.529, 0.510, 0.420], // stone
];

// Monochrome — 12 stops between #1A1A1A and #F0F0F0 with small hue
// shifts (slight blue/green/red bias on adjacent buckets) so neighbours
// remain distinguishable even on B&W displays.
const PAL_MONOCHROME: [[f32; 3]; 12] = [
    [0.102, 0.102, 0.110], // #1A1A1C
    [0.180, 0.176, 0.184],
    [0.255, 0.247, 0.243],
    [0.329, 0.325, 0.337],
    [0.404, 0.404, 0.392],
    [0.478, 0.471, 0.486],
    [0.553, 0.553, 0.541],
    [0.627, 0.620, 0.635],
    [0.702, 0.706, 0.694],
    [0.776, 0.769, 0.784],
    [0.851, 0.855, 0.843],
    [0.941, 0.941, 0.941], // #F0F0F0
];

// Single-hue desaturated blue cycle, 10 stops.
const PAL_MONO_SINGLE_HUE: [[f32; 3]; 10] = [
    [0.090, 0.137, 0.184],
    [0.157, 0.227, 0.298],
    [0.220, 0.314, 0.404],
    [0.294, 0.396, 0.498],
    [0.376, 0.475, 0.580],
    [0.459, 0.553, 0.659],
    [0.541, 0.624, 0.725],
    [0.624, 0.694, 0.788],
    [0.706, 0.761, 0.843],
    [0.792, 0.831, 0.894],
];

// Paper grayscale — off-white #E8E0D0 down to charcoal #2A2520, 10 stops.
const PAL_PAPER_GRAYSCALE: [[f32; 3]; 10] = [
    [0.910, 0.878, 0.816], // #E8E0D0
    [0.835, 0.804, 0.745],
    [0.757, 0.729, 0.671],
    [0.682, 0.651, 0.596],
    [0.604, 0.576, 0.525],
    [0.529, 0.502, 0.451],
    [0.451, 0.424, 0.380],
    [0.376, 0.349, 0.306],
    [0.298, 0.275, 0.235],
    [0.165, 0.145, 0.125], // #2A2520
];

/// Lookup the static palette table for `id`. Tables are at least 10
/// stops long; the consumer is responsible for `idx % len()` cycling.
pub fn palette_table(id: PaletteId) -> &'static [[f32; 3]] {
    match id {
        PaletteId::Tableau20 => &PAL_TABLEAU20,
        PaletteId::OkabeIto => &PAL_OKABE_ITO,
        PaletteId::ColorBrewerSet1 => &PAL_BREWER_SET1,
        PaletteId::ColorBrewerDark2 => &PAL_BREWER_DARK2,
        PaletteId::Viridis => &PAL_VIRIDIS,
        PaletteId::Plasma => &PAL_PLASMA,
        PaletteId::SchrodingerCorporate => &PAL_SCHRO_CORPORATE,
        PaletteId::SchrodingerScientific => &PAL_SCHRO_SCIENTIFIC,
        PaletteId::Monochrome => &PAL_MONOCHROME,
        PaletteId::MonoSingleHue => &PAL_MONO_SINGLE_HUE,
        PaletteId::PaperGrayscale => &PAL_PAPER_GRAYSCALE,
    }
}

pub fn palette_color(idx: u32, id: PaletteId) -> [f32; 3] {
    let table = palette_table(id);
    table[(idx as usize) % table.len()]
}

/// Resolve a community id to its `egui::Color32` slot in the chosen
/// palette. Public so the inspector / modal can tint badges to match
/// the focused node's community swatch in the canvas.
pub fn community_color(community_id: u32, palette: PaletteId) -> eframe::egui::Color32 {
    let [r, g, b] = palette_color(community_id, palette);
    eframe::egui::Color32::from_rgb(
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Resolve the canvas-rendered colour for a single node under the active
/// `color_by` metric. Mirrors the per-index branch of
/// [`colors_from_metric`] so the inspector + modal can tint metadata
/// badges with the same swatch the node wears on the canvas.
///
/// Returns `None` when the metric is missing or `node_idx` is out of
/// range — consumers fall back to the per-kind palette.
pub fn node_color_for_key(
    metric_key: &str,
    node_idx: u32,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    palette: PaletteId,
) -> Option<eframe::egui::Color32> {
    let v = metrics.get(metric_key)?;
    let i = node_idx as usize;
    if i >= v.len() {
        return None;
    }
    let categorical = matches!(metric_key, "community" | "wcc" | "doctype" | "folder" | "tag");
    let (r, g, b) = if categorical {
        let bucket = v[i].max(0.0) as u32;
        let c = palette_color(bucket, palette);
        (c[0], c[1], c[2])
    } else {
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for &x in v.iter() {
            if x.is_finite() {
                mn = mn.min(x);
                mx = mx.max(x);
            }
        }
        if !mn.is_finite() {
            return None;
        }
        let range = (mx - mn).max(1e-6);
        let t = ((v[i] - mn) / range).clamp(0.0, 1.0);
        let r = 0.20 + 0.75 * t;
        let g = 0.30 + 0.55 * t;
        let b = 0.95 - 0.55 * t;
        (r, g, b)
    };
    Some(eframe::egui::Color32::from_rgb(
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ))
}

/// Build an RGBA buffer of length `n*4` from a per-node metric. Sequential
/// metrics (degree, pagerank, …) get a viridis-ish gradient; categorical
/// metrics (community, wcc) get a palette cycle. Falls back to default if
/// the metric is missing.
pub fn colors_from_metric(
    metric_key: &str,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    n: usize,
    palette: PaletteId,
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
            let c = palette_color(bucket, palette);
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

/// Build a per-edge RGBA buffer (length `m*4`) from a categorical
/// node-side metric. For each edge `(s, t)`:
///   - if `metric[s] == metric[t]` (same community/folder/doctype):
///     the edge gets that bucket's palette swatch with `fallback.a` alpha
///     preserved from the uniform setting.
///   - if the endpoints differ (a "bridging" edge): the edge falls back
///     to `fallback` (the user's uniform `edge_color`). This keeps
///     cross-community edges neutral so the community swatches read as
///     the dominant signal.
///
/// Falls back to a buffer of `fallback` for every edge if the metric is
/// missing or undersized.
pub fn edge_colors_from_metric(
    metric_key: &str,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    n_nodes: usize,
    edges: &[u32],
    fallback: [f32; 4],
    palette: PaletteId,
) -> Vec<f32> {
    let m = edges.len() / 2;
    let mut out = Vec::with_capacity(m * 4);
    let metric = metrics.get(metric_key);
    let usable = matches!(metric, Some(v) if v.len() >= n_nodes);
    if !usable {
        for _ in 0..m {
            out.extend_from_slice(&fallback);
        }
        return out;
    }
    let v = metric.unwrap();
    for chunk in edges.chunks_exact(2) {
        let s = chunk[0] as usize;
        let t = chunk[1] as usize;
        if s >= n_nodes || t >= n_nodes {
            out.extend_from_slice(&fallback);
            continue;
        }
        let bs = v[s].max(0.0) as u32;
        let bt = v[t].max(0.0) as u32;
        if bs == bt {
            let c = palette_color(bs, palette);
            out.extend_from_slice(&[c[0], c[1], c[2], fallback[3]]);
        } else {
            out.extend_from_slice(&fallback);
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

/// Number of distinct sprite primitives the node fragment shader knows
/// how to draw. Kept small (5) so that even on graphs with many
/// communities/folders each glyph stays visually distinct rather than
/// degenerating into "lots of similar quads". The current intended
/// mapping is:
///
/// | shape-id | primitive  |
/// |----------|------------|
/// | 0        | circle     |
/// | 1        | square     |
/// | 2        | triangle   |
/// | 3        | diamond    |
/// | 4        | hexagon    |
///
/// Consumed by [`shapes_from_metric`]; the WGSL switch on shape-id is
/// the GPU-side follow-up (see report).
pub const N_NODE_SHAPES: u32 = 5;

/// Build a per-node shape-id buffer (`u32`-per-node) from a categorical
/// metric. Each distinct bucket gets `bucket % N_NODE_SHAPES`, so e.g.
/// markdown=0 (circle), code=1 (square), image=2 (triangle), … under
/// `ShapeBy::Doctype`.
///
/// `"uniform"` short-circuits to all-zero (every node a disc). Missing
/// or undersized metric → all zeros (graceful: matches today's disc
/// behaviour). Non-finite values are clamped to bucket 0.
///
/// Returning `Vec<u32>` (rather than packing into the existing colors /
/// sizes buffers) keeps the data flow explicit on the CPU side. The
/// GPU-side follow-up can either:
///   (a) upload it as a fifth storage buffer (puts node pipeline at
///       binding(6) — within the 10-buffer per-stage cap, see
///       `2425ca20`'s headroom note), or
///   (b) pack low-byte of an existing `u32` style attribute, which
///       requires widening one of the f32 buffers first; not worth it
///       at this graph size.
pub fn shapes_from_metric(
    metric_key: &str,
    metrics: &std::collections::HashMap<String, Vec<f32>>,
    n: usize,
) -> Vec<u32> {
    if metric_key == "uniform" {
        return vec![0u32; n];
    }
    let Some(v) = metrics.get(metric_key) else {
        return vec![0u32; n];
    };
    if v.len() < n {
        return vec![0u32; n];
    }
    let mut out = Vec::with_capacity(n);
    for &x in v.iter().take(n) {
        let bucket = if x.is_finite() { x.max(0.0) as u32 } else { 0 };
        out.push(bucket % N_NODE_SHAPES);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_from_metric_uniform_is_all_zero() {
        let metrics = std::collections::HashMap::new();
        let out = shapes_from_metric("uniform", &metrics, 8);
        assert_eq!(out, vec![0; 8]);
    }

    #[test]
    fn shapes_from_metric_missing_metric_falls_back_to_zero() {
        let metrics = std::collections::HashMap::new();
        let out = shapes_from_metric("doctype", &metrics, 4);
        assert_eq!(out, vec![0; 4]);
    }

    #[test]
    fn shapes_from_metric_cycles_modulo_n_shapes() {
        let mut metrics = std::collections::HashMap::new();
        // Buckets 0,1,2,3,4,5,6 → 0,1,2,3,4,0,1 with N_NODE_SHAPES=5.
        metrics.insert(
            "doctype".to_string(),
            vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        );
        let out = shapes_from_metric("doctype", &metrics, 7);
        assert_eq!(out, vec![0, 1, 2, 3, 4, 0, 1]);
    }

    #[test]
    fn shapes_from_metric_handles_non_finite() {
        let mut metrics = std::collections::HashMap::new();
        metrics.insert(
            "doctype".to_string(),
            vec![f32::NAN, f32::INFINITY, -1.0, 2.0],
        );
        let out = shapes_from_metric("doctype", &metrics, 4);
        assert_eq!(out, vec![0, 0, 0, 2]);
    }

    #[test]
    fn fibonacci_sphere_is_on_shell() {
        let n = 1024;
        let radius = 800.0;
        let pts = spawn_on_unit_sphere(n, radius);
        assert_eq!(pts.len(), n * 3);
        for chunk in pts.chunks_exact(3) {
            let r = (chunk[0] * chunk[0] + chunk[1] * chunk[1] + chunk[2] * chunk[2]).sqrt();
            assert!(
                (r - radius).abs() < 1e-3,
                "point off sphere: r={r} expected {radius}"
            );
        }
    }

    #[test]
    fn fibonacci_sphere_is_deterministic() {
        let a = spawn_on_unit_sphere(256, 100.0);
        let b = spawn_on_unit_sphere(256, 100.0);
        assert_eq!(a, b);
    }

    #[test]
    fn fibonacci_sphere_handles_zero() {
        assert!(spawn_on_unit_sphere(0, 800.0).is_empty());
    }

    #[test]
    fn fibonacci_sphere_is_roughly_balanced() {
        // Centroid of a uniformly distributed sphere should hover near
        // the origin. Fibonacci lattice gives ~O(1/n) bias on the z
        // axis (one extra sample on one hemisphere); bound it loosely.
        let n = 4096;
        let radius = 1.0;
        let pts = spawn_on_unit_sphere(n, radius);
        let mut sx = 0.0;
        let mut sy = 0.0;
        let mut sz = 0.0;
        for c in pts.chunks_exact(3) {
            sx += c[0];
            sy += c[1];
            sz += c[2];
        }
        let nf = n as f32;
        let cx = sx / nf;
        let cy = sy / nf;
        let cz = sz / nf;
        assert!(cx.abs() < 0.01, "cx={cx}");
        assert!(cy.abs() < 0.01, "cy={cy}");
        assert!(cz.abs() < 0.01, "cz={cz}");
    }
}
