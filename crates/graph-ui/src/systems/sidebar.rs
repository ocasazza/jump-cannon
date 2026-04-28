use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use crate::state::ui::{UiState, SidebarTab};
use crate::graph::interaction::SelectionState;
use crate::vault::VaultGraphResource;

pub fn sidebar_system(
    mut contexts: EguiContexts,
    mut state: ResMut<UiState>,
    mut selection: ResMut<SelectionState>,
    vault: Res<VaultGraphResource>,
) {
    let ctx = contexts.ctx_mut();
    let sidebar_width = state.sidebar_width;

    egui::SidePanel::left("sidebar")
        .min_width(sidebar_width)
        .show(ctx, |ui| {
            // Tab bar
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
                    ui.heading("Search");

                    // Text input — ResMut change detection picks up mutations automatically.
                    ui.text_edit_singleline(&mut state.search_query);

                    ui.checkbox(&mut state.focus_mode, "Focus mode");

                    ui.separator();

                    // Results list (up to 20 entries shown)
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let results: Vec<String> = state.search_results
                            .iter()
                            .take(20)
                            .cloned()
                            .collect();

                        for node_id in &results {
                            if let Some(node) = vault.graph.nodes.get(node_id) {
                                let label = if node.meta.tags.is_empty() {
                                    node.meta.title.clone()
                                } else {
                                    format!("{} [{}]", node.meta.title, node.meta.tags.join(", "))
                                };

                                let is_selected = selection.selected_node.as_deref() == Some(node_id);
                                if ui.selectable_label(is_selected, label).clicked() {
                                    selection.selected_node = Some(node_id.clone());
                                    state.modal_open = true;
                                }
                            }
                        }
                    });
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
