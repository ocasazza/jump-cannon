use eframe::egui;
use crate::app::App;

/// Render the global options section
pub fn render(app: &mut App, ui: &mut egui::Ui) {
    ui.collapsing("Global Options", |ui| {
        ui.add(egui::Slider::new(&mut app.global_options.node_size, 1.0..=50.0).text("Node Size"));
        ui.add(egui::Slider::new(&mut app.global_options.edge_width, 0.5..=10.0).text("Edge Width"));
        ui.checkbox(&mut app.global_options.show_labels, "Show Labels");
        if app.global_options.show_labels {
            ui.add(egui::Slider::new(&mut app.global_options.label_size, 8.0..=24.0).text("Label Size"));
        }
        ui.checkbox(&mut app.global_options.dark_mode, "Dark Mode");
    });
}
