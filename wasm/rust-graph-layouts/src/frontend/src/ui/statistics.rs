use eframe::egui;
use crate::app::App;

/// Render the graph statistics section
pub fn render(app: &mut App, ui: &mut egui::Ui) {
    ui.collapsing("Graph Statistics", |ui| {
        ui.label(format!("Nodes: {}", app.graph.nodes.len()));
        ui.label(format!("Edges: {}", app.graph.edges.len()));
        ui.label(format!("Selected Nodes: {}", app.selected_nodes.len()));
        ui.label(format!("Selected Edges: {}", app.selected_edges.len()));
    });
}
