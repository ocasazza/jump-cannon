use eframe::egui;
use crate::app::App;

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check if we need to apply a layout update due to debounced changes
        self.check_debounce_timer();
        
        // Request continuous redraw while debounce timer is active or animation is in progress
        if self.layout_debounce_timer.is_some() || self.animation_state.is_some() {
            ctx.request_repaint();
        }
        
        // Update animation if in progress
        if self.animation_state.is_some() {
            self.update_animation();
        }

        // Render the main UI
        super::render(self, ctx);
    }
}
