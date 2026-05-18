//! Active-filter chip bar.
//!
//! Previously rendered as a floating `egui::Area` anchored at
//! `Align2::CENTER_TOP` with `y=12` — which on real apps overlapped
//! (and was clipped by) any top panel egui_dock / eframe added above
//! the canvas. The user's complaint: "the tag filter bar at the top
//! is hidden / half off the top of the screen."
//!
//! Switched to `TopBottomPanel::top`. The panel participates in the
//! layout flow so it always sits below the menu bar / window chrome
//! and steals a few px from the canvas — never clipped. Mirrors the
//! status-footer pattern (`TopBottomPanel::bottom`) for symmetry.
//!
//! Hidden when no filters are active. Each chip is a
//! [`super::badge::Badge`] with the ✕ tail enabled; the field-name
//! lozenge sports its own ✕ that clears every value for that field.

use eframe::egui;
use eframe::egui::Ui;

use crate::ui::badge::{Badge, BadgeAction, BadgeKind};
use crate::ui::floating::FloatingPanel;
use crate::ui::query::QueryModel;
use crate::ui::state::{AppState, PanelId};
use crate::ui::theme::palette;

pub fn show(ctx: &egui::Context, query: &mut QueryModel) {
    let total: usize = query
        .active_filters
        .by_field
        .values()
        .map(|s| s.len())
        .sum();
    if total == 0 {
        return;
    }

    egui::TopBottomPanel::top("filter-chips")
        .resizable(false)
        .show_separator_line(true)
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(5, 7, 16, 235))
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
        )
        .show(ctx, |ui| {
            render_filter_chips(ui, query);
        });
}

/// Floating variant of the filter chip bar. Renders the same inner body
/// inside a `FloatingPanel` keyed by `PanelId::FilterStrip`. Hidden when
/// no filters are active (matches the docked variant).
pub fn show_floating(ctx: &egui::Context, state: &mut AppState) {
    let total: usize = state
        .query
        .active_filters
        .by_field
        .values()
        .map(|s| s.len())
        .sum();
    if total == 0 {
        return;
    }

    FloatingPanel::new(PanelId::FilterStrip, "Filters")
        .default_pos([16.0, 620.0])
        .default_size([600.0, 80.0])
        .show(ctx, &mut state.filter_strip_open, |ui| {
            render_filter_chips(ui, &mut state.query);
        });
}

fn render_filter_chips(ui: &mut Ui, query: &mut QueryModel) {
    let total: usize = query
        .active_filters
        .by_field
        .values()
        .map(|s| s.len())
        .sum();

    let mut to_clear_field: Option<String> = None;
    let mut to_toggle: Option<(String, String)> = None;
    let mut clear_all = false;

    ui.horizontal_wrapped(|ui| {
        // Render fields in user-insertion order.
        let order: Vec<String> = query
            .active_filters
            .insertion_order
            .iter()
            .filter(|f| query.active_filters.by_field.contains_key(*f))
            .cloned()
            .collect();
        for field in &order {
            // Field-name lozenge with ✕.
            let field_badge = Badge::new(field, field, BadgeKind::Generic)
                .with_x(true)
                .small(true);
            match field_badge.show(ui) {
                BadgeAction::Toggle { .. } => {
                    to_clear_field = Some(field.clone());
                }
                _ => {}
            }
            if let Some(values) = query.active_filters.by_field.get(field) {
                for v in values {
                    let kind = badge_kind_for(field, v);
                    let b = Badge::new(field, v, kind)
                        .active(true)
                        .with_x(true)
                        .small(true);
                    match b.show(ui) {
                        BadgeAction::Toggle { field, value } => {
                            to_toggle = Some((field, value));
                        }
                        _ => {}
                    }
                }
            }
            ui.add_space(6.0);
        }
        if total >= 2 {
            if ui.small_button("clear filters").clicked() {
                clear_all = true;
            }
        }
    });

    if clear_all {
        query.clear_all_filters();
        return;
    }
    if let Some(f) = to_clear_field {
        query.clear_field(&f);
    }
    if let Some((f, v)) = to_toggle {
        query.toggle_field_filter(&f, &v);
    }
}

fn badge_kind_for(field: &str, _value: &str) -> BadgeKind {
    match field {
        "tags" | "tag" => BadgeKind::Tag,
        "doctype" => BadgeKind::Doctype,
        "folder" => BadgeKind::Folder,
        "authors" => BadgeKind::Author,
        "entities" => BadgeKind::Entity { ty: None },
        "status" => BadgeKind::Status,
        _ => BadgeKind::Generic,
    }
}
