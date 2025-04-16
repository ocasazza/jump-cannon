use eframe::egui;
use crate::app::App;

/// Render the graph view
pub fn render(app: &mut App, ui: &mut egui::Ui) -> egui::Response {
    // Allocate space for the graph view
    let response = ui.allocate_rect(
        ui.available_rect_before_wrap(),
        egui::Sense::click_and_drag(),
    );
    
    // Render the graph
    app.render_graph(ui);
    
    response
}
