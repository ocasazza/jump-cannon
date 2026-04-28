use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ViewState {
    pub camera_x: f32,
    pub camera_y: f32,
    pub camera_zoom: f32,
    pub selected_node: Option<String>,
    pub search_query: String,
    pub focus_mode: bool,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
}

fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("jump-cannon").join("view.json")
}

pub fn load_view_state() -> ViewState {
    let path = config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_view_state(state: &ViewState) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
    }
}
