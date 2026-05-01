//! UI state. Owned by `App`, persisted via `eframe::Storage` as JSON.
//!
//! Phase D is UI-only: every field here is bound to an egui widget but
//! nothing yet reads these values to drive a render. Phases B/C/F wire
//! the actual graph render, data fetch, and query builder.

use serde::{Deserialize, Serialize};

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
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            preset: LayoutPreset::default(),
            repulsion: 800.0,
            spring_k: 0.05,
            spring_len: 60.0,
            gravity: 0.02,
            damping: 0.85,
            dt: 0.016,
            steps_per_call: 4.0,
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
}

pub const STORAGE_KEY: &str = "graph_renderer_app_state_v1";
