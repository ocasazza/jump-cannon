use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use crate::graph::interaction::SelectionState;
use crate::state::ui::UiState;
use crate::vault::VaultGraphResource;

pub fn modal_system(
    mut contexts: EguiContexts,
    selection: Res<SelectionState>,
    vault: Res<VaultGraphResource>,
    mut ui_state: ResMut<UiState>,
) {
    let Some(ref node_id) = selection.selected_node else { return };
    let Some(node) = vault.graph.nodes.get(node_id) else { return };
    if !ui_state.modal_open { return };

    let ctx = contexts.ctx_mut();

    let mut open = ui_state.modal_open;
    egui::Window::new(&node.meta.title)
        .open(&mut open)
        .resizable(true)
        .default_width(400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Basic info
                ui.heading("Metadata");
                egui::Grid::new("meta_grid").num_columns(2).striped(true).show(ui, |ui| {
                    ui.label("Path");
                    ui.monospace(&node.meta.path);
                    ui.end_row();

                    ui.label("Folder");
                    ui.label(&node.meta.folder);
                    ui.end_row();

                    if let Some(ref dt) = node.meta.doctype {
                        ui.label("Doctype");
                        ui.label(dt);
                        ui.end_row();
                    }

                    if !node.meta.tags.is_empty() {
                        ui.label("Tags");
                        ui.horizontal_wrapped(|ui| {
                            for tag in &node.meta.tags {
                                ui.label(egui::RichText::new(tag).monospace().small());
                            }
                        });
                        ui.end_row();
                    }
                });

                ui.separator();
                ui.heading("Graph Metrics");
                egui::Grid::new("metrics_grid").num_columns(2).striped(true).show(ui, |ui| {
                    ui.label("Degree"); ui.label(node.metrics.degree.to_string()); ui.end_row();
                    ui.label("In-degree"); ui.label(node.metrics.indegree.to_string()); ui.end_row();
                    ui.label("Out-degree"); ui.label(node.metrics.outdegree.to_string()); ui.end_row();
                    ui.label("PageRank"); ui.label(format!("{:.4}", node.metrics.pagerank)); ui.end_row();
                    ui.label("Betweenness"); ui.label(format!("{:.4}", node.metrics.betweenness)); ui.end_row();
                    ui.label("K-core"); ui.label(node.metrics.kcore.to_string()); ui.end_row();
                    ui.label("Community"); ui.label(node.metrics.community.to_string()); ui.end_row();
                    ui.label("WCC"); ui.label(node.metrics.wcc.to_string()); ui.end_row();
                });

                // Frontmatter
                if !node.meta.frontmatter.is_empty() {
                    ui.separator();
                    ui.heading("Frontmatter");
                    egui::Grid::new("fm_grid").num_columns(2).striped(true).show(ui, |ui| {
                        let mut keys: Vec<_> = node.meta.frontmatter.keys().collect();
                        keys.sort();
                        for k in keys {
                            ui.label(k);
                            ui.label(node.meta.frontmatter[k].to_string());
                            ui.end_row();
                        }
                    });
                }
            });
        });

    ui_state.modal_open = open;
}
