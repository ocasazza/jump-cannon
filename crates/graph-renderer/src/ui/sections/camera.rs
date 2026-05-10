use eframe::egui;

use crate::ui::state::AppState;
use crate::ui::theme::accent;

use super::{reset_row, row, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if reset_row(ui) {
        state.camera = Default::default();
    }
    ui.horizontal(|ui| {
        let _ = ui.button("Fit");
        let _ = ui.button("Reset");
    });

    subgroup_separator(ui);

    let c = &mut state.camera;
    row(ui, "Invert mouse X", |ui| { ui.checkbox(&mut c.invert_mouse_x, ""); });
    row(ui, "Invert mouse Y", |ui| { ui.checkbox(&mut c.invert_mouse_y, ""); });
    row(ui, "Invert A/D",     |ui| { ui.checkbox(&mut c.invert_ad, ""); });
    row(ui, "Invert Q/E",     |ui| { ui.checkbox(&mut c.invert_qe, ""); });

    ui.add_space(10.0);

    // Follow centroid: blue tint on the row label when active.
    let _ = accent::BLUE;
    row(ui, "Follow centroid", |ui| { ui.checkbox(&mut c.follow_centroid, ""); });
    row(ui, "Fit to window",   |ui| { ui.checkbox(&mut c.fit_to_window, ""); });
}
