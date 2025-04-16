use eframe::egui;
use crate::app::App;

/// Handle graph interactions (zooming, panning, etc.)
pub fn handle_interactions(app: &mut App, ctx: &egui::Context, response: &egui::Response) {
    // Handle zooming with mouse wheel and trackpad pinch gestures
    if let Some(pos) = response.hover_pos() {
        ctx.input(|input| {
            // Handle scroll wheel zoom
            if input.scroll_delta.y != 0.0 && !input.modifiers.ctrl && !input.modifiers.command {
                // Calculate zoom factor based on scroll direction
                let zoom_factor = if input.scroll_delta.y > 0.0 { 1.1 } else { 0.9 };
                app.apply_zoom(pos, zoom_factor);
            }
            
            // Handle trackpad pinch gesture
            let zoom_delta = input.zoom_delta();
            if zoom_delta != 1.0 {
                app.apply_zoom(pos, zoom_delta as f64);
            }
            
            // Handle Ctrl+Scroll as an alternative zoom method
            if (input.modifiers.ctrl || input.modifiers.command) && input.scroll_delta.y != 0.0 {
                let zoom_factor = if input.scroll_delta.y > 0.0 { 1.1 } else { 0.9 };
                app.apply_zoom(pos, zoom_factor);
            }
        });
    }
    
    // Handle panning with mouse drag
    if response.dragged() {
        let delta = response.drag_delta();
        app.viewport.pan_x += delta.x as f64;
        app.viewport.pan_y += delta.y as f64;
    }
}
