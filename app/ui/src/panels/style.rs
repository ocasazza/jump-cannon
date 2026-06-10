//! Style panel — Dioxus port of crates/graph-renderer/src/ui/sections/style.rs.
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained. Renderer access goes
//! through `crate::render::with_host`.
//!
//! The egui app applies style from its per-frame update loop
//! (`app.rs::apply_style_to_gpu`): edge style + shader intensity are pushed
//! every frame (cheap uniform writes) and the node/edge buffers are rebuilt
//! when a `style_key` change-detect fires. The Dioxus renderer's rAF tick
//! lives in `render/mod.rs` (read-only for this panel), so an equivalent
//! 1 Hz loop is spawned here on first render: it re-stages the uniforms and
//! re-detects buffer loss after a host rebuild (graph panel minimize →
//! restore reloads the original scene colors).
//!
//! Metric buffers come from `/graph/metrics/:name` via `crate::api::metric`,
//! cached in a thread-local (the loop runs outside the Dioxus runtime, so
//! the cache cannot be a `GlobalSignal`). Keys with no server buffer
//! (doctype / folder / recency / the "uniform" sentinel) fall back inside
//! the buffer builders — the same fallback the egui app hits for keys it
//! never fetches into `self.metrics`.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::render;
use crate::Ctx;

const STORE_KEY: &str = "jc_style_v1";

// --- state (mirrors ui/state.rs::StyleState and its enums) ---------------------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
enum SizeBy {
    #[default]
    PageRank,
    Degree,
    Uniform,
    Recency,
}

impl SizeBy {
    const ALL: &'static [SizeBy] = &[
        SizeBy::PageRank,
        SizeBy::Degree,
        SizeBy::Uniform,
        SizeBy::Recency,
    ];
    fn label(self) -> &'static str {
        match self {
            SizeBy::PageRank => "PageRank",
            SizeBy::Degree => "Degree",
            SizeBy::Uniform => "Uniform",
            SizeBy::Recency => "Recency",
        }
    }
    /// Metric key — "uniform" is a sentinel handled specially in
    /// [`render::data::sizes_from_metric`] ("recency" has no server buffer
    /// and takes the same flat-size fallback, as in the egui app).
    fn metric_key(self) -> &'static str {
        match self {
            SizeBy::PageRank => "pagerank",
            SizeBy::Degree => "degree",
            SizeBy::Uniform => "uniform",
            SizeBy::Recency => "recency",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
enum ColorBy {
    #[default]
    Community,
    Folder,
    Recency,
    Doctype,
    /// Categorical tint by primary tag (first-sorted-tag hash). Served by
    /// `/graph/metrics/tag` — same tiebreaker the egui FieldIndex uses.
    Tag,
}

impl ColorBy {
    const ALL: &'static [ColorBy] = &[
        ColorBy::Community,
        ColorBy::Folder,
        ColorBy::Recency,
        ColorBy::Doctype,
        ColorBy::Tag,
    ];
    fn label(self) -> &'static str {
        match self {
            ColorBy::Community => "Community",
            ColorBy::Folder => "Folder",
            ColorBy::Recency => "Recency",
            ColorBy::Doctype => "Doctype",
            ColorBy::Tag => "Tag",
        }
    }
    fn metric_key(self) -> &'static str {
        match self {
            ColorBy::Community => "community",
            ColorBy::Folder => "folder",
            ColorBy::Recency => "recency",
            ColorBy::Doctype => "doctype",
            ColorBy::Tag => "tag",
        }
    }
}

/// What attribute decides each node's rendered glyph shape. Default
/// `Doctype` (notes / code / image distinguishable at a glance); `Uniform`
/// is the disc-only opt-out. Bucket → primitive is `value_hash % 5`
/// ([`shapes_from_metric`]).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
enum ShapeBy {
    #[default]
    Doctype,
    Community,
    Folder,
    Uniform,
}

impl ShapeBy {
    const ALL: &'static [ShapeBy] = &[
        ShapeBy::Doctype,
        ShapeBy::Community,
        ShapeBy::Folder,
        ShapeBy::Uniform,
    ];
    fn label(self) -> &'static str {
        match self {
            ShapeBy::Doctype => "Doctype",
            ShapeBy::Community => "Community",
            ShapeBy::Folder => "Folder",
            ShapeBy::Uniform => "Uniform",
        }
    }
    fn metric_key(self) -> &'static str {
        match self {
            ShapeBy::Doctype => "doctype",
            ShapeBy::Community => "community",
            ShapeBy::Folder => "folder",
            ShapeBy::Uniform => "uniform",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
enum EdgeColorBy {
    /// Use the uniform `edge_color` (existing behaviour).
    #[default]
    None,
    Community,
    Folder,
    Doctype,
    /// Edges whose endpoints share a primary-tag bucket get the tag's
    /// palette swatch; "bridging" edges fall back to `edge_color`.
    Tag,
}

impl EdgeColorBy {
    const ALL: &'static [EdgeColorBy] = &[
        EdgeColorBy::None,
        EdgeColorBy::Community,
        EdgeColorBy::Folder,
        EdgeColorBy::Doctype,
        EdgeColorBy::Tag,
    ];
    fn label(self) -> &'static str {
        match self {
            EdgeColorBy::None => "None (uniform)",
            EdgeColorBy::Community => "Community",
            EdgeColorBy::Folder => "Folder",
            EdgeColorBy::Doctype => "Doctype",
            EdgeColorBy::Tag => "Tag",
        }
    }
    /// `None` returns an empty key (unused — the call site short-circuits).
    fn metric_key(self) -> &'static str {
        match self {
            EdgeColorBy::None => "",
            EdgeColorBy::Community => "community",
            EdgeColorBy::Folder => "folder",
            EdgeColorBy::Doctype => "doctype",
            EdgeColorBy::Tag => "tag",
        }
    }
}

/// Whether the `community` categorical metric (consumed by
/// `ColorBy::Community`, `EdgeColorBy::Community`, `ShapeBy::Community`)
/// is the server-side Louvain result or overridden with the primary-tag
/// buckets. The egui app derives the tag metric client-side from its
/// FieldIndex; here the server's `/graph/metrics/tag` buffer (same
/// first-sorted-tag tiebreaker) plays that role.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
enum CommunitySource {
    #[default]
    Computed,
    Tag,
}

impl CommunitySource {
    const ALL: &'static [CommunitySource] = &[CommunitySource::Computed, CommunitySource::Tag];
    fn label(self) -> &'static str {
        match self {
            CommunitySource::Computed => "Computed (Louvain)",
            CommunitySource::Tag => "By tag",
        }
    }
}

/// Verbatim field-for-field port of ui/state.rs::StyleState, including the
/// serde defaults so persisted blobs survive field additions. All fields
/// are `Copy` so the loop mirror below can live in a `Cell`. `pub(crate)`
/// so `crate::appstate` can carry it as the round-trip `style` field.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct StyleState {
    size_by: SizeBy,
    color_by: ColorBy,
    #[serde(default)]
    shape_by: ShapeBy,
    #[serde(default)]
    edge_color_by: EdgeColorBy,
    size_mul: f32,
    /// Edge width multiplier applied on top of `edge_width`.
    #[serde(default = "default_edge_size_mul")]
    edge_size_mul: f32,
    /// When true, both node and edge multipliers are interpreted as
    /// `10^(slider - 1.0)` at the consumer site.
    #[serde(default)]
    log_scale_size: bool,
    /// Post-process visual-intensity scalar (multiplies fragment alpha
    /// in node + edge shaders). 1.0 = neutral.
    #[serde(default = "default_shader_intensity")]
    shader_intensity: f32,
    /// Cosmograph-style edge tint (RGBA, 0..1). Default #3a4880.
    #[serde(default = "default_edge_color")]
    edge_color: [f32; 4],
    /// Density multiplier on the edge alpha.
    #[serde(default = "default_edge_alpha_mul")]
    edge_alpha_mul: f32,
    /// `linkVisibilityDistanceRange` from the cosmograph reference.
    #[serde(default = "default_edge_dist_min")]
    edge_dist_min: f32,
    #[serde(default = "default_edge_dist_max")]
    edge_dist_max: f32,
    /// `linkVisibilityMinTransparency` — alpha floor at long edges.
    #[serde(default = "default_edge_min_transparency")]
    edge_min_transparency: f32,
    /// Long-distance asymptotic alpha floor.
    #[serde(default = "default_edge_fade_floor")]
    edge_fade_floor: f32,
    /// Fat-line pixel width (vertex-shader quad expansion).
    #[serde(default = "default_edge_width")]
    edge_width: f32,
    /// Active categorical palette for community / metric colouring.
    #[serde(default)]
    palette: PaletteId,
    #[serde(default)]
    community_source: CommunitySource,
}

fn default_edge_color() -> [f32; 4] {
    [0.227, 0.282, 0.502, 1.0]
}
fn default_edge_alpha_mul() -> f32 {
    2.0
}
// 50 / 1600 track the 800-unit Fibonacci-shell spawn — see the egui
// state.rs note on typical settle-time edge lengths.
fn default_edge_dist_min() -> f32 {
    50.0
}
fn default_edge_dist_max() -> f32 {
    1600.0
}
fn default_edge_min_transparency() -> f32 {
    1.0
}
fn default_edge_fade_floor() -> f32 {
    0.085
}
fn default_edge_width() -> f32 {
    2.1
}
fn default_edge_size_mul() -> f32 {
    1.0
}
fn default_shader_intensity() -> f32 {
    1.0
}

impl Default for StyleState {
    fn default() -> Self {
        Self {
            size_by: SizeBy::default(),
            color_by: ColorBy::default(),
            shape_by: ShapeBy::default(),
            edge_color_by: EdgeColorBy::default(),
            // 0.5 = the egui default (user requested ~25% smaller nodes).
            size_mul: 0.5,
            edge_size_mul: default_edge_size_mul(),
            log_scale_size: false,
            shader_intensity: default_shader_intensity(),
            edge_color: default_edge_color(),
            edge_alpha_mul: default_edge_alpha_mul(),
            edge_dist_min: default_edge_dist_min(),
            edge_dist_max: default_edge_dist_max(),
            edge_min_transparency: default_edge_min_transparency(),
            edge_fade_floor: default_edge_fade_floor(),
            edge_width: default_edge_width(),
            palette: PaletteId::default(),
            community_source: CommunitySource::default(),
        }
    }
}

// --- palettes (port of data.rs::PaletteId + tables) -----------------------------
//
// `render/data.rs` only carries the default Tableau20 table (private) and is
// read-only for this panel, so the full catalogue lives here. A follow-up
// can hoist these into render/data.rs and drop the duplicate Tableau20.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum PaletteId {
    #[default]
    Tableau20,
    OkabeIto,
    ColorBrewerSet1,
    ColorBrewerDark2,
    Viridis,
    Plasma,
    SchrodingerCorporate,
    SchrodingerScientific,
    Monochrome,
    MonoSingleHue,
    PaperGrayscale,
}

impl PaletteId {
    const ALL: &'static [PaletteId] = &[
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

    fn label(self) -> &'static str {
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
    [0.106, 0.255, 0.404],
    [0.176, 0.388, 0.553],
    [0.282, 0.541, 0.682],
    [0.420, 0.659, 0.745],
    [0.169, 0.475, 0.467],
    [0.318, 0.604, 0.553],
    [0.494, 0.706, 0.671],
    [0.624, 0.624, 0.643],
    [0.376, 0.420, 0.482],
    [0.165, 0.220, 0.290],
];

// Schrödinger Scientific — earthy greens, muted oranges, clay browns.
const PAL_SCHRO_SCIENTIFIC: [[f32; 3]; 10] = [
    [0.314, 0.451, 0.243],
    [0.490, 0.604, 0.349],
    [0.663, 0.722, 0.420],
    [0.769, 0.604, 0.345],
    [0.804, 0.451, 0.224],
    [0.722, 0.345, 0.196],
    [0.561, 0.318, 0.227],
    [0.400, 0.275, 0.220],
    [0.275, 0.224, 0.176],
    [0.529, 0.510, 0.420],
];

// Monochrome — #1A1A1A→#F0F0F0 with small hue shifts per bucket.
const PAL_MONOCHROME: [[f32; 3]; 12] = [
    [0.102, 0.102, 0.110],
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
    [0.941, 0.941, 0.941],
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

// Paper grayscale — off-white #E8E0D0 down to charcoal #2A2520.
const PAL_PAPER_GRAYSCALE: [[f32; 3]; 10] = [
    [0.910, 0.878, 0.816],
    [0.835, 0.804, 0.745],
    [0.757, 0.729, 0.671],
    [0.682, 0.651, 0.596],
    [0.604, 0.576, 0.525],
    [0.529, 0.502, 0.451],
    [0.451, 0.424, 0.380],
    [0.376, 0.349, 0.306],
    [0.298, 0.275, 0.235],
    [0.165, 0.145, 0.125],
];

fn palette_table(id: PaletteId) -> &'static [[f32; 3]] {
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

fn palette_color(idx: u32, id: PaletteId) -> [f32; 3] {
    let table = palette_table(id);
    table[(idx as usize) % table.len()]
}

// --- buffer builders (palette-aware ports of egui data.rs) ----------------------
//
// `render::data::colors_from_metric` is Tableau20-only; the palette-aware
// variants from the egui app's data.rs live here so the Palette picker has
// full effect. Sizes go through the shared `render::data::sizes_from_metric`.

/// Translate a UI multiplier slider value into the actual scalar applied
/// downstream (`app.rs::apply_size_scale`): with log scale the slider is
/// `10^(value - 1.0)` so 1.0 → 1.0×, 2.0 → 10×, 0.0 → 0.1×.
#[inline]
fn apply_size_scale(slider: f32, log_scale: bool) -> f32 {
    if log_scale {
        10f32.powf(slider - 1.0)
    } else {
        slider
    }
}

fn colors_from_metric(
    metric_key: &str,
    metrics: &HashMap<String, Vec<f32>>,
    n: usize,
    palette: PaletteId,
) -> Vec<f32> {
    let Some(v) = metrics.get(metric_key) else {
        return render::data::default_colors(n);
    };
    if v.len() < n {
        return render::data::default_colors(n);
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

/// Per-edge RGBA: endpoints sharing a categorical bucket get the bucket's
/// swatch (alpha from `fallback`); bridging edges keep `fallback` so
/// cross-community edges stay neutral.
fn edge_colors_from_metric(
    metric_key: &str,
    metrics: &HashMap<String, Vec<f32>>,
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

/// Number of distinct sprite primitives node.wgsl draws (circle, square,
/// triangle, diamond, hexagon) — data.rs::N_NODE_SHAPES.
const N_NODE_SHAPES: u32 = 5;

/// Per-node shape-id buffer: `bucket % N_NODE_SHAPES`; "uniform" and
/// missing/undersized metrics short-circuit to all-zero (disc-only).
fn shapes_from_metric(metric_key: &str, metrics: &HashMap<String, Vec<f32>>, n: usize) -> Vec<u32> {
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

/// Port of `app.rs::metrics_view`: when `community_source == Tag`, the
/// `community` key is overridden with the primary-tag buckets. Unlike the
/// egui app the `tag` key needs no injection — `/graph/metrics/tag` is
/// fetched straight into the cache.
fn metrics_view<'a>(
    metrics: &'a HashMap<String, Vec<f32>>,
    style: &StyleState,
) -> Cow<'a, HashMap<String, Vec<f32>>> {
    if style.community_source != CommunitySource::Tag {
        return Cow::Borrowed(metrics);
    }
    let Some(tag_m) = metrics.get("tag") else {
        return Cow::Borrowed(metrics);
    };
    let mut m = metrics.clone();
    m.insert("community".to_string(), tag_m.clone());
    Cow::Owned(m)
}

// --- persisted state + loop mirrors ---------------------------------------------

static STYLE: GlobalSignal<StyleState> =
    Signal::global(|| LocalStorage::get(STORE_KEY).unwrap_or_default());

// Plain mirrors for the spawn_local loop below — `GlobalSignal` reads
// require a Dioxus runtime context, which a detached future doesn't have.
thread_local! {
    static INIT: Cell<bool> = const { Cell::new(false) };
    static STYLE_MIRROR: Cell<StyleState> = Cell::new(StyleState::default());
    /// Metric buffers fetched from /graph/metrics/:name, keyed by name.
    static METRICS_TL: RefCell<HashMap<String, Vec<f32>>> = RefCell::new(HashMap::new());
    /// Bumped on every cache insert — part of the recompute change-detect.
    static METRICS_GEN: Cell<u32> = const { Cell::new(0) };
    /// Metric fetches in flight (or backing off after a failure).
    static PENDING: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    /// Change-detect for the buffer recompute — the egui app's
    /// `prev_style_key`, extended with the metrics generation and the
    /// `sizes_base` allocation address. The address changes when either we
    /// or a host rebuild (`mount_canvas` → `load`) replace the buffers, so
    /// a minimize → restore of the Graph panel re-triggers the recompute.
    static LAST_APPLIED: Cell<Option<(StyleState, u32, usize)>> = const { Cell::new(None) };
    /// Selected node index + last panel render time, mirrored so the loop
    /// can re-push the selection emphasis / search-dim overlay after a
    /// recompute (the egui app forces this via `prev_selected_hash = None`).
    static SEL_MIRROR: Cell<Option<u32>> = const { Cell::new(None) };
    static LAST_PANEL_RENDER_MS: Cell<f64> = const { Cell::new(0.0) };
}

fn update(mutate: impl FnOnce(&mut StyleState)) {
    // Attribute the auto-snapshot like the egui section, which stamps
    // `snapshot_source = Some("Style")` every frame it renders.
    crate::appstate::note_source("Style");
    let snap = {
        let mut s = STYLE.write();
        mutate(&mut s);
        *s
    };
    let _ = LocalStorage::set(STORE_KEY, &snap);
    STYLE_MIRROR.with(|c| c.set(snap));
    ensure_metrics(&snap);
    apply_now();
}

/// AppState round-trip seam (`crate::appstate`): the live style state.
pub(crate) fn state_snapshot() -> StyleState {
    *STYLE.read()
}

/// Reset to the egui defaults — the palette's `reset-style` builtin
/// (the same swap the panel's own "↺ defaults" button performs).
pub(crate) fn reset_to_defaults() {
    update(|s| *s = StyleState::default());
}

/// AppState round-trip seam: write an imported style straight to
/// localStorage. The apply path reloads the page, and the boot re-seed of
/// [`STYLE`] is the live swap — no signal write needed here.
pub(crate) fn state_restore(s: &StyleState) {
    let _ = LocalStorage::set(STORE_KEY, s);
}

/// Kick off fetches for every server-served metric the current style needs
/// and doesn't have cached. Keys with no server buffer (uniform / recency /
/// doctype / folder) are skipped — the builders fall back, exactly like the
/// egui app does for keys absent from its metrics map.
fn ensure_metrics(style: &StyleState) {
    const SERVED: &[&str] = &[
        "degree",
        "indegree",
        "outdegree",
        "pagerank",
        "betweenness",
        "kcore",
        "community",
        "wcc",
        "tag",
    ];
    let mut want = vec![
        style.size_by.metric_key(),
        style.color_by.metric_key(),
        style.shape_by.metric_key(),
    ];
    if style.edge_color_by != EdgeColorBy::None {
        want.push(style.edge_color_by.metric_key());
    }
    if style.community_source == CommunitySource::Tag {
        want.push("tag");
    }
    for key in want {
        if !SERVED.contains(&key) {
            continue;
        }
        if METRICS_TL.with(|m| m.borrow().contains_key(key)) {
            continue;
        }
        if PENDING.with(|p| !p.borrow_mut().insert(key)) {
            continue;
        }
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::metric(key).await {
                Ok(v) => {
                    METRICS_TL.with(|m| {
                        m.borrow_mut().insert(key.to_string(), v);
                    });
                    METRICS_GEN.with(|g| g.set(g.get().wrapping_add(1)));
                    PENDING.with(|p| {
                        p.borrow_mut().remove(key);
                    });
                    apply_now();
                }
                Err(e) => {
                    tracing::warn!("[style] metric {key}: {e}");
                    // 15 s before the key becomes fetchable again — the
                    // server may still be indexing the vault.
                    gloo_timers::future::TimeoutFuture::new(15_000).await;
                    PENDING.with(|p| {
                        p.borrow_mut().remove(key);
                    });
                }
            }
        });
    }
}

/// Mirror of `app.rs::apply_style_to_gpu`. Edge style + shader intensity
/// are uniform writes and pushed on every call; the node/edge buffer
/// recompute is gated on the (style, metrics-gen, buffer-address) key.
fn apply_now() {
    let style = STYLE_MIRROR.with(Cell::get);
    let recomputed = METRICS_TL.with(|cache| {
        let metrics = cache.borrow();
        let gen = METRICS_GEN.with(Cell::get);
        render::with_host(|h| {
            let (pipes, queue) = h.pipes_and_queue();
            if !pipes.is_loaded() {
                return false;
            }
            pipes.set_edge_style(
                style.edge_color,
                style.edge_alpha_mul,
                (style.edge_dist_min, style.edge_dist_max),
                style.edge_min_transparency,
                style.edge_width * apply_size_scale(style.edge_size_mul, style.log_scale_size),
                style.edge_fade_floor,
            );
            pipes.set_shader_intensity(style.shader_intensity);

            let buf_ptr = pipes.sizes_base().as_ptr() as usize;
            if LAST_APPLIED.with(Cell::get) == Some((style, gen, buf_ptr)) {
                return false;
            }

            let n = pipes.n_nodes() as usize;
            let mv = metrics_view(&metrics, &style);
            let colors = colors_from_metric(style.color_by.metric_key(), mv.as_ref(), n, style.palette);
            let sizes = render::data::sizes_from_metric(
                style.size_by.metric_key(),
                &metrics,
                n,
                apply_size_scale(style.size_mul, style.log_scale_size),
            );
            let shapes = shapes_from_metric(style.shape_by.metric_key(), mv.as_ref(), n);
            pipes.update_colors(queue, colors);
            pipes.update_sizes(queue, sizes);
            pipes.update_shape_ids(queue, shapes);
            // Edge colors: when EdgeColorBy::None, push the uniform
            // edge_color for every edge so per-edge tinting is inert.
            let n_edges = pipes.n_edges() as usize;
            if n_edges > 0 {
                let edge_colors = if style.edge_color_by == EdgeColorBy::None {
                    let mut v = Vec::with_capacity(n_edges * 4);
                    for _ in 0..n_edges {
                        v.extend_from_slice(&style.edge_color);
                    }
                    v
                } else {
                    let edges = pipes.edges_cpu().to_vec();
                    edge_colors_from_metric(
                        style.edge_color_by.metric_key(),
                        mv.as_ref(),
                        n,
                        &edges,
                        style.edge_color,
                        style.palette,
                    )
                };
                pipes.update_edge_colors(queue, edge_colors);
            }
            let new_ptr = pipes.sizes_base().as_ptr() as usize;
            LAST_APPLIED.with(|c| c.set(Some((style, gen, new_ptr))));
            true
        })
        .unwrap_or(false)
    });
    // The recompute replaced colors_base / sizes_base — re-push the
    // selection emphasis + search-dim overlay (egui: prev_selected_hash =
    // None). The mirror is only trusted while the panel renders; see the
    // PARITY GAP note in ensure_init.
    if recomputed && js_sys::Date::now() - LAST_PANEL_RENDER_MS.with(Cell::get) < 3000.0 {
        render::set_selected_node(SEL_MIRROR.with(Cell::get));
    }
}

/// `pub(crate)`: `appstate::ensure_init` arms this loop from the FIRST
/// panel that renders (Nodes is open in the default layout), so style
/// applies from effective app start like the egui update loop — not only
/// once the Style panel itself first opens.
pub(crate) fn ensure_init() {
    if INIT.with(|c| c.replace(true)) {
        return;
    }
    let s = *STYLE.read();
    STYLE_MIRROR.with(|c| c.set(s));
    // PARITY GAP: after a loop-triggered recompute with the panel closed
    // (Graph panel minimize → restore), the selection-emphasis re-push is
    // skipped (stale mirror) — emphasis/dim return on the next selection or
    // search change via main.rs's effects.
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            let style = STYLE_MIRROR.with(Cell::get);
            ensure_metrics(&style);
            apply_now();
            gloo_timers::future::TimeoutFuture::new(1000).await;
        }
    });
}

// --- row widgets (HTML analogs of ui/widgets.rs::{row, reset_row}) --------------

fn select_row(
    label: &'static str,
    options: Vec<&'static str>,
    selected_idx: usize,
    on: impl FnMut(usize) + 'static,
) -> Element {
    let mut on = on;
    rsx! {
        div { class: "sty-row",
            span { class: "sty-label", "{label}" }
            select { class: "sty-select",
                onchange: move |e| {
                    if let Ok(i) = e.value().parse::<usize>() {
                        on(i);
                    }
                },
                for (i, lab) in options.iter().enumerate() {
                    option { value: "{i}", selected: i == selected_idx, "{lab}" }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn slider_row(
    label: &'static str,
    suffix: &'static str,
    min: f64,
    max: f64,
    step: f64,
    decimals: usize,
    value: f32,
    on: impl FnMut(f32) + 'static,
) -> Element {
    let mut on = on;
    rsx! {
        div { class: "sty-row",
            span { class: "sty-label", "{label}" }
            input {
                r#type: "range",
                min: "{min}",
                max: "{max}",
                step: "{step}",
                value: "{value}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on(v);
                    }
                },
            }
            span { class: "sty-val", { format!("{:.*}{}", decimals, value, suffix) } }
        }
    }
}

fn rgb_hex(c: [f32; 4]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

fn hex_rgb(s: &str) -> Option<[f32; 3]> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let p = |r: std::ops::Range<usize>| u8::from_str_radix(&s[r], 16).ok();
    Some([
        p(0..2)? as f32 / 255.0,
        p(2..4)? as f32 / 255.0,
        p(4..6)? as f32 / 255.0,
    ])
}

// --- panel ---------------------------------------------------------------------

pub fn panel(ctx: Ctx) -> Element {
    ensure_init();
    crate::appstate::ensure_init();
    // Selection mirror for the post-recompute overlay re-push. Kept fresh
    // for as long as the panel renders (the workspace re-renders open
    // panels whenever the selection signal changes).
    {
        let g = ctx.graph.read();
        let sel = ctx.selected.read();
        let idx = g
            .as_ref()
            .zip(sel.as_ref())
            .and_then(|(g, id)| g.id_to_idx.get(id))
            .copied();
        SEL_MIRROR.with(|c| c.set(idx));
        LAST_PANEL_RENDER_MS.with(|c| c.set(js_sys::Date::now()));
    }

    let s = *STYLE.read();
    // The uniform edge-color picker only applies while per-edge tinting is
    // off — same enablement as the egui color button.
    let uniform_edge = s.edge_color_by == EdgeColorBy::None;
    let hex = rgb_hex(s.edge_color);

    rsx! {
        div { class: "sty",
            div { class: "sty-reset-row",
                button { class: "btn sty-small",
                    onclick: move |_| update(|s| *s = StyleState::default()),
                    "↺ Reset"
                }
            }

            {select_row("Size by",
                SizeBy::ALL.iter().map(|v| v.label()).collect(),
                SizeBy::ALL.iter().position(|v| *v == s.size_by).unwrap_or(0),
                move |i| { if let Some(&v) = SizeBy::ALL.get(i) { update(|s| s.size_by = v); } })}

            {select_row("Color by",
                ColorBy::ALL.iter().map(|v| v.label()).collect(),
                ColorBy::ALL.iter().position(|v| *v == s.color_by).unwrap_or(0),
                move |i| { if let Some(&v) = ColorBy::ALL.get(i) { update(|s| s.color_by = v); } })}

            // "Community source" override. Always-shown for discoverability —
            // the underlying override is a no-op unless at least one of
            // color_by / edge_color_by / shape_by is set to Community.
            {select_row("Community source",
                CommunitySource::ALL.iter().map(|v| v.label()).collect(),
                CommunitySource::ALL.iter().position(|v| *v == s.community_source).unwrap_or(0),
                move |i| { if let Some(&v) = CommunitySource::ALL.get(i) { update(|s| s.community_source = v); } })}

            {select_row("Shape by",
                ShapeBy::ALL.iter().map(|v| v.label()).collect(),
                ShapeBy::ALL.iter().position(|v| *v == s.shape_by).unwrap_or(0),
                move |i| { if let Some(&v) = ShapeBy::ALL.get(i) { update(|s| s.shape_by = v); } })}

            {select_row("Palette",
                PaletteId::ALL.iter().map(|p| p.label()).collect(),
                PaletteId::ALL.iter().position(|p| *p == s.palette).unwrap_or(0),
                move |i| { if let Some(&p) = PaletteId::ALL.get(i) { update(|s| s.palette = p); } })}

            {slider_row("Node size multiplier", "×", 0.25, 4.0, 0.01, 2, s.size_mul,
                move |v| update(|s| s.size_mul = v))}

            {slider_row("Edge size multiplier", "×", 0.25, 4.0, 0.01, 2, s.edge_size_mul,
                move |v| update(|s| s.edge_size_mul = v))}

            div { class: "sty-row",
                span { class: "sty-label", "Log scale (10^(v−1))" }
                input {
                    r#type: "checkbox",
                    checked: s.log_scale_size,
                    onchange: move |e| update(|s| s.log_scale_size = e.checked()),
                }
            }

            {slider_row("Shader intensity", "×", 0.0, 4.0, 0.01, 2, s.shader_intensity,
                move |v| update(|s| s.shader_intensity = v))}

            {select_row("Edge color by",
                EdgeColorBy::ALL.iter().map(|v| v.label()).collect(),
                EdgeColorBy::ALL.iter().position(|v| *v == s.edge_color_by).unwrap_or(0),
                move |i| { if let Some(&v) = EdgeColorBy::ALL.get(i) { update(|s| s.edge_color_by = v); } })}

            // egui uses one RGBA picker (Alpha::OnlyBlend); the HTML analog
            // is a color input (RGB) + an alpha slider over the same state.
            div { class: "sty-row",
                span { class: if uniform_edge { "sty-label" } else { "sty-label dim" }, "Edge color" }
                input {
                    r#type: "color",
                    value: "{hex}",
                    disabled: !uniform_edge,
                    oninput: move |e| {
                        if let Some([r, g, b]) = hex_rgb(&e.value()) {
                            update(|s| {
                                s.edge_color[0] = r;
                                s.edge_color[1] = g;
                                s.edge_color[2] = b;
                            });
                        }
                    },
                }
                input {
                    r#type: "range",
                    class: "sty-alpha",
                    min: "0",
                    max: "1",
                    step: "0.01",
                    value: "{s.edge_color[3]}",
                    disabled: !uniform_edge,
                    title: "alpha",
                    oninput: move |e| {
                        if let Ok(a) = e.value().parse::<f32>() {
                            update(|s| s.edge_color[3] = a);
                        }
                    },
                }
                span { class: "sty-val", { format!("α {:.2}", s.edge_color[3]) } }
            }

            {slider_row("Edge width (px)", "px", 0.5, 8.0, 0.05, 2, s.edge_width,
                move |v| update(|s| s.edge_width = v))}

            {slider_row("Edge density", "α×", 0.0, 2.0, 0.01, 2, s.edge_alpha_mul,
                move |v| update(|s| s.edge_alpha_mul = v))}

            // Two sliders share one logical "Edge distance range" label —
            // separate rows so each slider gets its own grow space.
            {slider_row("Edge distance min", "min", 0.0, 200.0, 1.0, 0, s.edge_dist_min,
                move |v| update(|s| s.edge_dist_min = v))}
            {slider_row("Edge distance max", "max", 50.0, 2400.0, 1.0, 0, s.edge_dist_max,
                move |v| update(|s| s.edge_dist_max = v))}

            {slider_row("Edge min visibility", "", 0.0, 1.0, 0.01, 2, s.edge_min_transparency,
                move |v| update(|s| s.edge_min_transparency = v))}

            {slider_row("Long-distance fade floor", "", 0.0, 0.5, 0.005, 3, s.edge_fade_floor,
                move |v| update(|s| s.edge_fade_floor = v))}
        }
    }
}
