use eframe::egui;

use crate::ui::state::AppState;
use crate::ui::theme::accent;

pub fn show(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.label(
        egui::RichText::new("Card-stream query builder coming in Phase F.")
            .italics()
            .color(egui::Color32::from_gray(150))
            .size(11.0),
    );
    ui.add_space(8.0);
    ui.add_enabled(false, egui::Button::new("Add filter"));
    ui.add_space(12.0);
    let danger = egui::RichText::new("Clear all filters")
        .color(accent::RED)
        .size(11.0);
    ui.add_enabled(false, egui::Button::new(danger));
}
