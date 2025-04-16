use eframe::egui;
use crate::app::App;

/// Render the view controls section
pub fn render(app: &mut App, ui: &mut egui::Ui) {
    ui.collapsing("View Controls", |ui| {
        ui.horizontal(|ui| {
            if ui.button("Reset View").clicked() {
                // Reset the layout_applied flag so the graph will be centered
                app.layout_applied = false;
                app.center_graph();
            }
            
            // Get the center of the screen for zooming with buttons
            let screen_center = ui.available_rect_before_wrap().center();
            
            if ui.button("Zoom In").clicked() {
                app.apply_zoom(screen_center, 1.2);
            }
            
            if ui.button("Zoom Out").clicked() {
                app.apply_zoom(screen_center, 0.8);
            }
        });
        
        // Zoom slider with custom response handling
        let zoom_slider = egui::Slider::new(&mut app.viewport.zoom, 0.1..=5.0).text("Zoom");
        let zoom_response = ui.add(zoom_slider);
        
        // If the slider value changed, apply the zoom centered on the screen
        if zoom_response.changed() {
            // Store the current zoom value before it's modified by the slider
            let old_zoom = app.viewport.zoom;
            
            // Get the center of the screen for zooming
            let screen_center = ui.available_rect_before_wrap().center();
            
            // Calculate zoom factor based on the ratio of new zoom to old zoom
            let zoom_factor = app.viewport.zoom / old_zoom;
            
            // Apply the zoom centered on the screen
            app.apply_zoom(screen_center, zoom_factor);
        }
    });
}
