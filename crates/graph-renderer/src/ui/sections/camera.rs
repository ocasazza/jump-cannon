use eframe::egui;

use crate::ui::state::AppState;
use crate::ui::theme::accent;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let _ = ui.button("Fit");
        let _ = ui.button("Reset");
    });
    ui.add_space(10.0);

    let c = &mut state.camera;
    ui.checkbox(&mut c.invert_mouse_x, "Invert mouse X");
    ui.checkbox(&mut c.invert_mouse_y, "Invert mouse Y");
    ui.checkbox(&mut c.invert_ad, "Invert A/D");
    ui.checkbox(&mut c.invert_qe, "Invert Q/E");

    ui.add_space(10.0);

    // Follow centroid: blue tint on the label when active.
    let follow_label = if c.follow_centroid {
        egui::RichText::new("Follow centroid").color(accent::BLUE)
    } else {
        egui::RichText::new("Follow centroid")
    };
    ui.checkbox(&mut c.follow_centroid, follow_label);
    ui.checkbox(&mut c.fit_to_window, "Fit to window");
}
