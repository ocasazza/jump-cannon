pub mod camera;
pub mod cursor;
pub mod filter;
pub mod focus;
pub mod layout;
pub mod stats;
pub mod style;

use eframe::egui;

use super::state::Section;
use super::theme;

/// Tiny uppercase section header. egui 0.30 has no per-glyph
/// letter-spacing API, so we synthesize it by spacing the chars manually.
pub fn header(ui: &mut egui::Ui, label: &str) {
    let spaced: String = label
        .to_uppercase()
        .chars()
        .collect::<Vec<_>>()
        .chunks(1)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    let rich = egui::RichText::new(spaced)
        .size(10.0)
        .color(egui::Color32::from_gray(200));
    ui.add(egui::Label::new(rich));
    ui.add_space(2.0);
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().min.y;
    ui.painter().hline(
        rect.x_range(),
        y,
        egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
    );
    ui.add_space(8.0);
}

pub fn show(ui: &mut egui::Ui, section: Section, state: &mut super::state::AppState) {
    header(ui, section.title());
    match section {
        Section::Filter => filter::show(ui, state),
        Section::Style => style::show(ui, state),
        Section::Layout => layout::show(ui, state),
        Section::Camera => camera::show(ui, state),
        Section::Focus => focus::show(ui, state),
        Section::Cursor => cursor::show(ui, state),
        Section::Stats => stats::show(ui, state),
    }
    let _ = theme::accent::RED; // keep accent module referenced from here for tooling
}
