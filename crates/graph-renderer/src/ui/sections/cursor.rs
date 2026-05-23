use eframe::egui;

use crate::ui::state::AppState;

use super::{hint_label, reset_row, row};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Cursor".into());
    if reset_row(ui) {
        state.cursor = Default::default();
    }
    let c = &mut state.cursor;
    row(ui, "radius",   |ui| { ui.add(egui::Slider::new(&mut c.radius, 1.0..=400.0)); });
    row(ui, "strength", |ui| { ui.add(egui::Slider::new(&mut c.strength, 0.0..=4.0)); });
    row(ui, "depth",    |ui| { ui.add(egui::Slider::new(&mut c.depth, 0.0..=400.0)); });
    ui.add_space(8.0);
    hint_label(ui, "LMB = attract · RMB = repel");
}
