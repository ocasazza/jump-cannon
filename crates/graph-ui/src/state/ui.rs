use bevy::prelude::*;

#[derive(Resource)]
pub struct UiState {
    pub palette_open: bool,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub active_tab: SidebarTab,
    pub modal_open: bool,
    pub search_query: String,
    pub search_results: Vec<String>,
    pub focus_mode: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            palette_open: false,
            sidebar_open: true,
            sidebar_width: 240.0,
            active_tab: SidebarTab::default(),
            modal_open: false,
            search_query: String::new(),
            search_results: vec![],
            focus_mode: false,
        }
    }
}

#[derive(Default, PartialEq, Debug)]
pub enum SidebarTab {
    #[default]
    Search,
    Info,
    Settings,
}
