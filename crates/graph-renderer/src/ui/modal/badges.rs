//! Inline badge widgets used by the metadata modal — chips that look
//! like tag pills but with semantic colour for dates / tickets / status
//! pills. The `Badge` API in `ui::badge` is the structured/clickable
//! variant; these are simpler one-shot buttons used directly by the
//! frontmatter renderer when no toggle/halo state is needed.

use eframe::egui;

use crate::ui::theme::{accent, palette};

pub(crate) fn plain_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, palette::BORDER))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

pub(crate) fn date_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, accent::YELLOW))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

pub(crate) fn ticket_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label)
        .monospace()
        .small()
        .color(accent::YELLOW);
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, accent::YELLOW))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

pub(crate) fn status_pill(
    ui: &mut egui::Ui,
    label: &str,
    color: egui::Color32,
) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small().color(color);
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, color))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

pub(crate) fn status_color(s: &str) -> Option<egui::Color32> {
    match s.to_ascii_lowercase().as_str() {
        "active" | "done" | "ok" | "ready" | "passed" => Some(accent::GREEN),
        "failed" | "blocked" | "broken" | "error" => Some(accent::RED),
        "needs-review" | "needs-fetch" | "in-progress" | "wip" => Some(accent::YELLOW),
        "draft" | "pending" => Some(accent::BLUE),
        "archived" | "deprecated" | "stale" => Some(egui::Color32::WHITE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_palette() {
        assert!(status_color("active").is_some());
        assert!(status_color("DONE").is_some());
        assert!(status_color("failed").is_some());
        assert!(status_color("needs-review").is_some());
        assert!(status_color("draft").is_some());
        assert!(status_color("archived").is_some());
        assert!(status_color("foobar").is_none());
    }
}
