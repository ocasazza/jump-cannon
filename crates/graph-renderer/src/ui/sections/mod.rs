pub mod camera;
pub mod cursor;
pub mod debug;
pub mod filter;
pub mod focus;
pub mod instances;
pub mod layout;
pub mod stats;
pub mod style;

use eframe::egui;

use super::actions::ActionRegistry;
use super::layout::registry::LayoutRegistry;
use super::state::Section;
use super::theme;
use crate::perf::PerfCollector;

/// Section header: uppercase letter-spaced title flanked by thin lines.
///
/// Visual:  ─── STYLE ───
///
/// egui 0.30 has no per-glyph letter-spacing API; we approximate by
/// inserting a space between each character.
pub fn header(ui: &mut egui::Ui, label: &str) {
    let spaced: String = label
        .to_uppercase()
        .chars()
        .collect::<Vec<_>>()
        .chunks(1)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");

    ui.horizontal(|ui| {
        // Left rule.
        let line_color = egui::Color32::from_gray(80);
        let rule_h = 1.0_f32;
        let (rule_rect, _) = ui.allocate_exact_size(
            egui::vec2(12.0, rule_h),
            egui::Sense::hover(),
        );
        let mid_y = rule_rect.center().y;
        ui.painter().hline(rule_rect.x_range(), mid_y, egui::Stroke::new(rule_h, line_color));

        ui.label(
            egui::RichText::new(&spaced)
                .size(11.0)
                .strong()
                .color(egui::Color32::WHITE),
        );

        // Right rule — expand to fill remainder.
        let avail_w = ui.available_width().max(0.0);
        let (rule_rect, _) = ui.allocate_exact_size(
            egui::vec2(avail_w, rule_h),
            egui::Sense::hover(),
        );
        let mid_y = rule_rect.center().y;
        ui.painter().hline(rule_rect.x_range(), mid_y, egui::Stroke::new(rule_h, line_color));
    });

    ui.add_space(8.0);
}

/// Subgroup label — 10px, white at 0.6 alpha, uppercase letter-spaced.
pub fn subgroup_label(ui: &mut egui::Ui, label: &str) {
    let spaced: String = label
        .to_uppercase()
        .chars()
        .collect::<Vec<_>>()
        .chunks(1)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    ui.label(
        egui::RichText::new(spaced)
            .size(10.0)
            .color(egui::Color32::from_rgba_premultiplied(153, 153, 153, 153)),
    );
}

/// Dim horizontal divider between subgroups (0.3 alpha).
pub fn subgroup_separator(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().min.y;
    ui.painter().hline(
        rect.x_range(),
        y,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(77, 77, 77, 77)),
    );
    ui.add_space(6.0);
}

/// Hint / help text — 10px, italic, white at 0.5 alpha.
pub fn hint_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .italics()
            .color(egui::Color32::from_rgba_premultiplied(128, 128, 128, 128)),
    );
}

pub fn show(
    ui: &mut egui::Ui,
    section: Section,
    state: &mut super::state::AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    header(ui, section.title());
    match section {
        Section::Filter => filter::show(ui, state),
        Section::Style => style::show(ui, state),
        Section::Layout => layout::show(ui, state, layout_registry),
        Section::Camera => camera::show(ui, state),
        Section::Focus => focus::show(ui, state),
        Section::Cursor => cursor::show(ui, state),
        Section::Stats => stats::show(ui, state),
        Section::Instances => instances::show(ui, state, registry),
        Section::Debug => debug::show(ui, state, perf),
    }
    let _ = theme::accent::RED; // keep accent module referenced from here for tooling
}
