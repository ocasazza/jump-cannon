use eframe::egui;

use crate::ui::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("CURSOR");
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            state.cursor = Default::default();
        }
    });
    ui.add_space(4.0);

    let c = &mut state.cursor;
    ui.add(egui::Slider::new(&mut c.radius, 1.0..=400.0).text("radius"));
    ui.add(egui::Slider::new(&mut c.strength, 0.0..=4.0).text("strength"));
    ui.add(egui::Slider::new(&mut c.depth, 0.0..=400.0).text("depth"));
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("LMB = attract · RMB = repel")
            .italics()
            .color(egui::Color32::from_gray(150))
            .size(11.0),
    );
}
