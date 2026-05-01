use eframe::egui;

use crate::ui::state::AppState;

use super::hint_label;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            state.cursor = Default::default();
        }
    });
    let c = &mut state.cursor;
    ui.add(egui::Slider::new(&mut c.radius, 1.0..=400.0).text("radius"));
    ui.add(egui::Slider::new(&mut c.strength, 0.0..=4.0).text("strength"));
    ui.add(egui::Slider::new(&mut c.depth, 0.0..=400.0).text("depth"));
    ui.add_space(8.0);
    hint_label(ui, "LMB = attract · RMB = repel");
}
