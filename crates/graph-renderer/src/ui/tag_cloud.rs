//! Always-visible tag-cloud surface anchored to the top-left of the
//! canvas.
//!
//! Renders the top-N tag values across the loaded vault as clickable
//! [`super::badge::Badge`] chips — exists from app boot, **does not**
//! require a node selection or any user interaction to materialize.
//! This is the chip surface the user actually sees on first load; it
//! exists so badges are a first-class part of the renderer's UI rather
//! than a sub-surface of the modal/inspector chains.
//!
//! Data source: [`crate::ui::field_index::FieldIndex`], populated by
//! the `/graph/meta_summary` fetch at boot. Tags are sorted by
//! frequency (count of nodes carrying each tag) descending, then
//! alphabetically as a stable tiebreak.
//!
//! Click behaviour:
//!   - body-click: toggle a `tags=<value>` filter on `QueryModel`
//!   - `+` affordance: same as body-click for now (no separate "focus
//!     a node" meaning at the cloud level — these chips aren't
//!     attached to a single node)
//!
//! The `active` state of each chip reflects whether the corresponding
//! filter is currently engaged so the user can see at a glance which
//! tags are in the active query.

use eframe::egui::{self, Align2};

use crate::ui::badge::{Badge, BadgeAction, BadgeKind};
use crate::ui::field_index::FieldIndex;
use crate::ui::query::QueryModel;
use crate::ui::theme::palette;

/// Max chips to display in the cloud — keeps the surface from
/// dominating the canvas on big vaults. Top-N by frequency.
const MAX_CHIPS: usize = 24;

/// Render the tag cloud as a floating panel anchored to the top-left
/// of the canvas. Returns silently if no field index has loaded yet
/// (the boot fetch hasn't returned) or if the index has no `tags`
/// field.
pub fn show(
    ctx: &egui::Context,
    field_index: Option<&FieldIndex>,
    query: &mut QueryModel,
) {
    let Some(fi) = field_index else {
        return;
    };
    let Some(tag_buckets) = fi.by_field.get("tags") else {
        return;
    };
    if tag_buckets.is_empty() {
        return;
    }

    // Sort by frequency desc, then alphabetical asc for a stable
    // ordering that doesn't reshuffle when two tags share the same
    // count (which is common on small vaults).
    let mut ranked: Vec<(&String, usize)> = tag_buckets
        .iter()
        .map(|(v, idxs)| (v, idxs.len()))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked.truncate(MAX_CHIPS);

    let mut to_toggle: Option<(String, String)> = None;

    egui::Area::new("tag-cloud".into())
        .anchor(Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(5, 7, 16, 220))
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("Tags ({})", tag_buckets.len()))
                            .small()
                            .color(palette::GREY),
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for (value, count) in &ranked {
                            let active = query
                                .active_filters
                                .by_field
                                .get("tags")
                                .map(|s| s.contains(*value))
                                .unwrap_or(false);
                            // Show count as a parenthetical so the
                            // user can scan tag frequency without
                            // hovering for a tooltip.
                            let label = format!("{} ({})", value, count);
                            let b = Badge::new("tags", &label, BadgeKind::Tag)
                                .active(active)
                                .small(true);
                            // The label string lives only for this
                            // frame; the Toggle action we get back
                            // would carry that "value (count)"
                            // composite. Override by emitting the
                            // raw value ourselves on click.
                            if let BadgeAction::Toggle { .. } = b.show(ui) {
                                to_toggle = Some(("tags".to_string(), (*value).clone()));
                            }
                        }
                    });
                });
        });

    if let Some((f, v)) = to_toggle {
        query.toggle_field_filter(&f, &v);
    }
}
