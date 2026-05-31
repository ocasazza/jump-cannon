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
use crate::ui::state::{AppState, FilterBehavior, FocusedPanel, FrontendEventLog, PanelId};
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
    // Tiled mode → workspace tree owns the rendering, bail.
    if state.filter_strip_placement == crate::ui::tiles::Placement::Tiled {
        return;
    }

    // Attribute any mutations made inside the floating filter chip
    // panel (chip toggles, combinator flips, behavior toggle, clear-
    // all). `tick_snapshots` drains this every frame so a hover with
    // no actual mutation can't bleed onto a later unrelated diff.
    state.snapshot_source = Some("Filter: chip".into());

    let mut placement = state.filter_strip_placement;
    let placement_before = placement;
    let mut focused = std::mem::take(&mut state.focused_panel);
    FloatingPanel::new(PanelId::FilterStrip, "Filters")
        .default_pos([16.0, 620.0])
        .default_size([420.0, 360.0])
        .with_placement(&mut placement)
        .with_focus(&mut focused, FocusedPanel::FilterStrip)
        .show(ctx, &mut state.filter_strip_open, |ui| {
            render_floating_body(
                ui,
                &mut state.query,
                &mut state.filter_behavior,
                &mut state.frontend_events,
            );
        });
    state.focused_panel = focused;
    // Closing the focused filter strip drops focus to the canvas.
    if !state.filter_strip_open && state.focused_panel == Some(FocusedPanel::FilterStrip) {
        state.focused_panel = None;
    }
    if placement != placement_before {
        state.filter_strip_placement = placement;
        if placement == crate::ui::tiles::Placement::Tiled {
            let mut ws = std::mem::take(&mut state.tiles);
            ws.snap_insert(crate::ui::tiles::PaneKind::FilterStrip);
            state.tiles = ws;
        }
    }
}

/// Tiled-mode body for the filter strip — same content as the floating
/// variant, but with no FloatingPanel wrapper (egui_tiles owns the
/// chrome).
pub fn render_tiled_body(ui: &mut Ui, state: &mut AppState) {
    let total: usize = state
        .query
        .active_filters
        .by_field
        .values()
        .map(|s| s.len())
        .sum();
    if total == 0 {
        ui.label(
            egui::RichText::new("no active filters")
                .color(palette::GREY)
                .small(),
        );
        return;
    }
    render_floating_body(
        ui,
        &mut state.query,
        &mut state.filter_behavior,
        &mut state.frontend_events,
    );
}

/// Header (cross-field combinator + behavior toggle) followed by the
/// chip group, all inside a vertical `ScrollArea` so the panel stays
/// usable at any height.
fn render_floating_body(
    ui: &mut Ui,
    query: &mut QueryModel,
    behavior: &mut FilterBehavior,
    events: &mut FrontendEventLog,
) {
    render_header(ui, query, behavior, events);
    ui.add_space(4.0);
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            render_grouped_chips(ui, query, events);
        });
}

fn render_header(
    ui: &mut Ui,
    query: &mut QueryModel,
    behavior: &mut FilterBehavior,
    events: &mut FrontendEventLog,
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
            events.push(
                "filter-strip",
                format!(
                    "cross-field combinator -> {}",
                    query.active_filters.cross_field_combinator.label()
                ),
            );
        }

        ui.separator();

        // Filter / Focus toggle.
        ui.label(egui::RichText::new("when matched:").color(palette::TEXT));
        let beh_resp = ui
            .small_button(behavior.label())
            .on_hover_text(behavior.tooltip());
        if beh_resp.clicked() {
            *behavior = behavior.toggled();
            events.push(
                "filter-strip",
                format!("behavior -> {}", behavior.label()),
            );
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
                events.push("filter-strip", "clear all filters");
            }
        });
    });
}

/// One row per field: `field_name [any|all] [chip] [chip] ...`. Each
/// row wraps inside its own `horizontal_wrapped` block, so chip lines
/// reflow with the panel width independently per field.
fn render_grouped_chips(ui: &mut Ui, query: &mut QueryModel, events: &mut FrontendEventLog) {
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
        events.push("filter-strip", format!("clear field {f}"));
        query.clear_field(&f);
    }
    if let Some((f, c)) = to_set_combinator {
        events.push(
            "filter-strip",
            format!("{f} combinator -> {}", c.label()),
        );
        query.active_filters.set_combinator_for(&f, c);
    }
    if let Some((f, v)) = to_toggle {
        events.push("filter-strip", format!("toggle chip {f}={v}"));
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
