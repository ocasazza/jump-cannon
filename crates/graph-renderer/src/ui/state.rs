//! UI state. Owned by `App`, persisted via `eframe::Storage` as JSON.
//!
//! Phase D is UI-only: every field here is bound to an egui widget but
//! nothing yet reads these values to drive a render. Phases B/C/F wire
//! the actual graph render, data fetch, and query builder.

use serde::{Deserialize, Serialize};

use crate::ui::query::QueryModel;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Section {
    Filter,
    Style,
    Layout,
    Camera,
    Focus,
    Cursor,
    Stats,
    Instances,
    Debug,
}

impl Section {
    pub const ALL: &'static [Section] = &[
        Section::Filter,
        Section::Style,
        Section::Layout,
        Section::Camera,
        Section::Focus,
        Section::Cursor,
        Section::Stats,
        Section::Instances,
        Section::Debug,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Filter => "Filter",
            Section::Style => "Style",
            Section::Layout => "Layout",
            Section::Camera => "Camera",
            Section::Focus => "Focus",
            Section::Cursor => "Cursor",
            Section::Stats => "Stats",
            Section::Instances => "Instances",
            Section::Debug => "Debug",
        }
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
}

impl ColorBy {
    pub const ALL: &'static [ColorBy] = &[
        ColorBy::Community,
        ColorBy::Folder,
        ColorBy::Recency,
        ColorBy::Doctype,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ColorBy::Community => "Community",
            ColorBy::Folder => "Folder",
            ColorBy::Recency => "Recency",
            ColorBy::Doctype => "Doctype",
        }
    }
    pub fn metric_key(self) -> &'static str {
        match self {
            ColorBy::Community => "community",
            ColorBy::Folder => "folder",
            ColorBy::Recency => "recency",
            ColorBy::Doctype => "doctype",
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
}

impl EdgeColorBy {
    pub const ALL: &'static [EdgeColorBy] = &[
        EdgeColorBy::None,
        EdgeColorBy::Community,
        EdgeColorBy::Folder,
        EdgeColorBy::Doctype,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EdgeColorBy::None => "None (uniform)",
            EdgeColorBy::Community => "Community",
            EdgeColorBy::Folder => "Folder",
            EdgeColorBy::Doctype => "Doctype",
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppState {
    pub active_section: Option<Section>,
    pub style: StyleState,
    pub layout: LayoutState,
    pub camera: CameraState,
    pub focus: FocusState,
    pub cursor: CursorState,
    #[serde(default)]
    pub workspace: WorkspaceSettings,
    /// Dockable workspace (tabs + splits) for the central panel. Default
    /// is a single Graph tab. Old persisted state predates this field —
    /// `#[serde(default)]` keeps it loadable.
    #[serde(default)]
    pub dock: crate::ui::workspace::Workspace,
    #[serde(default)]
    pub sim_status: SimStatus,
    #[serde(default)]
    pub query: QueryModel,
    /// Persisted ActionInstances (the registry itself is re-seeded on
    /// startup; only the live instance list survives a reload).
    #[serde(default)]
    pub action_instances: Vec<crate::ui::actions::ActionInstance>,
    /// Right-hand inspector sidebar open/collapsed flag. Default true so
    /// new users see it immediately on first node click.
    #[serde(default = "default_inspector_open")]
    pub inspector_open: bool,
    /// When true, the inspector renders as a draggable floating
    /// `egui::Window` instead of a docked `SidePanel::right`. Default
    /// `false` so existing users see no change. A pin/unpin icon in
    /// the inspector header toggles this at runtime.
    #[serde(default)]
    pub inspector_floating: bool,
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

/// Identifies a floating/dockable panel that can be collapsed into the tray
/// strip. Modal dialogs and the command palette stay non-collapsible and
/// are intentionally absent from this enum.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PanelId {
    Sidebar,
    Inspector,
    FilterStrip,
    Canvas,
}

impl PanelId {
    pub fn label(self) -> &'static str {
        match self {
            PanelId::Sidebar => "Sidebar",
            PanelId::Inspector => "Inspector",
            PanelId::FilterStrip => "Filters",
            PanelId::Canvas => "Graph",
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
pub const STORAGE_KEY: &str = "graph_renderer_app_state_v2";

fn default_inspector_open() -> bool { true }
fn default_true() -> bool { true }

impl AppState {
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
        // TODO(multi-tab dock restore): when `was_dock_visible` is true,
        // re-show the egui_dock tab strip / restore any extra tabs that
        // were hidden by the pop-out. v1 is single-tab so this is a no-op.
        self.canvas_mount = CanvasMount::Background;
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
            active_section: None,
            style: StyleState::default(),
            layout: LayoutState::default(),
            camera: CameraState::default(),
            focus: FocusState::default(),
            cursor: CursorState::default(),
            workspace: WorkspaceSettings::default(),
            dock: crate::ui::workspace::Workspace::default(),
            sim_status: SimStatus::default(),
            query: QueryModel::default(),
            action_instances: Vec::new(),
            inspector_open: default_inspector_open(),
            inspector_floating: false,
            status_footer_open: false,
            tag_browser_query: String::new(),
            filter_strip_open: true,
            filter_behavior: FilterBehavior::default(),
            canvas_mount: CanvasMount::default(),
            stats: LiveStats::default(),
            layout_solve_requested: false,
        }
    }
}
