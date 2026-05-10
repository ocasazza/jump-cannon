//! Shared frontmatter-chip rendering used by both the metadata modal and
//! the right-hand inspector sidebar.
//!
//! Walks a parsed `serde_json::Map<String, Value>` and emits one chip per
//! detectable scalar (or array element). Detection rules — wikilinks,
//! URLs, status pills, ISO dates, ticket ids, plain values — match what
//! the modal renders, so both surfaces show the same set of chips for
//! the same frontmatter input.
//!
//! The caller drains a [`ChipOutcome`] per click and routes it into the
//! appropriate app-level channel (filter toggle / node navigate / open
//! url).

use eframe::egui;
use serde_json::Value;

use crate::ui::badge::{Badge, BadgeAction, BadgeKind};

use super::modal::badges::{date_badge, plain_badge, status_color, status_pill, ticket_badge};
use super::modal::detect::{host_from_url, is_iso_date, is_url, parse_ticket_id, parse_wikilink};

/// What the chip strip wants the host to do this frame.
#[derive(Default)]
pub struct ChipOutcome {
    pub toggle_filter: Option<(String, String)>,
    pub navigate_to: Option<String>,
    pub open_url: Option<String>,
}

/// Render every frontmatter (key, value) as a chip.
///
/// `filters_for_active` is consulted so chips that match an already-active
/// `(field, value)` filter render with the active halo. Pass an empty
/// reference if you don't have one yet.
///
/// `community_tint` is the optional swatch (community / centrality / etc)
/// that the inspector and modal both apply so the chip strip reads as
/// part of the focused node's colour cohort.
/// Frontmatter keys already promoted to typed `NodeMeta` fields (and so
/// rendered explicitly by the inspector / modal above the frontmatter
/// strip), plus a small set that don't make sense as filter chips
/// (identity / display-only fields). Skipping these here prevents the
/// strip from double-emitting a chip the host already drew.
const SKIP_KEYS: &[&str] = &[
    "tags", "tag", "doctype", "folder", "aliases", "title", "id", "path",
];

fn is_skipped(key: &str) -> bool {
    SKIP_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key))
}

pub fn render_frontmatter_chips(
    ui: &mut egui::Ui,
    map: &serde_json::Map<String, Value>,
    filters: &crate::ui::query::ActiveFieldFilters,
    community_tint: Option<egui::Color32>,
) -> ChipOutcome {
    let mut out = ChipOutcome::default();
    for (key, value) in map {
        if is_skipped(key) {
            continue;
        }
        render_value(ui, key, value, filters, community_tint, &mut out);
    }
    out
}

fn render_value(
    ui: &mut egui::Ui,
    field: &str,
    value: &Value,
    filters: &crate::ui::query::ActiveFieldFilters,
    tint: Option<egui::Color32>,
    out: &mut ChipOutcome,
) {
    match value {
        Value::String(s) => render_string(ui, field, s, filters, tint, out),
        Value::Array(arr) => {
            for v in arr {
                match v {
                    Value::Array(_) | Value::Object(_) | Value::Null => continue,
                    _ => render_value(ui, field, v, filters, tint, out),
                }
            }
        }
        Value::Number(n) => {
            let v = n.to_string();
            let active = is_active(filters, field, &v);
            let b = with_tint(
                Badge::new(field, &v, BadgeKind::Generic).active(active),
                tint,
            );
            if let BadgeAction::Toggle { field, value } = b.show(ui) {
                out.toggle_filter = Some((field, value));
            }
        }
        Value::Bool(b) => {
            let v = if *b { "true" } else { "false" };
            let active = is_active(filters, field, v);
            let badge = with_tint(
                Badge::new(field, v, BadgeKind::Generic).active(active),
                tint,
            );
            if let BadgeAction::Toggle { field, value } = badge.show(ui) {
                out.toggle_filter = Some((field, value));
            }
        }
        Value::Null | Value::Object(_) => {}
    }
}

fn render_string(
    ui: &mut egui::Ui,
    field: &str,
    s: &str,
    filters: &crate::ui::query::ActiveFieldFilters,
    tint: Option<egui::Color32>,
    out: &mut ChipOutcome,
) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return;
    }

    if let Some((page, alias)) = parse_wikilink(trimmed) {
        let label = alias.unwrap_or_else(|| page.clone());
        let b = Badge::new(
            field,
            &label,
            BadgeKind::Wikilink {
                resolved: true,
                target: page.clone(),
            },
        );
        if let BadgeAction::Navigate { target } = with_tint(b, tint).show(ui) {
            out.navigate_to = Some(target);
        }
        return;
    }

    if is_url(trimmed) {
        let host = host_from_url(trimmed).unwrap_or_default();
        let b = Badge::new(
            field,
            trimmed,
            BadgeKind::Url {
                href: trimmed.to_string(),
                host,
            },
        );
        if let BadgeAction::OpenUrl { href } = with_tint(b, tint).show(ui) {
            out.open_url = Some(href);
        }
        return;
    }

    if let Some(color) = status_color(trimmed) {
        if status_pill(ui, trimmed, color).clicked() {
            out.toggle_filter = Some((field.to_string(), trimmed.to_string()));
        }
        return;
    }

    if is_iso_date(trimmed) {
        if date_badge(ui, trimmed).clicked() {
            out.toggle_filter = Some((field.to_string(), trimmed.to_string()));
        }
        return;
    }

    if let Some(ticket) = parse_ticket_id(trimmed) {
        if ticket_badge(ui, trimmed).clicked() {
            out.navigate_to = Some(ticket);
        }
        return;
    }

    if trimmed.chars().count() > 120 {
        return;
    }

    let active = is_active(filters, field, trimmed);
    if active {
        let b = with_tint(
            Badge::new(field, trimmed, BadgeKind::Generic).active(true),
            tint,
        );
        if let BadgeAction::Toggle { field, value } = b.show(ui) {
            out.toggle_filter = Some((field, value));
        }
    } else if plain_badge(ui, trimmed).clicked() {
        out.toggle_filter = Some((field.to_string(), trimmed.to_string()));
    }
}

fn is_active(filters: &crate::ui::query::ActiveFieldFilters, field: &str, value: &str) -> bool {
    filters
        .by_field
        .get(field)
        .map(|s| s.contains(value))
        .unwrap_or(false)
}

fn with_tint<'a>(b: Badge<'a>, tint: Option<egui::Color32>) -> Badge<'a> {
    match tint {
        Some(c) => b.override_color(c),
        None => b,
    }
}
