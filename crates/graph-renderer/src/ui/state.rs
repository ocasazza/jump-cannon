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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyleState {
    pub size_by: SizeBy,
    pub color_by: ColorBy,
    pub size_mul: f32,
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
}

fn default_edge_color() -> [f32; 4] { [0.227, 0.282, 0.502, 1.0] }
fn default_edge_alpha_mul() -> f32 { 0.6 }
fn default_edge_dist_min() -> f32 { 10.0 }
fn default_edge_dist_max() -> f32 { 400.0 }
fn default_edge_min_transparency() -> f32 { 0.6 }

impl Default for StyleState {
    fn default() -> Self {
        Self {
            size_by: SizeBy::default(),
            color_by: ColorBy::default(),
            size_mul: 1.0,
            edge_color: default_edge_color(),
            edge_alpha_mul: default_edge_alpha_mul(),
            edge_dist_min: default_edge_dist_min(),
            edge_dist_max: default_edge_dist_max(),
            edge_min_transparency: default_edge_min_transparency(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LayoutPreset {
    Fast,
    #[default]
    Balanced,
    Pretty,
}

impl LayoutPreset {
    /// Apply the canonical slider values for this preset. Tuned for
    /// convergence on 10k-node vaults — the system bleeds kinetic energy
    /// via cooling_alpha until damping bottoms out at cooling_floor, so
    /// the layout reaches a steady state instead of orbiting forever.
    pub fn apply_to(self, l: &mut LayoutState) {
        match self {
            // Fast: settle quickly, accept a less-spread layout. For big
            // graphs (>10k) where you'd rather see structure now than
            // wait for a perfect arrangement.
            LayoutPreset::Fast => {
                l.repulsion = 150.0;
                l.spring_k = 0.10;
                l.spring_len = 40.0;
                l.gravity = 0.005;
                l.damping = 0.85;
                l.dt = 0.045;
                l.steps_per_call = 1.0;
                l.cooling_alpha = 0.99;
                l.cooling_floor = 0.65;
                l.energy_threshold = 0.5;
            }
            // Balanced (default): wide edges + strong repulsion + slow
            // cool-down so the layout has time to spread into 3D and
            // doesn't collapse into a ring. Per-frame compute stays low
            // (steps_per_call=2) so it isn't perceived as slow.
            LayoutPreset::Balanced => {
                l.repulsion = 250.0;
                l.spring_k = 0.06;
                l.spring_len = 60.0;
                l.gravity = 0.003;
                l.damping = 0.90;
                l.dt = 0.04;
                l.steps_per_call = 2.0;
                l.cooling_alpha = 0.997;
                l.cooling_floor = 0.55;
                l.energy_threshold = 0.05;
            }
            // Pretty: more iterations, even softer cool-down, finer dt.
            // For small/medium graphs where the user wants the cleanest
            // possible final layout.
            LayoutPreset::Pretty => {
                l.repulsion = 400.0;
                l.spring_k = 0.05;
                l.spring_len = 80.0;
                l.gravity = 0.002;
                l.damping = 0.92;
                l.dt = 0.025;
                l.steps_per_call = 4.0;
                l.cooling_alpha = 0.999;
                l.cooling_floor = 0.55;
                l.energy_threshold = 0.02;
            }
        }
        l.preset = self;
    }
}

/// Repulsion backend mirror of `graph_layouts::RepulsionMode`. Lives in
/// the UI state so it persists across sessions and so the UI doesn't have
/// to depend on the layouts crate's enum directly.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RepulsionBackend {
    #[default]
    Grid,
    BarnesHut,
}

impl RepulsionBackend {
    pub const ALL: &'static [RepulsionBackend] = &[
        RepulsionBackend::Grid,
        RepulsionBackend::BarnesHut,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RepulsionBackend::Grid => "Grid (27-cell)",
            RepulsionBackend::BarnesHut => "Barnes-Hut",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutState {
    pub preset: LayoutPreset,
    pub repulsion: f32,
    pub spring_k: f32,
    pub spring_len: f32,
    pub gravity: f32,
    pub damping: f32,
    pub dt: f32,
    pub steps_per_call: f32,
    /// Multiplied into damping per `step_with_encoder` call, until damping
    /// floors at `cooling_floor`. Drives the sim toward a steady state
    /// instead of perpetual orbiting.
    #[serde(default = "default_cooling_alpha")]
    pub cooling_alpha: f32,
    #[serde(default = "default_cooling_floor")]
    pub cooling_floor: f32,
    /// Auto-halt threshold on max per-node kinetic energy. 0.0 disables
    /// auto-halt entirely (the sim runs forever); ~0.05 is a good
    /// "settled" value for the default tuning. Drives `is_halted()` and
    /// the Stats panel running/settled indicator.
    #[serde(default = "default_energy_threshold")]
    pub energy_threshold: f32,
    /// Repulsion backend. Default Grid; flip to BarnesHut for clustered
    /// graphs (Obsidian-style hub neighborhoods) where the 27-cell grid
    /// degenerates into hundreds of pairs per voxel.
    #[serde(default)]
    pub repulsion_mode: RepulsionBackend,
}

fn default_cooling_alpha() -> f32 { 0.998 }
fn default_cooling_floor() -> f32 { 0.55 }
fn default_energy_threshold() -> f32 { 0.05 }

impl Default for LayoutState {
    fn default() -> Self {
        // Per-frame compute is bounded by the low steps_per_call (2);
        // the sim is allowed to keep running for many seconds so 3D
        // structure has time to emerge (otherwise the sphere collapses
        // to a planar ring before repulsion has spread it out).
        // Bigger spring_len + repulsion give clusters more breathing
        // room and push the layout into 3D rather than flattening.
        Self {
            preset: LayoutPreset::default(),
            repulsion: 250.0,        // mass-weighted; halved from 500 to compensate for hub amplification
            spring_k: 0.06,          // softer springs let repulsion win
            spring_len: 60.0,        // was 30 — wider edges
            gravity: 0.003,          // was 0.005 — less inward pull
            damping: 0.90,
            dt: 0.04,
            steps_per_call: 2.0,     // keep low for per-frame budget
            cooling_alpha: 0.997,    // slow cool — give it time
            cooling_floor: 0.55,
            energy_threshold: 0.05,  // hold off on halting until truly settled
            repulsion_mode: RepulsionBackend::default(),
        }
    }
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
            follow_centroid: false,
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
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            dof_enabled: false,
            distance: 100.0,
            thickness: 50.0,
            blur: 0.5,
            max_coc: 8.0,
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

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AppState {
    pub active_section: Option<Section>,
    pub style: StyleState,
    pub layout: LayoutState,
    pub camera: CameraState,
    pub focus: FocusState,
    pub cursor: CursorState,
    #[serde(default)]
    pub workspace: WorkspaceSettings,
    #[serde(default)]
    pub sim_status: SimStatus,
    #[serde(default)]
    pub query: QueryModel,
    /// Persisted ActionInstances (the registry itself is re-seeded on
    /// startup; only the live instance list survives a reload).
    #[serde(default)]
    pub action_instances: Vec<crate::ui::actions::ActionInstance>,
    #[serde(skip)]
    pub stats: LiveStats,
}

pub const STORAGE_KEY: &str = "graph_renderer_app_state_v1";
