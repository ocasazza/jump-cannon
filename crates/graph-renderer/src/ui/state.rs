//! UI state. Owned by `App`, persisted via `eframe::Storage` as JSON.
//!
//! Phase D is UI-only: every field here is bound to an egui widget but
//! nothing yet reads these values to drive a render. Phases B/C/F wire
//! the actual graph render, data fetch, and query builder.

use serde::{Deserialize, Serialize};

use crate::ui::query::QueryModel;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Section {
    Filter,
    Style,
    Layout,
    Camera,
    Focus,
    Cursor,
    Stats,
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
}

impl Default for StyleState {
    fn default() -> Self {
        Self {
            size_by: SizeBy::default(),
            color_by: ColorBy::default(),
            size_mul: 1.0,
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
            LayoutPreset::Fast => {
                l.repulsion = 150.0;
                l.spring_k = 0.10;
                l.spring_len = 25.0;
                l.gravity = 0.005;
                l.damping = 0.72;
                l.dt = 0.045;
                l.steps_per_call = 12.0;
                l.cooling_alpha = 0.995;
                l.cooling_floor = 0.50;
                l.energy_threshold = 0.05;
            }
            LayoutPreset::Balanced => {
                l.repulsion = 200.0;
                l.spring_k = 0.08;
                l.spring_len = 30.0;
                l.gravity = 0.005;
                l.damping = 0.78;
                l.dt = 0.04;
                l.steps_per_call = 8.0;
                l.cooling_alpha = 0.998;
                l.cooling_floor = 0.55;
                l.energy_threshold = 0.05;
            }
            LayoutPreset::Pretty => {
                l.repulsion = 300.0;
                l.spring_k = 0.06;
                l.spring_len = 40.0;
                l.gravity = 0.008;
                l.damping = 0.92;
                l.dt = 0.025;
                l.steps_per_call = 4.0;
                l.cooling_alpha = 0.999;
                l.cooling_floor = 0.65;
                l.energy_threshold = 0.05;
            }
        }
        l.preset = self;
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
}

fn default_cooling_alpha() -> f32 { 0.998 }
fn default_cooling_floor() -> f32 { 0.55 }
fn default_energy_threshold() -> f32 { 0.05 }

impl Default for LayoutState {
    fn default() -> Self {
        // Tuned for 10k-node convergence: lower repulsion, stronger damping,
        // higher steps_per_call, plus cooling. Without these the sim just
        // orbits forever — kinetic energy never dissipates.
        Self {
            preset: LayoutPreset::default(),
            repulsion: 200.0,
            spring_k: 0.08,
            spring_len: 30.0,
            gravity: 0.005,
            damping: 0.78,
            dt: 0.04,
            steps_per_call: 8.0,
            cooling_alpha: 0.998,
            cooling_floor: 0.55,
            energy_threshold: 0.05,
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
    pub distance: f32,
    pub thickness: f32,
    pub blur: f32,
    pub max_coc: f32,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
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

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AppState {
    pub active_section: Option<Section>,
    pub style: StyleState,
    pub layout: LayoutState,
    pub camera: CameraState,
    pub focus: FocusState,
    pub cursor: CursorState,
    #[serde(default)]
    pub sim_status: SimStatus,
    #[serde(default)]
    pub query: QueryModel,
    #[serde(skip)]
    pub stats: LiveStats,
}

pub const STORAGE_KEY: &str = "graph_renderer_app_state_v1";
