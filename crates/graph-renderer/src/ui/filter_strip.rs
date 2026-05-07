//! Top-center floating chip strip for active (field, value) filters.
//!
//! Anchored to `Align2::CENTER_TOP`. Hidden when no filters are active.
//! Each chip is a [`super::badge::Badge`] with the ✕ tail enabled; the
//! field-name lozenge sports its own ✕ that clears every value for that
//! field.

use eframe::egui::{self, Align2};

use crate::ui::badge::{Badge, BadgeAction, BadgeKind};
use crate::ui::query::QueryModel;
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

    let mut to_clear_field: Option<String> = None;
    let mut to_toggle: Option<(String, String)> = None;
    let mut clear_all = false;

    egui::Area::new("filter-chips".into())
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(5, 7, 16, 220))
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                .show(ui, |ui| {
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
                });
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
