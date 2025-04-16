use eframe::egui;
use crate::app::App;

// Export modules
pub mod app_impl;
pub mod app_utils;
pub mod view_controls;
pub mod global_options;
pub mod layout_options;
pub mod statistics;
pub mod graph_operations;
pub mod interactions;
pub mod graph_view;

/// Render the UI
pub fn render(app: &mut App, ctx: &egui::Context) {
    // Left panel for controls
    let _left_panel = egui::SidePanel::left("controls_panel").show(ctx, |ui| {
        ui.heading("Graph Controls");
        
        // View controls section
        view_controls::render(app, ui);
        
        // Global options section
        global_options::render(app, ui);
        
        // Layout options section
        layout_options::render(app, ui);
        
        // Graph statistics
        statistics::render(app, ui);
        
        // Graph operations section
        graph_operations::render(app, ui);
    });
    
    // Central panel for the graph
    let _central_panel = egui::CentralPanel::default().show(ctx, |ui| {
        // Render the graph and get the response for interactions
        let response = graph_view::render(app, ui);
        
        // Handle graph interactions
        interactions::handle_interactions(app, ctx, &response);
    });
}

/// Render the controls panel
fn render_controls(app: &mut App, ui: &mut egui::Ui) {
    view_controls::render(app, ui);
    global_options::render(app, ui);
    layout_options::render(app, ui);
    statistics::render(app, ui);
    graph_operations::render(app, ui);
}

/// Render global options
fn render_global_options(app: &mut App, ui: &mut egui::Ui) {
    global_options::render(app, ui);
}

/// Render layout options
fn render_layout_options(app: &mut App, ui: &mut egui::Ui) {
    layout_options::render(app, ui);
}

/// Render graph statistics
fn render_statistics(app: &mut App, ui: &mut egui::Ui) {
    statistics::render(app, ui);
}

/// Render the graph view
fn render_graph_view(app: &mut App, ui: &mut egui::Ui) {
    graph_view::render(app, ui);
}
