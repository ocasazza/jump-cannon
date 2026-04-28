use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use crate::state::ui::{UiState, SidebarTab};

pub fn sidebar_system(
    mut contexts: EguiContexts,
    mut state: ResMut<UiState>,
) {
    let ctx = contexts.ctx_mut();
    let sidebar_width = state.sidebar_width;

    egui::SidePanel::left("sidebar")
        .min_width(sidebar_width)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(state.active_tab == SidebarTab::Search, "Search").clicked() {
                    state.active_tab = SidebarTab::Search;
                }
                if ui.selectable_label(state.active_tab == SidebarTab::Info, "Info").clicked() {
                    state.active_tab = SidebarTab::Info;
                }
                if ui.selectable_label(state.active_tab == SidebarTab::Settings, "Settings").clicked() {
                    state.active_tab = SidebarTab::Settings;
                }
            });

            ui.separator();

            match state.active_tab {
                SidebarTab::Search => {
                    ui.label("Search the vault graph…");
                    ui.label("(search UI coming soon)");
                }
                SidebarTab::Info => {
                    ui.label("Node info panel.");
                    ui.label("Select a node to see details.");
                }
                SidebarTab::Settings => {
                    ui.label("Settings panel.");
                    ui.label("Layout and display options coming soon.");
                }
            }
        });
}
