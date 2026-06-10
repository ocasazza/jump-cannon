//! Filters (chip strip) panel — Dioxus port of crates/graph-renderer/src/ui/filter_strip.rs.
//!
//! Renders the active (field, value) chips: one row per field — the field
//! name as a removable lozenge, a per-field `any / all` combinator toggle,
//! and the value chips. The header carries the cross-field combinator and
//! the Filter/Focus behavior toggle; clear-all appears from two chips up.
//! Shows "no active filters" when the set is empty (the tiled-body variant
//! — panel-kit owns the floating/tiled chrome the egui FloatingPanel did).
//!
//! All state is shared with the Filter panel via `crate::panels::filter`'s
//! `GlobalSignal`s; every mutation routes through `filter::edit_filters` so
//! persistence + the GPU mask push stay in one place.

use dioxus::prelude::*;

use crate::panels::filter;
use crate::Ctx;

/// Chip tint per field — port of filter_strip.rs::badge_kind_for + the
/// badge.rs color table (classes live under this panel's CSS anchor).
fn chip_class(field: &str) -> &'static str {
    match field {
        "tags" | "tag" => "chip-tag",
        "doctype" => "chip-doctype",
        "folder" => "chip-folder",
        "authors" => "chip-author",
        "entities" => "chip-entity",
        "status" => "chip-status",
        _ => "chip-generic",
    }
}

pub fn panel(_ctx: Ctx) -> Element {
    // Opening the strip alone must still arm the meta_summary fetch — the
    // chips resolve to node sets through the shared FieldIndex.
    filter::ensure_field_index();

    let q = filter::QUERY.read().clone();
    let behavior = *filter::BEHAVIOR.read();
    let total: usize = q.active_filters.by_field.values().map(|s| s.len()).sum();
    if total == 0 {
        return rsx! { div { class: "fst-empty", "no active filters" } };
    }

    let cross = q.active_filters.cross_field_combinator;
    let cross_label = format!("{} fields", cross.label());
    let behavior_label = behavior.label();
    let behavior_tip = behavior.tooltip();

    // Fields render in user-insertion order, not BTreeMap name order.
    let order: Vec<String> = q
        .active_filters
        .insertion_order
        .iter()
        .filter(|f| q.active_filters.by_field.contains_key(*f))
        .cloned()
        .collect();

    rsx! {
        div { class: "fst",
            div { class: "fst-head",
                span { class: "fst-k", "match" }
                button { class: "fst-toggle",
                    title: "How field-level results combine: `all` = AND (intersect), `any` = OR (union).",
                    onclick: move |_| {
                        let next = cross.toggled();
                        tracing::info!("[filter-strip] cross-field combinator -> {}", next.label());
                        filter::edit_filters(move |q| q.active_filters.cross_field_combinator = next);
                    },
                    "{cross_label}"
                }
                span { class: "fst-sep" }
                // Filter / Focus toggle.
                span { class: "fst-k", "when matched:" }
                button { class: "fst-toggle", title: "{behavior_tip}",
                    onclick: move |_| {
                        filter::toggle_behavior();
                        tracing::info!(
                            "[filter-strip] behavior -> {}",
                            filter::BEHAVIOR.peek().label()
                        );
                    },
                    "{behavior_label}"
                }
                // Clear-all on the right — only worth a button from 2 chips up.
                if total >= 2 {
                    button { class: "fst-toggle fst-clear",
                        onclick: move |_| {
                            tracing::info!("[filter-strip] clear all filters");
                            filter::edit_filters(|q| q.clear_all_filters());
                        },
                        "clear filters"
                    }
                }
            }
            div { class: "fst-rows",
                for field in order {
                    { field_row(&q, field) }
                }
            }
        }
    }
}

/// One row per field: `field_name [any|all] [chip] [chip] …` — chips wrap
/// per field so dense filter sets reflow with the panel width.
fn field_row(q: &filter::QueryModel, field: String) -> Element {
    let combinator = q.active_filters.combinator_for(&field);
    let comb_label = combinator.label();
    let values: Vec<String> = q
        .active_filters
        .by_field
        .get(&field)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    // Only meaningful when ≥ 2 values are selected; still rendered (greyed)
    // for single-value fields so the affordance stays visible.
    let multi = values.len() > 1;
    let f_clear = field.clone();
    let f_comb = field.clone();
    rsx! {
        div { class: "fst-row", key: "{field}",
            // Field-name lozenge with ✕ — clears the whole field.
            button { class: "fst-chip chip-generic active", title: "Clear this field",
                onclick: move |_| {
                    tracing::info!("[filter-strip] clear field {f_clear}");
                    filter::edit_filters(|q| q.clear_field(&f_clear));
                },
                span { "{field}" }
                span { class: "fst-x", "×" }
            }
            button {
                class: if multi { "fst-toggle fst-comb" } else { "fst-toggle fst-comb dim" },
                title: "Toggle how this field's values combine: `any` = OR (match any value), `all` = AND (match all values).",
                onclick: move |_| {
                    let next = combinator.toggled();
                    tracing::info!("[filter-strip] {f_comb} combinator -> {}", next.label());
                    filter::edit_filters(|q| q.active_filters.set_combinator_for(&f_comb, next));
                },
                "{comb_label}"
            }
            // Value chips — click removes (BadgeAction::Toggle in egui).
            for value in values {
                { chip(field.clone(), value) }
            }
        }
    }
}

fn chip(field: String, value: String) -> Element {
    let class = format!("fst-chip {} active", chip_class(&field));
    let label = value.clone();
    rsx! {
        button { key: "{label}", class: "{class}", title: "Remove this filter",
            onclick: move |_| {
                tracing::info!("[filter-strip] toggle chip {field}={value}");
                filter::edit_filters(|q| q.toggle_field_filter(&field, &value));
            },
            span { "{label}" }
            span { class: "fst-x", "×" }
        }
    }
}
