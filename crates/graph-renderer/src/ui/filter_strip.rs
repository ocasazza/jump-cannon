//! Active-filter chip bar.
//!
//! Floating-panel form for the active (field, value) chips. Renders
//! one row per field — each labelled with the field name, a per-field
//! `any / all` combinator toggle, and the value chips themselves. The
//! panel header carries the cross-field combinator and the
//! [`FilterBehavior`] toggle (Filter = discard non-matches, Focus =
//! dim non-matches).
//!
//! The legacy docked variant (`TopBottomPanel::top`) is preserved as
//! [`show`] for the rare caller that still wants an inline strip; it
//! intentionally omits the combinator controls — the docked strip is
//! a "read-only" indicator only.
//!
//! Chips reflow inside the panel via grouped rows + `horizontal_wrapped`,
//! all wrapped in a vertical `ScrollArea` so dense filter sets stay
//! reachable when the panel is shrunk.

use eframe::egui;
use eframe::egui::Ui;

use crate::ui::badge::{Badge, BadgeAction, BadgeKind};
use crate::ui::floating::FloatingPanel;
use crate::ui::query::{Combinator, QueryModel};
use crate::ui::state::{AppState, FilterBehavior, PanelId};
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
        .default_size([420.0, 360.0])
        .show(ctx, &mut state.filter_strip_open, |ui| {
            render_floating_body(ui, &mut state.query, &mut state.filter_behavior);
        });
}

/// Header (cross-field combinator + behavior toggle) followed by the
/// chip group, all inside a vertical `ScrollArea` so the panel stays
/// usable at any height.
fn render_floating_body(
    ui: &mut Ui,
    query: &mut QueryModel,
    behavior: &mut FilterBehavior,
) {
    render_header(ui, query, behavior);
    ui.add_space(4.0);
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            render_grouped_chips(ui, query);
        });
}

fn render_header(
    ui: &mut Ui,
    query: &mut QueryModel,
    behavior: &mut FilterBehavior,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("match").color(palette::TEXT));
        let cross_label = format!("{} fields", query.active_filters.cross_field_combinator.label());
        let cross_resp = ui
            .small_button(cross_label)
            .on_hover_text(
                "How field-level results combine: `all` = AND (intersect), \
                 `any` = OR (union).",
            );
        if cross_resp.clicked() {
            query.active_filters.cross_field_combinator =
                query.active_filters.cross_field_combinator.toggled();
        }

        ui.separator();

        // Filter / Focus toggle.
        ui.label(egui::RichText::new("when matched:").color(palette::TEXT));
        let beh_resp = ui
            .small_button(behavior.label())
            .on_hover_text(behavior.tooltip());
        if beh_resp.clicked() {
            *behavior = behavior.toggled();
        }

        // Clear-all on the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let total: usize = query
                .active_filters
                .by_field
                .values()
                .map(|s| s.len())
                .sum();
            if total >= 2 && ui.small_button("clear filters").clicked() {
                query.clear_all_filters();
            }
        });
    });
}

/// One row per field: `field_name [any|all] [chip] [chip] ...`. Each
/// row wraps inside its own `horizontal_wrapped` block, so chip lines
/// reflow with the panel width independently per field.
fn render_grouped_chips(ui: &mut Ui, query: &mut QueryModel) {
    let mut to_clear_field: Option<String> = None;
    let mut to_toggle: Option<(String, String)> = None;
    let mut to_set_combinator: Option<(String, Combinator)> = None;

    let order: Vec<String> = query
        .active_filters
        .insertion_order
        .iter()
        .filter(|f| query.active_filters.by_field.contains_key(*f))
        .cloned()
        .collect();

    for field in &order {
        // Snapshot per-field state for read-only use below; mutations
        // are deferred into the `to_*` channels.
        let combinator = query.active_filters.combinator_for(field);
        let values: Vec<String> = query
            .active_filters
            .by_field
            .get(field)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let multi = values.len() > 1;

        ui.horizontal_wrapped(|ui| {
            // Field-name lozenge with ✕.
            let field_badge = Badge::new(field, field, BadgeKind::Generic)
                .with_x(true)
                .small(true);
            if let BadgeAction::Toggle { .. } = field_badge.show(ui) {
                to_clear_field = Some(field.clone());
            }

            // Per-field combinator toggle. Only meaningful when ≥ 2
            // values are selected; we still render it (greyed) for
            // single-value fields so the affordance stays visible —
            // see `Combinator` docs on the single-value degenerate
            // case.
            let label = combinator.label();
            let mut btn = egui::Button::new(
                egui::RichText::new(label)
                    .small()
                    .color(if multi { palette::TEXT } else { palette::GREY }),
            )
            .small();
            if !multi {
                btn = btn.fill(egui::Color32::TRANSPARENT);
            }
            let resp = ui
                .add(btn)
                .on_hover_text(
                    "Toggle how this field's values combine: \
                     `any` = OR (match any value), `all` = AND (match all values).",
                );
            if resp.clicked() {
                to_set_combinator = Some((field.clone(), combinator.toggled()));
            }

            // Value chips.
            for v in &values {
                let kind = badge_kind_for(field, v);
                let b = Badge::new(field, v, kind)
                    .active(true)
                    .with_x(true)
                    .small(true);
                if let BadgeAction::Toggle { field, value } = b.show(ui) {
                    to_toggle = Some((field, value));
                }
            }
        });
        ui.add_space(2.0);
    }

    if let Some(f) = to_clear_field {
        query.clear_field(&f);
    }
    if let Some((f, c)) = to_set_combinator {
        query.active_filters.set_combinator_for(&f, c);
    }
    if let Some((f, v)) = to_toggle {
        query.toggle_field_filter(&f, &v);
    }
}

/// Inline (docked-strip) renderer. Kept around for the legacy
/// `TopBottomPanel` mount — no combinator controls, just the chips.
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
        let order: Vec<String> = query
            .active_filters
            .insertion_order
            .iter()
            .filter(|f| query.active_filters.by_field.contains_key(*f))
            .cloned()
            .collect();
        for field in &order {
            let field_badge = Badge::new(field, field, BadgeKind::Generic)
                .with_x(true)
                .small(true);
            if let BadgeAction::Toggle { .. } = field_badge.show(ui) {
                to_clear_field = Some(field.clone());
            }
            if let Some(values) = query.active_filters.by_field.get(field) {
                for v in values {
                    let kind = badge_kind_for(field, v);
                    let b = Badge::new(field, v, kind)
                        .active(true)
                        .with_x(true)
                        .small(true);
                    if let BadgeAction::Toggle { field, value } = b.show(ui) {
                        to_toggle = Some((field, value));
                    }
                }
            }
            ui.add_space(6.0);
        }
        if total >= 2 && ui.small_button("clear filters").clicked() {
            clear_all = true;
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
