//! UI state. Owned by `App`, persisted via `eframe::Storage` as JSON.
//!
//! Phase D is UI-only: every field here is bound to an egui widget but
//! nothing yet reads these values to drive a render. Phases B/C/F wire
//! the actual graph render, data fetch, and query builder.

use serde::{Deserialize, Serialize};

use crate::ui::query::QueryModel;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Filter,
    Style,
    Layout,
    Camera,
    Instances,
    Debug,
    Metrics,
    Generate,
    Timeline,
}

impl Section {
    pub const ALL: &'static [Section] = &[
        Section::Filter,
        Section::Style,
        Section::Layout,
        Section::Camera,
        Section::Instances,
        Section::Debug,
        Section::Metrics,
        Section::Generate,
        Section::Timeline,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Filter => "Filter",
            Section::Style => "Style",
            Section::Layout => "Layout",
            Section::Camera => "Camera",
            Section::Instances => "Instances",
            Section::Debug => "Debug",
            Section::Metrics => "Metrics",
            Section::Generate => "Generate (tvix)",
            Section::Timeline => "Timeline",
        }
    }
}

/// A layout-quality metric the Metrics panel can display and pin.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MetricKind {
    EdgeLengthCv,
    EdgeStress,
    FullStress,
    Crossings,
}

impl MetricKind {
    pub const ALL: &'static [MetricKind] = &[
        MetricKind::EdgeLengthCv,
        MetricKind::EdgeStress,
        MetricKind::FullStress,
        MetricKind::Crossings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetricKind::EdgeLengthCv => "Edge-length CV",
            MetricKind::EdgeStress => "Edge stress (norm.)",
            MetricKind::FullStress => "Full stress (norm.)",
            MetricKind::Crossings => "Edge crossings",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            MetricKind::EdgeLengthCv => {
                "Coefficient of variation of edge lengths. 0 = perfectly uniform. Cheap, O(E)."
            }
            MetricKind::EdgeStress => {
                "Scale-normalized stress over edges only (target distance 1). Cheap, O(E)."
            }
            MetricKind::FullStress => {
                "Scale-normalized stress over ALL node pairs (graph-theoretic distances). \
                 O(n²) — computed on demand and only for small graphs."
            }
            MetricKind::Crossings => {
                "Number of edge pairs that cross in 2D — fewer is more readable. \
                 O(E²), so computed on demand alongside full stress."
            }
        }
    }

    /// This metric's raw value from a snapshot, if computed. Crossings is
    /// surfaced through the same `f32` channel; [`format_value`](Self::format_value)
    /// renders it as an integer.
    pub fn value(self, snap: &MetricsSnapshot) -> Option<f32> {
        match self {
            MetricKind::EdgeLengthCv => Some(snap.edge_length_cv),
            MetricKind::EdgeStress => Some(snap.edge_stress),
            MetricKind::FullStress => snap.full_stress,
            MetricKind::Crossings => snap.crossings.map(|c| c as f32),
        }
    }

    /// Display string for this metric's value, or `"—"` when not yet computed.
    /// Crossings render as an integer; everything else as a 3-decimal float.
    /// Single source of truth shared by the panel and the always-on HUD.
    pub fn format_value(self, snap: &MetricsSnapshot) -> String {
        match self.value(snap) {
            None => "—".to_string(),
            Some(v) if matches!(self, MetricKind::Crossings) => format!("{}", v as u32),
            Some(v) => format!("{v:.3}"),
        }
    }
}

/// Latest computed layout-quality values for the active layout.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub n_nodes: u32,
    pub n_edges: u32,
    pub edge_length_cv: f32,
    pub edge_stress: f32,
    /// `None` until a full-stress compute is requested (and the graph is small
    /// enough that the O(n²) pass is allowed).
    pub full_stress: Option<f32>,
    /// Edge-crossing count — `None` until the on-demand O(E²) pass runs.
    pub crossings: Option<u32>,
}

/// Metrics-panel state: pinned metrics (persisted) + last computed snapshot +
/// one-shot request flags drained by `App::update` (mirrors
/// `layout_solve_requested`). Request flags are `skip`-ped from persistence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MetricsState {
    #[serde(default)]
    pub pinned: Vec<MetricKind>,
    #[serde(default)]
    pub last: Option<MetricsSnapshot>,
    /// Live mode: recompute the cheap (edge-based) metrics every frame.
    #[serde(default)]
    pub auto: bool,
    #[serde(default, skip)]
    pub compute_requested: bool,
    #[serde(default, skip)]
    pub compute_full_requested: bool,
}

/// Which view the Debug section is showing: the live frontend event log
/// or the perf / engine stats charts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DebugViewMode {
    #[default]
    Events,
    Stats,
}

/// A single entry in the rolling frontend-event log surfaced by the
/// Debug console. Captured at mutation sites (palette execute, chip
/// toggle, section open/close, anchored promote/expand).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrontendEvent {
    /// Unix epoch milliseconds at the moment the event fired.
    pub timestamp_ms: u64,
    /// Short tag for where the event came from, e.g. `"palette"`,
    /// `"filter-strip"`, `"section"`, `"anchored:promote"`.
    pub source: String,
    /// Human-readable one-liner.
    pub message: String,
}

/// Rolling buffer of [`FrontendEvent`]s. Capped at [`Self::cap`]; oldest
/// evicted on push. Not persisted — `#[serde(skip)]` on [`AppState`].
#[derive(Clone, Debug)]
pub struct FrontendEventLog {
    pub entries: std::collections::VecDeque<FrontendEvent>,
    pub cap: usize,
}

impl Default for FrontendEventLog {
    fn default() -> Self {
        Self { entries: std::collections::VecDeque::new(), cap: 500 }
    }
}

impl FrontendEventLog {
    /// Append a new event tagged with `source` + `message`. Caller is
    /// any UI mutation site — the helper handles the timestamp and the
    /// cap-driven eviction.
    pub fn push(&mut self, source: impl Into<String>, message: impl Into<String>) {
        let timestamp_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.entries.push_back(FrontendEvent {
            timestamp_ms,
            source: source.into(),
            message: message.into(),
        });
        let cap = self.cap.max(1);
        while self.entries.len() > cap {
            self.entries.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// How the filter chip set affects the rendered graph.
///
/// - `Filter` (default): non-matching nodes are *discarded* via the
///   shader's per-node filter mask (the path added by commit
///   `ca7d40d7`). Edges touching them disappear too.
/// - `Focus`: non-matching nodes remain visible but dim to ~0.25
///   alpha via the focus-set path. Useful for keeping the broader
///   structure on screen while highlighting matches.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FilterBehavior {
    #[default]
    Filter,
    Focus,
}

impl FilterBehavior {
    pub fn label(self) -> &'static str {
        match self {
            FilterBehavior::Filter => "Filter",
            FilterBehavior::Focus => "Focus",
        }
    }
    pub fn tooltip(self) -> &'static str {
        match self {
            FilterBehavior::Filter => "Hide non-matching nodes and the edges that touch them.",
            FilterBehavior::Focus => "Keep non-matches on screen but dim them to ~25% alpha.",
        }
    }
    pub fn toggled(self) -> Self {
        match self {
            FilterBehavior::Filter => FilterBehavior::Focus,
            FilterBehavior::Focus => FilterBehavior::Filter,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SizeBy {
    #[default]
    PageRank,
    Degree,
    Uniform,
    Recency,
}

impl SizeBy {
    pub const ALL: &'static [SizeBy] = &[
        SizeBy::PageRank,
        SizeBy::Degree,
        SizeBy::Uniform,
        SizeBy::Recency,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SizeBy::PageRank => "PageRank",
            SizeBy::Degree => "Degree",
            SizeBy::Uniform => "Uniform",
            SizeBy::Recency => "Recency",
        }
    }
    /// Bootstrap.metrics key — "uniform" is a sentinel handled specially
    /// in [`crate::data::sizes_from_metric`].
    pub fn metric_key(self) -> &'static str {
        match self {
            SizeBy::PageRank => "pagerank",
            SizeBy::Degree => "degree",
            SizeBy::Uniform => "uniform",
            SizeBy::Recency => "recency",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ColorBy {
    #[default]
    Community,
    Folder,
    Recency,
    Doctype,
    /// Categorical tint by primary tag (first-sorted-tag hash, computed
    /// client-side from the MetaSummary `tags` field index).
    Tag,
}

impl ColorBy {
    pub const ALL: &'static [ColorBy] = &[
        ColorBy::Community,
        ColorBy::Folder,
        ColorBy::Recency,
        ColorBy::Doctype,
        ColorBy::Tag,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ColorBy::Community => "Community",
            ColorBy::Folder => "Folder",
            ColorBy::Recency => "Recency",
            ColorBy::Doctype => "Doctype",
            ColorBy::Tag => "Tag",
        }
    }
    pub fn metric_key(self) -> &'static str {
        match self {
            ColorBy::Community => "community",
            ColorBy::Folder => "folder",
            ColorBy::Recency => "recency",
            ColorBy::Doctype => "doctype",
            ColorBy::Tag => "tag",
        }
    }
}

/// What attribute decides each node's rendered glyph shape.
///
/// Default `Doctype` so notes, code, and image nodes are visually
/// distinguishable at a glance even when colour is being used for a
/// different signal (community / folder / recency). The mapping from
/// category bucket → primitive shape is handled by
/// [`crate::data::shapes_from_metric`] (`value_hash % n_shapes`).
///
/// `Uniform` (every node is a circle) is the opt-out so a user who
/// finds mixed-shape rendering noisy can fall back to disc-only.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ShapeBy {
    #[default]
    Doctype,
    Community,
    Folder,
    Uniform,
}

impl ShapeBy {
    pub const ALL: &'static [ShapeBy] = &[
        ShapeBy::Doctype,
        ShapeBy::Community,
        ShapeBy::Folder,
        ShapeBy::Uniform,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ShapeBy::Doctype => "Doctype",
            ShapeBy::Community => "Community",
            ShapeBy::Folder => "Folder",
            ShapeBy::Uniform => "Uniform",
        }
    }
    /// `Bootstrap.metrics` key for the underlying categorical metric.
    /// `Uniform` returns the `"uniform"` sentinel that
    /// [`crate::data::shapes_from_metric`] short-circuits to shape-id 0.
    pub fn metric_key(self) -> &'static str {
        match self {
            ShapeBy::Doctype => "doctype",
            ShapeBy::Community => "community",
            ShapeBy::Folder => "folder",
            ShapeBy::Uniform => "uniform",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
pub enum EdgeColorBy {
    /// Use the uniform `edge_color` (existing behaviour).
    #[default]
    None,
    Community,
    Folder,
    Doctype,
    /// Categorical tint by primary tag (first-sorted-tag hash). Edges
    /// whose endpoints share a primary tag get the tag's palette swatch;
    /// "bridging" edges fall back to the uniform `edge_color`.
    Tag,
}

impl EdgeColorBy {
    pub const ALL: &'static [EdgeColorBy] = &[
        EdgeColorBy::None,
        EdgeColorBy::Community,
        EdgeColorBy::Folder,
        EdgeColorBy::Doctype,
        EdgeColorBy::Tag,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EdgeColorBy::None => "None (uniform)",
            EdgeColorBy::Community => "Community",
            EdgeColorBy::Folder => "Folder",
            EdgeColorBy::Doctype => "Doctype",
            EdgeColorBy::Tag => "Tag",
        }
    }
    /// `Bootstrap.metrics` key for the underlying categorical metric.
    /// `None` returns an empty key (unused — the call site short-circuits).
    pub fn metric_key(self) -> &'static str {
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
/// is sourced from the server-side Louvain result or derived
/// client-side from each node's primary tag (first sorted tag's hash).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
pub enum CommunitySource {
    /// Server's `metrics["community"]` (Louvain). Default — preserves
    /// existing behaviour for persisted state.
    #[default]
    Computed,
    /// Client-side override: replace the `community` metric with a hash
    /// of each node's first-sorted-tag. Untagged nodes hash to bucket 0.
    Tag,
}

impl CommunitySource {
    pub const ALL: &'static [CommunitySource] =
        &[CommunitySource::Computed, CommunitySource::Tag];
    pub fn label(self) -> &'static str {
        match self {
            CommunitySource::Computed => "Computed (Louvain)",
            CommunitySource::Tag => "By tag",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyleState {
    pub size_by: SizeBy,
    pub color_by: ColorBy,
    /// Categorical attribute that drives per-node sprite shape.
    /// `#[serde(default)]` so pre-shape persisted state keeps loading
    /// (defaults to `ShapeBy::Doctype`, i.e. doctype-keyed glyphs).
    #[serde(default)]
    pub shape_by: ShapeBy,
    /// Per-edge categorical tinting. `None` keeps the existing uniform
    /// `edge_color` behaviour. Default `None` so persisted state and
    /// existing users see no visual change.
    #[serde(default)]
    pub edge_color_by: EdgeColorBy,
    pub size_mul: f32,
    /// Edge width multiplier applied on top of `edge_width`.
    #[serde(default = "default_edge_size_mul")]
    pub edge_size_mul: f32,
    /// When true, both node and edge multipliers are interpreted as
    /// `10^(slider - 1.0)` at the consumer site.
    #[serde(default)]
    pub log_scale_size: bool,
    /// Post-process visual-intensity scalar (multiplies fragment alpha
    /// in node + edge shaders). 1.0 = neutral.
    #[serde(default = "default_shader_intensity")]
    pub shader_intensity: f32,
    /// Cosmograph-style edge tint (RGBA, 0..1). Default #3a4880.
    #[serde(default = "default_edge_color")]
    pub edge_color: [f32; 4],
    /// Density multiplier on the edge alpha. Cosmograph mimics line
    /// width via stacking many low-alpha lines; this is the same dial.
    #[serde(default = "default_edge_alpha_mul")]
    pub edge_alpha_mul: f32,
    /// `linkVisibilityDistanceRange` from the reference.
    #[serde(default = "default_edge_dist_min")]
    pub edge_dist_min: f32,
    #[serde(default = "default_edge_dist_max")]
    pub edge_dist_max: f32,
    /// `linkVisibilityMinTransparency` — alpha floor at long edges.
    #[serde(default = "default_edge_min_transparency")]
    pub edge_min_transparency: f32,
    /// Long-distance asymptotic alpha floor. The fade curve smooths from
    /// `edge_min_transparency` toward this value past `edge_dist_max` and
    /// then 1/(1+x)-tails toward (but never reaches) it. Default 0.02.
    #[serde(default = "default_edge_fade_floor")]
    pub edge_fade_floor: f32,
    /// Fat-line pixel width (vertex-shader quad expansion). 1.0 ≈ the
    /// old wgpu LineList thickness; default 1.5 for a slightly heavier
    /// stroke on dense graphs.
    #[serde(default = "default_edge_width")]
    pub edge_width: f32,
    /// Active categorical palette for community / metric colouring.
    /// Default `Tableau20` so existing persisted state is unchanged.
    #[serde(default)]
    pub palette: crate::data::PaletteId,
    /// Whether `community`-keyed visuals (ColorBy/EdgeColorBy/ShapeBy)
    /// use the server's Louvain output (default) or a client-side
    /// override derived from each node's primary tag. New field; old
    /// persisted blobs load as `Computed`.
    #[serde(default)]
    pub community_source: CommunitySource,
}

fn default_edge_color() -> [f32; 4] { [0.227, 0.282, 0.502, 1.0] }
fn default_edge_alpha_mul() -> f32 { 2.0 }
// Bumped from 10 / 400 to 50 / 1600 to track the 800-unit Fibonacci-shell
// spawn (data::spawn_on_unit_sphere). Typical edge lengths during settle
// land in the few-hundred-to-low-thousand range, so the old 10..400 band
// was clamping every edge to the long-distance fade floor.
fn default_edge_dist_min() -> f32 { 50.0 }
fn default_edge_dist_max() -> f32 { 1600.0 }
fn default_edge_min_transparency() -> f32 { 1.0 }
fn default_edge_fade_floor() -> f32 { 0.085 }
fn default_edge_width() -> f32 { 2.1 }
fn default_edge_size_mul() -> f32 { 1.0 }
fn default_shader_intensity() -> f32 { 1.0 }

impl Default for StyleState {
    fn default() -> Self {
        Self {
            size_by: SizeBy::default(),
            color_by: ColorBy::default(),
            shape_by: ShapeBy::default(),
            edge_color_by: EdgeColorBy::default(),
            // 0.5 = 0.67 × 0.75 rounded to a clean slider value (user
            // requested ~25% smaller default node size).
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
            palette: crate::data::PaletteId::default(),
            community_source: CommunitySource::default(),
        }
    }
}

/// Persisted layout-section state.
///
/// The pre-refactor `LayoutState` carried every gpu-force slider as a
/// dedicated typed field. Step 1 of the layout abstraction collapses all
/// algorithm-specific knobs into a JSON-keyed bag so the registry can
/// host arbitrary static + physics layouts without growing this struct.
///
/// `active` is the registered layout id (e.g. `"gpu-force"`).
/// `settings[id]` is the algorithm-specific JSON block (decoded into the
/// appropriate `Settings` type by the algorithm's UI module).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutState {
    #[serde(default = "default_active_layout")]
    pub active: String,
    #[serde(default, deserialize_with = "deserialize_settings_with_migration")]
    pub settings: std::collections::HashMap<String, serde_json::Value>,
}

fn default_active_layout() -> String { "gpu-force".to_string() }

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            active: default_active_layout(),
            settings: std::collections::HashMap::new(),
        }
    }
}

impl LayoutState {
    /// Get-or-insert mutable JSON for the given layout id, falling back to
    /// the supplied default factory when the key is missing.
    pub fn settings_for_mut(
        &mut self,
        id: graph_layouts::LayoutId,
        default_factory: impl FnOnce() -> serde_json::Value,
    ) -> &mut serde_json::Value {
        self.settings
            .entry(id.to_string())
            .or_insert_with(default_factory)
    }
}

/// Migration shim: detects the *old* persisted shape (top-level
/// `repulsion` / `spring_k` / etc. fields living next to a `preset`) and
/// folds it into `settings["gpu-force"]`. New shape just deserialises a
/// `HashMap<String, Value>` straight through.
///
/// The old shape was a sibling field of `settings`, which serde won't
/// see when it parses `LayoutState` — so we hook in here, attempting to
/// pull the legacy fields out of the parent `Value` is impractical from
/// inside a per-field deserializer. The pragmatic compromise: try to
/// parse settings as the new shape; if absent, return empty. Any actual
/// migration of the old shape happens in `migrate_layout_state` below
/// which the App calls during startup with the raw stored Value.
fn deserialize_settings_with_migration<'de, D>(
    deserializer: D,
) -> Result<std::collections::HashMap<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use std::collections::HashMap;
    HashMap::<String, serde_json::Value>::deserialize(deserializer)
}

/// Inspect a raw stored `AppState` JSON value and, if it carries a
/// pre-refactor `LayoutState` (top-level `repulsion` / `spring_k` /
/// `repulsion_mode` etc. on the `layout` object), rewrite it into the
/// new `{ active, settings: { "gpu-force": {...} } }` shape.
///
/// Called once from `App::new` before deserialising into `AppState`.
/// Returns the value mutated in place.
pub fn migrate_layout_state(raw: &mut serde_json::Value) {
    let Some(obj) = raw.as_object_mut() else { return };
    let Some(layout) = obj.get_mut("layout").and_then(|v| v.as_object_mut()) else {
        return;
    };
    // New shape already? Bail.
    if layout.contains_key("active") && layout.contains_key("settings") {
        return;
    }
    // Build a gpu-force settings object out of whatever legacy keys exist.
    // We map each top-level key to its `GpuForceOptions` field name.
    // `repulsion_mode` was a UI-side enum (Grid / BarnesHut / NegativeSampling)
    // that GpuForceOptions's hand-rolled deser accepts as a lowercase
    // snake_case string ("grid" / "barnes_hut" / "negative_sampling").
    let mut gpu_force = serde_json::Map::new();
    let copy_keys = [
        "repulsion",
        "spring_k",
        "spring_len",
        "gravity",
        "damping",
        "dt",
        "cooling_alpha",
        "cooling_floor",
        "energy_threshold",
        "repulsion_samples",
    ];
    for k in copy_keys {
        if let Some(v) = layout.remove(k) {
            gpu_force.insert(k.to_string(), v);
        }
    }
    if let Some(v) = layout.remove("steps_per_call") {
        // Old field was f32; GpuForceOptions wants u32. round + clamp.
        let n = v.as_f64().unwrap_or(2.0).round().max(1.0) as u64;
        gpu_force.insert("steps_per_call".to_string(), serde_json::json!(n));
    }
    if let Some(v) = layout.remove("repulsion_mode") {
        // Old enum strings: "Grid" / "BarnesHut" / "NegativeSampling".
        let mapped = match v.as_str() {
            Some("BarnesHut") => "barnes_hut",
            Some("NegativeSampling") => "negative_sampling",
            _ => "grid",
        };
        gpu_force.insert("repulsion_mode".to_string(), serde_json::json!(mapped));
    }
    // Drop legacy preset key.
    layout.remove("preset");
    // Stamp new shape.
    layout.insert("active".to_string(), serde_json::json!("gpu-force"));
    let mut settings = serde_json::Map::new();
    settings.insert(
        "gpu-force".to_string(),
        serde_json::Value::Object(gpu_force),
    );
    layout.insert("settings".to_string(), serde_json::Value::Object(settings));
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CameraState {
    pub invert_mouse_x: bool,
    pub invert_mouse_y: bool,
    pub invert_ad: bool,
    pub invert_qe: bool,
    pub follow_centroid: bool,
    pub fit_to_window: bool,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            invert_mouse_x: false,
            invert_mouse_y: false,
            invert_ad: false,
            invert_qe: false,
            follow_centroid: true,
            fit_to_window: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FocusState {
    /// Master DoF toggle. When false, the shader runs the sharp path
    /// for every node (no bokeh halo, no fragment-area inflation) —
    /// this is the cosmograph baseline. When true, the configured
    /// distance / thickness / blur / max_coc band engages.
    #[serde(default)]
    pub dof_enabled: bool,
    pub distance: f32,
    pub thickness: f32,
    pub blur: f32,
    pub max_coc: f32,
    /// Membership criterion for hover/click focus dimming. See
    /// [`crate::ui::focus_set::FocusMode`].
    #[serde(default)]
    pub focus_mode: crate::ui::focus_set::FocusMode,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            dof_enabled: false,
            distance: 100.0,
            thickness: 50.0,
            blur: 0.5,
            max_coc: 8.0,
            focus_mode: crate::ui::focus_set::FocusMode::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CursorState {
    pub radius: f32,
    pub strength: f32,
    pub depth: f32,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            radius: 80.0,
            strength: 1.0,
            depth: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SimStatus {
    #[default]
    Settled,
    Running,
    Error,
}

/// Runtime-only stats surfaced in the Stats section. Not persisted — App
/// repopulates each frame from GraphPipelines / Bootstrap.
#[derive(Clone, Debug, Default)]
pub struct LiveStats {
    pub n_nodes: u32,
    pub n_edges: u32,
    pub n_communities: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FontFamilyChoice {
    #[default]
    Monospace,
    SansSerif,
    Serif,
}

impl FontFamilyChoice {
    pub const ALL: &'static [FontFamilyChoice] = &[
        FontFamilyChoice::Monospace,
        FontFamilyChoice::SansSerif,
        FontFamilyChoice::Serif,
    ];
    pub fn label(self) -> &'static str {
        match self {
            FontFamilyChoice::Monospace => "Monospace",
            FontFamilyChoice::SansSerif => "Sans Serif",
            FontFamilyChoice::Serif => "Serif",
        }
    }
}

/// Workspace-level settings driven by the Settings sub-tree of the action
/// registry. Persisted in `AppState` so a reload preserves preferences.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    pub font_size: f32,
    pub font_family: FontFamilyChoice,
    pub show_line_numbers: bool,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_family: FontFamilyChoice::default(),
            show_line_numbers: true,
        }
    }
}

/// One entry in the [`SnapshotRing`] timeline. Captures the full
/// `AppState` (minus the ring itself, which is `#[serde(skip)]`) as a
/// JSON string so it can round-trip back through serde at restore time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Unix epoch milliseconds at the moment of capture. Monotonic
    /// within a single session; not guaranteed across restarts.
    pub timestamp_ms: u64,
    /// Short human-readable description of what caused the snapshot
    /// (e.g. `"default"`, `"palette: focus-fit"`, `"Style"`, `"misc"`).
    pub source: String,
    /// Serialized `AppState` JSON. Because `AppState::snapshots` is
    /// `#[serde(skip)]`, the blob is naturally free of recursive ring
    /// bloat — no special elision needed.
    pub state_json: String,
}

/// In-memory ring of [`StateSnapshot`]s. Per-session only — not
/// persisted across reloads (the ring itself is `#[serde(skip)]` on
/// `AppState`). The user can YAML-export an interesting state to keep
/// it long-term.
#[derive(Clone, Debug)]
pub struct SnapshotRing {
    pub entries: Vec<StateSnapshot>,
    /// Cap on the timeline length. Oldest evicted on push.
    pub max: usize,
}

impl Default for SnapshotRing {
    fn default() -> Self {
        Self { entries: Vec::new(), max: 50 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppState {
    /// Per-section floating panel visibility. Each `Section` is an
    /// independent floating panel (matches Inspector/Filter/Canvas
    /// behaviour). Missing key = closed. Default empty.
    #[serde(default)]
    pub section_open: std::collections::BTreeMap<Section, bool>,
    /// Per-section placement: Floating (default) or Tiled. Missing key
    /// = Floating. See [`crate::ui::tiles::Placement`].
    #[serde(default)]
    pub section_placement: std::collections::BTreeMap<Section, crate::ui::tiles::Placement>,
    /// Placement of the filter strip panel. Default Floating preserves
    /// the historical chrome.
    #[serde(default)]
    pub filter_strip_placement: crate::ui::tiles::Placement,
    /// Placement of the promoted-node ("Node") panel: Floating (default)
    /// keeps it as a free-roaming `FloatingPanel`; Tiled snaps it into
    /// the workspace tree. Toggled via the green traffic light, exactly
    /// like every other panel. Floating by default so promoting a node
    /// pops a window rather than re-laying-out the workspace.
    #[serde(default = "default_node_placement")]
    pub node_panel_placement: crate::ui::tiles::Placement,
    /// Mirror of "a node is currently promoted" (App owns the actual
    /// `promoted_anchored_idx`). App writes this every frame before the
    /// workspace sync so `sync_tree_with_open_state` can mount/unmount
    /// the Node pane. Session-scoped, never persisted.
    #[serde(skip)]
    pub node_panel_open: bool,
    /// One-shot: set when the user closes the Node tile via its red
    /// traffic light inside the workspace. App reads-and-clears it after
    /// the workspace renders and dismisses the promoted node.
    #[serde(skip)]
    pub node_panel_close_requested: bool,
    /// Tile-tree workspace (`egui_tiles`). Hidden when zero tiled
    /// panels are present.
    #[serde(default)]
    pub tiles: crate::ui::tiles::TileWorkspace,
    pub style: StyleState,
    pub layout: LayoutState,
    pub camera: CameraState,
    pub focus: FocusState,
    pub cursor: CursorState,
    /// Metrics-panel state (pinned metrics + last computed snapshot).
    #[serde(default)]
    pub metrics: MetricsState,
    #[serde(default)]
    pub workspace: WorkspaceSettings,
    /// Dockable workspace (tabs + splits) for the central panel. Default
    /// is a single Graph tab. Old persisted state predates this field —
    /// `#[serde(default)]` keeps it loadable.
    #[serde(default)]
    pub dock: crate::ui::workspace::Workspace,
    /// If true, the dock tab strip should be forced visible on the next dock render.
    #[serde(default)]
    pub dock_tab_strip_force_show: bool,
    #[serde(default)]
    pub sim_status: SimStatus,
    #[serde(default)]
    pub query: QueryModel,
    /// Persisted ActionInstances (the registry itself is re-seeded on
    /// startup; only the live instance list survives a reload).
    #[serde(default)]
    pub action_instances: Vec<crate::ui::actions::ActionInstance>,
    /// Status footer open/collapsed flag. Default false so the footer
    /// stays as an unobtrusive 24px strip until the user expands it.
    #[serde(default)]
    pub status_footer_open: bool,
    /// Fuzzy-search query for the inspector's empty-state tag browser.
    /// Persisted across reloads so a user returning to the app finds
    /// their last filter still in place. Empty = show top-N by
    /// frequency.
    #[serde(default)]
    pub tag_browser_query: String,
    /// Visibility flag for the floating filter-strip panel. Default
    /// true so users see active filters as soon as they're applied.
    #[serde(default = "default_true")]
    pub filter_strip_open: bool,
    /// How active filter chips affect rendering. See [`FilterBehavior`].
    #[serde(default)]
    pub filter_behavior: FilterBehavior,
    /// Where the wgpu graph canvas is currently mounted. Driven by the
    /// tray "pop-out" toggle + the floating window's X. State machine
    /// lives in [`CanvasMount`] — UI code must transition through the
    /// `pop_canvas_out` / `dock_canvas_back` / `toggle_canvas_mount`
    /// methods on [`AppState`], not by mutating this field directly.
    #[serde(default)]
    pub canvas_mount: CanvasMount,
    #[serde(skip)]
    pub stats: LiveStats,
    /// One-shot signal: the Layout sidebar's "Solve" button sets this to
    /// `true`. `App::update` reads-and-clears it each frame and, if the
    /// active layout is Static, dispatches `run_static_solve` against the
    /// current settings (useful e.g. to re-roll a Random seed).
    #[serde(skip)]
    pub layout_solve_requested: bool,
    /// Snapshot of the YAML export from the Instances section. Populated
    /// by clicking "Export" — not round-tripped through the YAML itself
    /// (ephemeral UI scratch).
    #[serde(skip)]
    pub yaml_export_buffer: String,
    /// User-editable YAML pasted into the Instances import textarea.
    #[serde(skip)]
    pub yaml_import_buffer: String,
    /// Most recent YAML parse error, shown below the import textarea.
    #[serde(skip)]
    pub yaml_import_error: Option<String>,
    /// Two-step confirmation for "Reset to defaults". First click arms,
    /// second click commits. Cleared on any section change implicitly by
    /// being re-evaluated each frame.
    #[serde(skip)]
    pub yaml_reset_armed: bool,
    /// Last "Copy share link" output (the full link on WASM, the bare hash
    /// on native), shown read-only below the button. UI scratch — never
    /// persisted (it's derived from the rest of the state on demand).
    #[serde(skip)]
    pub share_link_buffer: String,
    /// User-pasted share hash/link in the Instances "Load from link" input.
    /// UI scratch — never persisted.
    #[serde(skip)]
    pub share_import_buffer: String,
    /// Most recent share-link decode error, shown below the input. UI scratch.
    #[serde(skip)]
    pub share_import_error: Option<String>,
    /// Versioned timeline of UI state changes. Each entry holds a JSON
    /// serialization of the entire `AppState` at the time of capture
    /// (the ring itself is `#[serde(skip)]`, so no recursion). The
    /// Instances section renders the timeline and exposes per-entry
    /// "Restore" buttons. In-memory only — lost on full app restart.
    #[serde(skip)]
    pub snapshots: SnapshotRing,
    /// Best-effort attribution label for the next auto-snapshot. UI
    /// mutation sites (sections, palette, filter chips) set this before
    /// they mutate; `App::tick_snapshots` drains it every frame so a
    /// stale label from a no-op call never lingers to mislabel a later
    /// unrelated diff. `None` falls back to `"misc"`. Skipped from
    /// serialization so it never perturbs the hash-based diff in
    /// `tick_snapshots` (and is never persisted).
    #[serde(skip)]
    pub snapshot_source: Option<String>,
    /// Which body the Debug console renders below its mode toggle.
    /// Defaults to [`DebugViewMode::Events`] so a returning user sees
    /// the live action feed first; the Stats charts are one click away.
    #[serde(default)]
    pub debug_view_mode: DebugViewMode,
    /// Rolling buffer of frontend actions / events surfaced by the
    /// Debug console's Events view. Not persisted across reloads (the
    /// log is intentionally session-scoped).
    #[serde(skip)]
    pub frontend_events: FrontendEventLog,
    /// Remote-engine picker state for the Layout section. Holds the last
    /// `/compute/engines` snapshot plus one-shot flags drained by
    /// `App::update` (which owns the `ApiClient` the section can't reach).
    /// Session-scoped, never persisted.
    #[serde(skip)]
    pub compute: ComputeEngineState,
    /// Currently logically-focused non-canvas panel. `None` means the
    /// canvas is focused (the always-receiving fallback). Drives:
    ///   * Scroll-wheel zoom gating in `ui::workspace` — when a panel
    ///     is focused, wheel events belong to its inner `ScrollArea`
    ///     and don't double-zoom the canvas.
    ///   * Per-panel "focused" visual chrome — `palette::PRIMARY` red
    ///     border instead of the standard 1px `palette::BORDER`.
    /// Session-scoped, never persisted.
    #[serde(skip)]
    pub focused_panel: Option<FocusedPanel>,
    /// Floating panels the user "minimized" (yellow traffic light) —
    /// rendered as just their header chrome (title + traffic lights),
    /// body suppressed, until the green/yellow light expands them again.
    /// Session-scoped scratch: never persisted (`#[serde(skip)]`) so it
    /// doesn't require a persist-version bump, and a fresh tab starts
    /// with everything expanded.
    #[serde(skip)]
    pub collapsed_panels: std::collections::HashSet<PanelId>,
    /// Generate-section state: the editable Nix generator expression plus a
    /// one-shot `pending` graph awaiting promotion to the GPU.
    ///
    /// The `editor.source` is a genuine user PARAMETER — an exported state /
    /// share link / example preset must be able to carry a generator expression
    /// so the UI can be driven headlessly. It therefore round-trips
    /// (`#[serde(default)]` with a template factory so old blobs / fresh tabs
    /// still get the runnable demo). The `pending` one-shot inside
    /// `GenerateState` is `#[serde(skip)]` (drained by `App::update`).
    #[serde(default = "GenerateState::with_demo")]
    pub generate: GenerateState,
    /// Initial-seed section state (Layout panel): the picked seed strategy + the
    /// Custom-Nix editor source, plus a one-shot of freshly evaluated positions.
    ///
    /// The `strategy` + `editor.source` are user PARAMETERS and round-trip so a
    /// share link / example preset can set the initial-seed regime headlessly
    /// (`#[serde(default)]` with a template factory so old blobs / fresh tabs
    /// still get the runnable template). The `pending` positions one-shot is
    /// `#[serde(skip)]` (drained by `App::update`).
    #[serde(default = "SeedState::with_template")]
    pub seed: SeedState,
    /// Timeline section state (Phase P3): the scrub/playback UI's transient
    /// position in the live frame-buffer ring plus the user-tunable capture
    /// knobs (ring depth, capture stride). See [`TimelineState`].
    #[serde(default)]
    pub timeline: TimelineState,
}

/// State for the `Section::Timeline` ("Timeline") panel — the scrubbable
/// simulation frame buffer.
///
/// The buffered frames themselves live on `App` (the `FrameRing`, fed from the
/// GPU positions readback each frame) — NOT here, because they're large and
/// purely session-scoped. This struct holds:
///
/// * the **capture knobs** (`depth`, `stride`) — genuine user PARAMETERS, so
///   they round-trip via serde (a share link / preset can pin the buffer size);
/// * the live **scrub position** (`scrub`) — `#[serde(skip)]`, since a paused-
///   at-frame-N position is meaningless once the ring is gone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineState {
    /// Max retained frames in the ring. Trades memory for scrub history; see
    /// the memory budget table in `crate::timeline`.
    pub depth: usize,
    /// Capture every `stride`-th live frame into the ring. `1` = every frame.
    /// Raising this stretches the buffered time window at the cost of temporal
    /// resolution (and keeps large graphs in memory budget).
    pub stride: usize,
    /// Live playback / scrub position. Session-only; never persisted.
    #[serde(skip)]
    pub scrub: crate::timeline::ScrubState,
    /// Mirror of the live ring's frame count, written by `App::update` each
    /// frame so the section can size the scrub slider. Session-only.
    #[serde(skip)]
    pub buffered_len: usize,
    /// Mirror of the live ring's approximate byte budget, for the readout.
    #[serde(skip)]
    pub buffered_bytes: usize,
    /// One-shot: while the scrub UI is paused, the user pressed a
    /// step/play/slider control. `App` reads `scrub` each frame regardless;
    /// this flag just lets the App know a fresh seek should be pushed to the
    /// GPU even if the index didn't change (e.g. re-pausing). Set by the
    /// section, cleared by `App`.
    #[serde(skip)]
    pub seek_dirty: bool,
}

impl Default for TimelineState {
    fn default() -> Self {
        TimelineState {
            depth: 300,
            stride: 1,
            scrub: crate::timeline::ScrubState::Live,
            buffered_len: 0,
            buffered_bytes: 0,
            seek_dirty: false,
        }
    }
}

impl TimelineState {
    /// Logical index the scrub UI currently points at: the paused frame when
    /// paused, else the head (newest buffered frame).
    pub fn current_idx(&self) -> usize {
        match self.scrub {
            crate::timeline::ScrubState::Paused { idx } => idx,
            crate::timeline::ScrubState::Live => self.buffered_len.saturating_sub(1),
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.scrub, crate::timeline::ScrubState::Paused { .. })
    }

    /// Pause at `idx`, clamped to the buffered range, and flag a seek.
    pub fn pause_at(&mut self, idx: usize) {
        let max = self.buffered_len.saturating_sub(1);
        self.scrub = crate::timeline::ScrubState::Paused { idx: idx.min(max) };
        self.seek_dirty = true;
    }

    /// Resume live playback.
    pub fn resume_live(&mut self) {
        self.scrub = crate::timeline::ScrubState::Live;
        self.seek_dirty = true;
    }
}

/// State for the `Section::Generate` ("Generate (tvix)") panel.
///
/// The panel (`ui::sections::generate`) owns no access to the renderer's
/// `SharedLoad` / GPU pipelines, mirroring the metrics/compute sections'
/// one-shot-flag pattern: it evaluates the Nix expression, stashes the
/// resulting `GeneratedGraph` in `pending`, and `App::update` drains it —
/// converting to a `Bootstrap` and triggering a live graph replace.
///
/// The `editor.source` (the authored Nix generator expression) is a user
/// PARAMETER and round-trips via `Serialize`/`Deserialize` so an exported state
/// / share link / example preset can carry a generator. `pending` is a
/// one-shot handoff slot (a freshly-evaluated graph awaiting GPU promotion) and
/// is `#[serde(skip)]` — meaningless to persist.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct GenerateState {
    /// The shared Nix-editor component state (source buffer, error, status).
    /// Reused by `ui::nix_extension::NixExtension`. `source` round-trips;
    /// `error`/`status` are `#[serde(skip)]` inside `NixEditorState`.
    #[serde(default)]
    pub editor: crate::ui::nix_extension::NixEditorState,
    /// One-shot: a freshly evaluated graph awaiting promotion to the GPU.
    /// Set by `App::update` once the background eval job completes; taken on
    /// the next frame, converted to a `Bootstrap`, and used to replace the
    /// live graph. Transient handoff slot — never persisted.
    #[serde(skip)]
    pub pending: Option<tvix_wasm::GeneratedGraph>,
    /// One-shot: a request to evaluate this Nix source OFF the click-handler.
    /// Set by the panel when the user presses "Evaluate"; taken by
    /// `App::update`, which spawns a [`crate::job::BackgroundJob`] running
    /// `tvix_wasm::eval_graph` so a large graph doesn't freeze the UI (native:
    /// off-thread; WASM: paint-first-then-run with coarse progress). Transient
    /// handoff slot — never persisted.
    #[serde(skip)]
    pub request: Option<String>,
    /// Which executor evaluates the generate expression. `Auto` (the default)
    /// lets `App` pick: Server when graph-api is reachable, else LocalWorker on
    /// wasm / Inline on native. An explicit pick from the panel/Debug dropdown
    /// overrides that. Round-trips so a share link can pin a backend.
    #[serde(default)]
    pub backend: GenerateBackendChoice,
}

/// The user-facing dispatch choice for the Generate panel. `Auto` defers to
/// `App`'s reachability-based default; the others force a specific
/// [`crate::job::ExecutionBackend`]. Mirrors the local-vs-remote *layout*
/// engine picker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerateBackendChoice {
    /// Server when reachable, else a local fallback (the sensible default).
    #[default]
    Auto,
    /// Force the Inline executor (native thread / wasm paint-first).
    Inline,
    /// Force the Server executor (async HTTP to graph-api `/generate`).
    Server,
    /// Force the Local Worker executor (feasibility-gated).
    LocalWorker,
}

/// A star-graph demo authored against the embedded tvix graph library. It
/// emits `toGraphJSON`'s `{ nodes, links }` shape (NOT `star-graph.nix`'s
/// ad-hoc `{ nodes, edges, hub_degree }`). `graph-combinators.nix` takes
/// `{ graph }` explicitly — the tvix closure-across-imports workaround —
/// so it is threaded through verbatim.
pub const GENERATE_DEMO_EXPR: &str = r#"# Edit this Nix expression, then press Evaluate.
# It must produce toGraphJSON's { nodes = [...]; links = [...]; } shape.
let
  g  = import /jc/src/graph.nix {};
  gc = import /jc/src/graph-combinators.nix { graph = g; };
in
  g.toGraphJSON (gc.starGen { nodes = 12; prefix = "n"; })
"#;

impl GenerateState {
    pub fn with_demo() -> Self {
        GenerateState {
            editor: crate::ui::nix_extension::NixEditorState::with_source(GENERATE_DEMO_EXPR),
            ..Default::default()
        }
    }
}

/// State for the Layout panel's "Initial seed" section.
///
/// Mirrors `GenerateState`'s one-shot-handoff shape. The seed section picks a
/// strategy (No seed / a built-in / Custom Nix); on Apply it evaluates a Nix
/// expression implementing the seed interface via `tvix_wasm::eval_seed` and
/// stashes the resulting positions in `pending`. `App::update` drains it and
/// writes the positions straight into the live GPU positions buffer.
///
/// The picked `strategy` + Custom-editor `source` are user PARAMETERS and
/// round-trip via `Serialize`/`Deserialize`, so an exported state / share link /
/// example preset can carry an initial-seed choice. `pending` is the one-shot
/// positions-handoff slot and is `#[serde(skip)]`.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct SeedState {
    /// The currently picked seed strategy. User-facing parameter — round-trips.
    #[serde(default)]
    pub strategy: SeedStrategy,
    /// The shared Nix-editor component state for the Custom (Nix) strategy.
    /// `source` round-trips; `error`/`status` are skipped.
    #[serde(default)]
    pub editor: crate::ui::nix_extension::NixEditorState,
    /// One-shot: freshly evaluated seed positions awaiting application to the
    /// live GPU graph. `Vec<[f32;3]>`; an empty vec is never stored here (the
    /// "No seed" path simply never sets `pending`). Taken by `App::update`.
    /// Transient handoff slot — never persisted.
    #[serde(skip)]
    pub pending: Option<Vec<[f32; 3]>>,
}

/// Which initial-seed strategy the Layout panel applies. `BuiltIn` carries the
/// embedded `seed_demos()` expression by index; `Custom` uses the editor.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SeedStrategy {
    /// Apply no seed — leave the current positions untouched.
    #[default]
    None,
    /// One of the embedded `tvix_wasm::seed_demos()` strategies, by index.
    BuiltIn(usize),
    /// A user-authored Nix expression implementing the seed interface.
    Custom,
}

/// Default Custom-strategy source: a runnable template for the seed interface.
pub const SEED_CUSTOM_EXPR: &str = r#"# Implement the seed interface, then press Apply seed.
#   seed : { n, ... } -> [ { x; y; z; } ]
# `n` (the live node count) is bound for you. Return exactly n positions,
# or [] to apply no seed. The embedded library has built-ins to build on:
let s = import /jc/src/seed.nix {};
in s.sphere { inherit n; radius = 800.0; }
"#;

impl SeedState {
    pub fn with_template() -> Self {
        SeedState {
            editor: crate::ui::nix_extension::NixEditorState::with_source(SEED_CUSTOM_EXPR),
            ..Default::default()
        }
    }
}

impl AppState {
    /// Push a snapshot of the current state into the timeline, tagged
    /// with `source`. Caller invokes from any mutation site (command
    /// palette, section UI, filter chip click, frame-tail diff
    /// detector). Because `snapshots` is `#[serde(skip)]`, serializing
    /// `self` directly is safe — the ring won't appear in the blob.
    pub fn snapshot_now(&mut self, source: impl Into<String>) {
        let source = source.into();
        let state_json = serde_json::to_string(self).unwrap_or_default();
        let timestamp_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let entry = StateSnapshot { timestamp_ms, source, state_json };
        self.snapshots.entries.push(entry);
        let max = self.snapshots.max.max(1);
        while self.snapshots.entries.len() > max {
            self.snapshots.entries.remove(0);
        }
    }
}

/// Recursively map non-finite-derived JSON `null`s back to a deserializable
/// `0.0`, but ONLY for the coordinate keys (`"x"` / `"y"`) where they originate.
///
/// Why: egui's nested types (`egui_dock::DockState`, `egui_tiles::Tree`) cache a
/// `Rect` per node, and an unlaid-out node's rect is `Rect::NOTHING` (`±∞`
/// extremities). `serde_json` renders a non-finite `f32` as JSON `null`, and
/// `f32`'s `Deserialize` then *rejects* `null` — so a plain JSON round-trip of
/// the default `AppState` (which ships a `dock` Graph tab whose rect is
/// `NOTHING`) already fails, and the persistence path silently resets to
/// default. egui's rects always live under `{"min":{"x":..,"y":..},"max":{..}}`
/// (and `viewport`), so the `±∞` nulls only ever appear at `"x"`/`"y"` keys.
///
/// Scoping the substitution to those keys is what makes it safe: a legitimate
/// `Option::None` (e.g. an `Option<usize>` field elsewhere) also serializes to
/// `null`, and zeroing *those* would corrupt the value (`0.0` is not a valid
/// `usize`). Such fields are never named `x`/`y`, so they're left untouched.
/// Rects are derived UI geometry egui recomputes on the next layout pass, so
/// zeroing the stale extents preserves all user intent.
pub fn sanitize_nonfinite(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Array(a) => a.iter_mut().for_each(sanitize_nonfinite),
        serde_json::Value::Object(o) => {
            for (k, val) in o.iter_mut() {
                if val.is_null() && (k == "x" || k == "y") {
                    *val = serde_json::json!(0.0);
                } else {
                    sanitize_nonfinite(val);
                }
            }
        }
        _ => {}
    }
}

/// Serialize `AppState` to a JSON string with non-finite floats sanitized so the
/// blob deserializes back losslessly (see [`sanitize_nonfinite`]). The single
/// canonical JSON-encode entry point for the persistence + share-link paths.
pub fn to_json_sanitized(state: &AppState) -> Result<String, String> {
    let mut value = serde_json::to_value(state).map_err(|e| e.to_string())?;
    sanitize_nonfinite(&mut value);
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

/// Serialize the entire `AppState` to a YAML document.
///
/// Round-trip stability depends on `AppState`'s `Serialize`/`Deserialize`
/// symmetry, which is already exercised by the sessionStorage and
/// `eframe::Storage` persistence paths. Fields marked `#[serde(skip)]`
/// (including the YAML UI buffers themselves) are intentionally omitted.
pub fn export_state_yaml(state: &AppState) -> Result<String, String> {
    serde_yml::to_string(state).map_err(|e| e.to_string())
}

/// Deserialize an `AppState` from a YAML document.
///
/// Unknown fields are silently ignored (serde default) so configs from
/// future versions of the app degrade gracefully. Round-trip stability
/// matches the sessionStorage path.
pub fn import_state_yaml(yaml: &str) -> Result<AppState, String> {
    serde_yml::from_str(yaml).map_err(|e| e.to_string())
}

/// Where the wgpu graph canvas is currently mounted. The canvas has
/// exactly two modes: it's either the persistent CentralPanel
/// background, or it's hosted inside a floating window. Mutate via
/// the transition methods on [`AppState`].
///
/// The `Floating` variant carries observational state for the popped-out
/// window so future commands (e.g. "reset position", multi-tab dock
/// restore) can read it without going through egui's window memory.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum CanvasMount {
    /// Canvas paints into the CentralPanel — full-bleed behind every
    /// floating panel. Default for new sessions.
    #[default]
    Background,
    /// Canvas paints into the body of a floating `egui::Window`. The
    /// CentralPanel renders a flat dark fill in this mode.
    Floating {
        /// Last-known content rect of the floating canvas window.
        /// `None` until the window first reports it (guards against
        /// feeding `Rect::NOTHING` to the wgpu paint callback).
        /// Serialized as `[f32; 4]` (`min.x, min.y, max.x, max.y`) so
        /// the format survives an egui `Rect` type change.
        #[serde(default, with = "rect_opt_serde")]
        rect: Option<egui::Rect>,
        /// Whether the egui_dock tab strip was visible at the moment
        /// of pop-out. v1 doesn't act on this (single-tab is the
        /// common path), but the bool is persisted so a future
        /// multi-tab restore on `dock_canvas_back` is non-breaking.
        #[serde(default)]
        was_dock_visible: bool,
    },
}

impl CanvasMount {
    pub fn is_floating(self) -> bool {
        matches!(self, CanvasMount::Floating { .. })
    }
}

/// Serde adapter for `Option<egui::Rect>` ↔ `Option<[f32; 4]>`. egui's
/// `Rect` does not implement `Serialize`/`Deserialize` directly; storing
/// the four extremities (`min.x, min.y, max.x, max.y`) future-proofs the
/// blob against any internal `Rect` type change.
mod rect_opt_serde {
    use eframe::egui;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &Option<egui::Rect>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let opt = value.map(|r| [r.min.x, r.min.y, r.max.x, r.max.y]);
        opt.serialize(ser)
    }

    pub fn deserialize<'de, D>(de: D) -> Result<Option<egui::Rect>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt = Option::<[f32; 4]>::deserialize(de)?;
        Ok(opt.map(|[x0, y0, x1, y1]| {
            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
        }))
    }
}

/// Logical "focused window" identifier. Tracks which non-canvas panel
/// currently owns scroll / text input so wheel events scrolled inside an
/// inspector body don't simultaneously zoom the canvas.
///
/// `None` (the absence) means the canvas is focused — the canvas is the
/// always-receiving fallback. Session-scoped, never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusedPanel {
    /// Promoted anchored panel for the given node idx.
    AnchoredNode(u32),
    /// Floating or tiled section panel.
    Section(Section),
    FilterStrip,
    /// The debug console (right-justified by default — handled like a
    /// section via `Section::Debug`, kept as a dedicated variant for
    /// future divergence).
    Debug,
}

/// Identifies a floating/dockable panel that can be collapsed into the tray
/// strip. Modal dialogs and the command palette stay non-collapsible and
/// are intentionally absent from this enum.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PanelId {
    /// Section panels are now per-`Section` floating panels — each gets
    /// its own egui memory key (`("floating", PanelId::Section(s))`).
    Section(Section),
    FilterStrip,
    Canvas,
    /// The promoted-node inspector window. One logical panel showing the
    /// currently-promoted node; its egui memory (position/size) persists
    /// per this id like every other floating panel.
    Node,
}

impl PanelId {
    pub fn label(self) -> &'static str {
        match self {
            PanelId::Section(s) => s.title(),
            PanelId::FilterStrip => "Filters",
            PanelId::Canvas => "Graph",
            PanelId::Node => "Node",
        }
    }
}

// Per-panel `*_open` bools on `AppState` drive visibility now; the
// previous `TrayState` collapsed-chips list was removed when the tray
// became a persistent launcher row rather than a parking lot for
// X-ed panels.

// Bumped from `_v1` → `_v2` when `CanvasMount::Floating` gained a
// `{ rect, was_dock_visible }` payload. Old persisted blobs encode the
// variant as the unit string `"Floating"`, which serde refuses to
// deserialize into the new struct variant. A version bump invalidates
// the cached AppState exactly once per user — preferable to carrying a
// custom deserializer for what is purely session-scoped state.
// Bumped `_v2` → `_v3` when `active_section: Option<Section>` was
// replaced by `section_open: BTreeMap<Section, bool>` so each section is
// an independent floating panel. Old persisted blobs carry the removed
// field, which serde would either error on or silently drop — bumping
// invalidates the cached AppState exactly once per user.
// Bumped `_v3` → `_v4` when `inspector_open` + `inspector_floating` fields
// were removed (right-side Inspector folded into the unified anchored
// panel). Old persisted blobs carry the removed fields; serde would
// silently drop them, but bumping the key keeps the invariant that
// schema-breaking changes invalidate the cached blob exactly once.
// Bumped `_v4` → `_v5` with the addition of `section_placement`,
// `filter_strip_placement`, and the `tiles` workspace tree. New fields
// all have `#[serde(default)]` so the bump is precautionary — but
// `TileWorkspace` ships an `egui_tiles::Tree` whose internal id
// counter is per-instance and would prefer a clean slate over a
// half-deserialized partial.
// Bumped `_v5` → `_v6` when `Section::Focus` and `Section::Cursor` were
// removed from the enum (Focus folded into Camera as a subgroup; Cursor
// dropped entirely from the section tray). Old persisted blobs encode
// these variants as map keys in `section_open` / `section_placement`
// (BTreeMap<Section, …>); serde refuses to deserialize an unknown enum
// discriminant, so the bump invalidates the cached AppState exactly once
// per user rather than silently corrupting state.
// Single stable key. Pre-alpha: breaking serde changes are expected;
// the load path in `ui::persist` already falls back to AppState::default
// when deserialization fails, so a schema break just resets to defaults
// on next reload. No version ceremony.
pub const STORAGE_KEY: &str = "graph_renderer_app_state";

fn default_true() -> bool { true }

/// Default placement for the promoted-node ("Node") panel. Floating —
/// unlike the section panels (which default Tiled) we keep node previews
/// as free-roaming windows so a click pops a card rather than mounting
/// the workspace side panel.
fn default_node_placement() -> crate::ui::tiles::Placement {
    crate::ui::tiles::Placement::Floating
}

impl AppState {
    /// Whether the given section's floating panel is currently open.
    pub fn is_section_open(&self, s: Section) -> bool {
        self.section_open.get(&s).copied().unwrap_or(false)
    }
    /// Toggle the given section's open flag.
    pub fn toggle_section(&mut self, s: Section) {
        let v = self.is_section_open(s);
        log::info!("[graph-renderer] section_open -> {:?} = {}", s, !v);
        self.section_open.insert(s, !v);
        self.frontend_events.push(
            "section",
            format!("{}: {}", s.title(), if !v { "open" } else { "close" }),
        );
    }
    /// Explicit set helper — replaces the old `active_section = Some(s)`
    /// pattern at every call site.
    pub fn set_section_open(&mut self, s: Section, open: bool) {
        self.section_open.insert(s, open);
    }

    pub fn pop_canvas_out(&mut self) {
        // Snapshot dock visibility *before* the transition — once
        // `canvas_mount` flips to `Floating`, the renderer hides the
        // dock strip and we'd lose the prior signal. Today the only
        // case where the strip was actually visible is >1 tab; with a
        // single tab the renderer collapses the strip to zero height
        // (see app.rs `n_tabs <= 1`).
        let was_dock_visible = self.dock.has_multiple_tabs();
        self.canvas_mount = CanvasMount::Floating { rect: None, was_dock_visible };
    }
    pub fn dock_canvas_back(&mut self) {
        let was_dock_visible = if let CanvasMount::Floating { was_dock_visible, .. } = self.canvas_mount {
            was_dock_visible
        } else {
            false
        };
        self.canvas_mount = CanvasMount::Background;
        if was_dock_visible {
            self.dock_tab_strip_force_show = true;
        }
    }
    pub fn toggle_canvas_mount(&mut self) {
        match self.canvas_mount {
            CanvasMount::Background => self.pop_canvas_out(),
            CanvasMount::Floating { .. } => self.dock_canvas_back(),
        }
    }
    pub fn canvas_is_floating(&self) -> bool {
        matches!(self.canvas_mount, CanvasMount::Floating { .. })
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            section_open: std::collections::BTreeMap::new(),
            section_placement: std::collections::BTreeMap::new(),
            filter_strip_placement: crate::ui::tiles::Placement::default(),
            node_panel_placement: default_node_placement(),
            node_panel_open: false,
            node_panel_close_requested: false,
            tiles: crate::ui::tiles::TileWorkspace::default(),
            style: StyleState::default(),
            layout: LayoutState::default(),
            camera: CameraState::default(),
            focus: FocusState::default(),
            cursor: CursorState::default(),
            metrics: MetricsState::default(),
            workspace: WorkspaceSettings::default(),
            dock: crate::ui::workspace::Workspace::default(),
            dock_tab_strip_force_show: false,
            sim_status: SimStatus::default(),
            query: QueryModel::default(),
            action_instances: Vec::new(),
            status_footer_open: false,
            tag_browser_query: String::new(),
            filter_strip_open: false,
            filter_behavior: FilterBehavior::default(),
            canvas_mount: CanvasMount::default(),
            stats: LiveStats::default(),
            layout_solve_requested: false,
            yaml_export_buffer: String::new(),
            yaml_import_buffer: String::new(),
            yaml_import_error: None,
            yaml_reset_armed: false,
            share_link_buffer: String::new(),
            share_import_buffer: String::new(),
            share_import_error: None,
            snapshots: SnapshotRing::default(),
            snapshot_source: None,
            debug_view_mode: DebugViewMode::default(),
            frontend_events: FrontendEventLog::default(),
            compute: ComputeEngineState::default(),
            focused_panel: None,
            collapsed_panels: std::collections::HashSet::new(),
            generate: GenerateState::with_demo(),
            seed: SeedState::with_template(),
            timeline: TimelineState::default(),
        }
    }
}

/// Renderer-side state for the Layout section's "Remote engine" picker.
///
/// The Layout section (`ui::sections::layout`) renders the dropdown but
/// has no access to the `ApiClient` / base url — those live on `App`. So
/// this struct is the seam: the section reads `snapshot` to draw the
/// combo and raises one-shot flags (`refresh_requested`, `select`), and
/// `App::update` drains those flags, performs the async HTTP call, and
/// writes the result back into `snapshot` via the shared `Arc<Mutex<..>>`.
///
/// `#[serde(skip)]` on the `AppState` field — never persisted.
#[derive(Clone, Default, Debug)]
pub struct ComputeEngineState {
    /// Latest `/compute/engines` result. `None` until the first fetch
    /// resolves; the inner `Result` distinguishes a server error (route
    /// 404 / graph-api down) from a successful "no worker" snapshot
    /// (`connected: false`). Shared with the async fetch task on `App`.
    pub snapshot: std::sync::Arc<
        std::sync::Mutex<Option<Result<crate::fetch::ComputeEngines, String>>>,
    >,
    /// True once a fetch has been kicked off, so the lazy auto-refresh on
    /// panel open fires exactly once per session unless the user hits the
    /// manual refresh button (which clears nothing — it just re-requests).
    pub requested_once: bool,
    /// One-shot: the section wants a fresh `/compute/engines` fetch.
    /// Drained by `App::update`.
    pub refresh_requested: bool,
    /// One-shot: the user picked engine id `Some(..)` in the combo.
    /// `App::update` PUTs `/compute/layout` then re-requests engines.
    pub select: Option<String>,
    /// `egui` time (seconds) of the last `/compute/engines` fetch attempt.
    /// Throttles the auto-retry-while-unavailable loop in the Layout section
    /// (see `RETRY_SECS`). `0.0` = never attempted.
    pub last_attempt: f64,
}

impl ComputeEngineState {
    /// Snapshot the current engines result (cloned out of the shared
    /// latch) for read-only rendering. `None` = not yet fetched.
    pub fn current(&self) -> Option<Result<crate::fetch::ComputeEngines, String>> {
        self.snapshot.lock().ok().and_then(|g| g.clone())
    }
}
