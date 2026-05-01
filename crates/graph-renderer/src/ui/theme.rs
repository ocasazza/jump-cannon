//! Global egui theme for the graph-renderer chrome.
//!
//! High-contrast white-on-black, square corners, 1px white borders.
//! Accent palette is provided for semantic UI states (errors, warnings,
//! info, success) but should be used sparingly — the dominant aesthetic
//! is white-on-black.

use eframe::egui;

pub mod accent {
    use eframe::egui::Color32;
    pub const RED: Color32 = Color32::from_rgb(0xff, 0x3b, 0x3b);
    pub const GREEN: Color32 = Color32::from_rgb(0x3b, 0xff, 0x7a);
    pub const BLUE: Color32 = Color32::from_rgb(0x3b, 0x9b, 0xff);
    pub const YELLOW: Color32 = Color32::from_rgb(0xff, 0xd8, 0x3b);
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.override_text_color = Some(egui::Color32::WHITE);
    v.window_fill = egui::Color32::BLACK;
    v.panel_fill = egui::Color32::BLACK;
    v.window_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.menu_rounding = egui::Rounding::ZERO;
    v.window_rounding = egui::Rounding::ZERO;
    v.widgets.noninteractive.rounding = egui::Rounding::ZERO;
    v.widgets.inactive.rounding = egui::Rounding::ZERO;
    v.widgets.hovered.rounding = egui::Rounding::ZERO;
    v.widgets.active.rounding = egui::Rounding::ZERO;
    v.widgets.open.rounding = egui::Rounding::ZERO;

    let bg = egui::Color32::BLACK;
    let fg = egui::Color32::WHITE;
    let stroke = egui::Stroke::new(1.0, fg);
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.noninteractive.bg_stroke = stroke;
    v.widgets.noninteractive.fg_stroke = stroke;
    v.widgets.inactive.bg_fill = bg;
    v.widgets.inactive.bg_stroke = stroke;
    v.widgets.inactive.fg_stroke = stroke;
    v.widgets.inactive.weak_bg_fill = bg;
    v.widgets.hovered.bg_fill = fg;
    v.widgets.hovered.bg_stroke = stroke;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.hovered.weak_bg_fill = fg;
    v.widgets.active.bg_fill = fg;
    v.widgets.active.bg_stroke = stroke;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, bg);
    v.widgets.active.weak_bg_fill = fg;
    v.widgets.open.bg_fill = bg;

    v.selection.bg_fill = accent::BLUE;
    v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);

    // Square slider handles — matches the no-rounding aesthetic.
    v.slider_trailing_fill = false;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };

    // Typography hierarchy.
    // Section headers: 11px proportional (heading style).
    // Body / slider labels: 11px monospace (body style).
    // Small / hint text falls back to egui's default small size.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(11.0, FontFamily::Monospace),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(10.0, FontFamily::Proportional),
    );

    // Tighter vertical rhythm: 4px between items (slider-to-slider).
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(0.0);

    ctx.set_style(style);
}
