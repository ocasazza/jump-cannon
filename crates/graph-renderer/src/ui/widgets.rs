//! Reusable egui widget helpers shared across the UI surface.
//!
//! Anything that draws a "section header", "subgroup label", "hint
//! line", "subgroup separator", or "↺ Reset" right-aligned button
//! lives here so the chrome stays visually consistent. Callers in
//! `ui/sections/*`, `ui/layout/algorithms/*`, `ui/modal`, and
//! `ui/inspector` should pull these instead of redefining their own.

use eframe::egui;

use super::theme::{
    self,
    palette,
    spacing::{DIVIDER_GAP, SECTION_GAP},
};

/// Section header: title flanked by thin rules.
///
/// Visual:  ─── Style ───
///
/// The header used to render in `S T Y L E`-style faux letter-spacing
/// (uppercased + space-joined) but that was the only place in the UI
/// using that aesthetic — the modal, inspector, palette, and footer
/// all run normal-case — and the injected spaces caused egui's word
/// wrapper to break labels mid-word on narrow panels (`EDGE DISTANCE
/// RANGE` → `E D G E\nR A N\n…`). Plain title case keeps the section
/// chrome legible and consistent with everything else.
pub fn header(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        let line_color = palette::ICON;
        let rule_h = 1.0_f32;
        let (rule_rect, _) =
            ui.allocate_exact_size(egui::vec2(12.0, rule_h), egui::Sense::hover());
        let mid_y = rule_rect.center().y;
        ui.painter()
            .hline(rule_rect.x_range(), mid_y, egui::Stroke::new(rule_h, line_color));

        ui.label(
            egui::RichText::new(label)
                .size(theme::font_size::BODY)
                .strong()
                .color(palette::TEXT),
        );

        let avail_w = ui.available_width().max(0.0);
        let (rule_rect, _) =
            ui.allocate_exact_size(egui::vec2(avail_w, rule_h), egui::Sense::hover());
        let mid_y = rule_rect.center().y;
        ui.painter()
            .hline(rule_rect.x_range(), mid_y, egui::Stroke::new(rule_h, line_color));
    });

    ui.add_space(SECTION_GAP);
}

/// Subgroup label — small, dim, plain case.
pub fn subgroup_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        egui::RichText::new(label)
            .size(theme::font_size::SMALL)
            .color(theme::subgroup_label_color()),
    );
}

/// Faint horizontal rule between sub-groups within a section.
pub fn subgroup_separator(ui: &mut egui::Ui) {
    ui.add_space(DIVIDER_GAP);
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().min.y;
    ui.painter().hline(
        rect.x_range(),
        y,
        egui::Stroke::new(1.0, theme::subgroup_separator_color()),
    );
    ui.add_space(DIVIDER_GAP);
}

/// Right-aligned "↺ Reset" button. Returns true on click.
///
/// The outer `ui.horizontal` constrains the row to one button-height
/// before the inner `right_to_left` layout glues the button to the
/// right edge — without it, `right_to_left` claims the entire
/// remaining vertical space and pushes everything below off-screen.
pub fn reset_row(ui: &mut egui::Ui) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("↺ Reset").clicked() {
                clicked = true;
            }
        });
    });
    clicked
}

/// Single-row labelled control: label flush-left, control flush-right.
///
/// Visual:  `Edge width (px)         [─── slider ───]`
///
/// egui's `Slider` doesn't auto-grow inside a `right_to_left` layout
/// — it pulls from `style.spacing.slider_width` (default ~100 px).
/// To make the slider span the gap between the label and the panel
/// edge we override `slider_width` *inside* the right-aligned closure
/// with the remaining width minus a small padding for the value text.
/// Combo-boxes, color pickers, and checkboxes ignore `slider_width`,
/// so they just sit flush right at their natural size.
pub fn row(ui: &mut egui::Ui, label: &str, add_control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        if !label.is_empty() {
            ui.label(
                egui::RichText::new(label)
                    .size(theme::font_size::SMALL)
                    .color(theme::subgroup_label_color()),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Reserve ~48 px for the slider's trailing value text so the
            // numeric readout doesn't get clipped against the panel edge.
            let avail = ui.available_width();
            ui.style_mut().spacing.slider_width = (avail - 48.0).max(60.0);
            add_control(ui);
        });
    });
}

/// Hint / help text — 10 px, italic, dim.
pub fn hint_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .italics()
            .color(theme::hint_label_color()),
    );
}
