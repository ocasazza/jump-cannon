//! Vault-specific badge construction (doctype/tag/field chips) — Dioxus port
//! of crates/graph-renderer/src/ui/modal/badges.rs + modal/detect.rs +
//! frontmatter_chip.rs over the generic badge widget in panel-kit.
//!
//! Three layers, same as the egui side:
//!   * pure detectors (wikilink / url / iso-date / ticket-id / status colour)
//!   * field → [`BadgeKind`] mapping (filter_strip / inspector parity)
//!   * Element builders over [`crate::proto::NodeMeta`]: `node_badges`
//!     (tags + doctype + folder rows) and `frontmatter_chips` (one chip per
//!     detectable frontmatter scalar / array element)
//!
//! Expected adopters: the filter-strip, inspector, and query panels embed
//! these Elements and route the emitted [`BadgeAction`]s (toggle filter /
//! focus node / navigate / open url) through their own state channels —
//! this module owns only the vault→badge mapping.

#![allow(dead_code)] // adopters land with the panel ports

use dioxus::prelude::*;
use panel_kit::badge::{Badge, BadgeAction, BadgeClickKind, BadgeKind, BadgeSpec, Rgb};
use panel_kit::widgets::detect::status_color;
use panel_kit_core::widgets::detect::{
    host_from_url, is_iso_date, is_url, parse_ticket_id, parse_wikilink,
};
use serde_json::Value;

use crate::proto::NodeMeta;

// --- field → kind ----------------------------------------------------------------

/// Per-field badge kind — port of filter_strip.rs / inspector.rs
/// `badge_kind_for`.
pub(crate) fn badge_kind_for(field: &str) -> BadgeKind {
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

// --- NodeMeta → badges --------------------------------------------------------------

/// Frontmatter keys already promoted to typed `NodeMeta` fields (rendered
/// explicitly by `node_badges`) plus identity / display-only fields that
/// don't make sense as filter chips. Skipping them here prevents the strip
/// from double-emitting a chip the host already drew.
const SKIP_KEYS: &[&str] = &[
    "tags", "tag", "doctype", "folder", "aliases", "title", "id", "path",
];

fn is_skipped(key: &str) -> bool {
    SKIP_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key))
}

/// Tags + doctype + folder rows for a node — port of the modal's tag /
/// doctype+folder sections and the inspector's `show_badges`. Body-click
/// emits `Clicked` (the panel routes it to "focus this node"); the explicit
/// `+` emits `AddFilter`; `tint` is the community swatch override.
pub(crate) fn node_badges(
    meta: &NodeMeta,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    rsx! {
        for tag in meta.tags.iter() {
            {
                let spec = clickable_filter_spec("tags", tag, BadgeKind::Tag, is_active("tags", tag), tint);
                rsx! {
                    Badge { spec, on_action: move |a| on_action.call(a) }
                }
            }
        }
        if let Some(dt) = meta.doctype.as_ref() {
            {
                let spec = clickable_filter_spec("doctype", dt, BadgeKind::Doctype, is_active("doctype", dt), tint);
                rsx! {
                    Badge { spec, on_action: move |a| on_action.call(a) }
                }
            }
        }
        if !meta.folder.is_empty() {
            {
                let spec = clickable_filter_spec("folder", &meta.folder, BadgeKind::Folder, is_active("folder", &meta.folder), tint);
                rsx! {
                    Badge { spec, on_action: move |a| on_action.call(a) }
                }
            }
        }
    }
}

/// Badge spec for chips whose body click selects and plus click filters.
fn clickable_filter_spec(
    field: &str,
    value: &str,
    kind: BadgeKind,
    active: bool,
    tint: Option<Rgb>,
) -> BadgeSpec {
    BadgeSpec {
        active,
        with_plus: true,
        click_kind: BadgeClickKind::Clicked,
        override_color: tint,
        ..BadgeSpec::new(field, value, kind)
    }
}

/// One chip per detectable frontmatter scalar — port of
/// frontmatter_chip.rs::render_frontmatter_chips over `frontmatter_json`.
/// Detection rules (wikilinks, urls, status pills, dates, ticket ids,
/// plain values) match the egui modal, so both stacks show the same chips
/// for the same frontmatter. Returns an empty Element when the JSON is
/// absent or malformed.
pub(crate) fn frontmatter_chips(
    frontmatter_json: &str,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, Value>>(frontmatter_json) else {
        return rsx! {};
    };
    rsx! {
        for (key, value) in map.iter().filter(|(k, _)| !is_skipped(k)) {
            {value_chips(key, value, is_active, tint, on_action)}
        }
    }
}

/// Schema-derived facts shared by the Nodes panel and Inspector. The server
/// preserves declaration order and sends values as JSON so every importer uses
/// this one presentation path.
pub(crate) fn node_facts(
    meta: &NodeMeta,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    if meta.facts.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "nodes-facts node-detail-section",
            h3 { "Facts" }
            dl { class: "node-facts-grid",
                for fact in &meta.facts {
                    div { class: "node-fact-row", key: "{fact.key}",
                        dt { "{fact.key}" }
                        dd { class: if fact.field_type == "number" { "node-fact-value number" } else { "node-fact-value" },
                            { fact_value(fact, is_active, tint, on_action) }
                        }
                    }
                }
            }
        }
    }
}

fn fact_value(
    fact: &crate::proto::NodeFact,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    let Ok(value) = serde_json::from_str::<Value>(&fact.value_json) else {
        return rsx! { span { "{fact.value_json}" } };
    };
    match fact.field_type.as_str() {
        "keyword_list" => rsx! {
            div { class: "node-fact-chips",
                { value_chips(&fact.key, &value, is_active, tint, on_action) }
            }
        },
        "number" => rsx! { span { "{value}" } },
        "text" => {
            let text = value.as_str().unwrap_or(&fact.value_json);
            rsx! { div { class: "node-fact-text", "{text}" } }
        }
        _ => {
            let display = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            rsx! { span { "{display}" } }
        }
    }
}

/// Phase-0 provenance shared by both node detail surfaces. Later phases fill
/// the optional transform/run fields without changing this component.
pub(crate) fn node_provenance(meta: &NodeMeta) -> Element {
    let Some(provenance) = meta.provenance.as_ref() else {
        return rsx! {};
    };
    rsx! {
        section { class: "nodes-provenance node-detail-section",
            h3 { "Provenance" }
            dl { class: "node-provenance-grid",
                if !provenance.source_id.is_empty() {
                    div { class: "node-fact-row",
                        dt { "source" }
                        dd { class: "node-fact-value", "{provenance.source_id}" }
                    }
                }
                if let Some(transform_id) = provenance.transform_id.as_ref() {
                    div { class: "node-fact-row",
                        dt { "transform" }
                        dd { class: "node-fact-value", "{transform_id}" }
                    }
                }
                if let Some(run) = provenance.first_seen_run {
                    div { class: "node-fact-row",
                        dt { "first seen" }
                        dd { class: "node-fact-value number", "#{run}" }
                    }
                }
                if let Some(run) = provenance.last_changed_run {
                    div { class: "node-fact-row",
                        dt { "last changed" }
                        dd { class: "node-fact-value number", "#{run}" }
                    }
                }
                div { class: "node-fact-row",
                    dt { "change" }
                    dd { class: "node-fact-value", "{provenance.change}" }
                }
                if provenance.history_bounded {
                    div { class: "node-provenance-bound", "history bounded" }
                }
            }
        }
    }
}

/// Port of frontmatter_chip.rs::render_value — strings get the detector
/// cascade, arrays recurse one level over scalars, numbers/bools become
/// toggle chips, nulls/objects are skipped.
fn value_chips(
    field: &str,
    value: &Value,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    match value {
        Value::String(s) => string_chip(field, s, is_active, tint, on_action),
        Value::Array(arr) => rsx! {
            for v in arr.iter().filter(|v| !matches!(v, Value::Array(_) | Value::Object(_) | Value::Null)) {
                {value_chips(field, v, is_active, tint, on_action)}
            }
        },
        Value::Number(n) => scalar_chip(field, &n.to_string(), is_active, tint, on_action),
        Value::Bool(b) => scalar_chip(
            field,
            if *b { "true" } else { "false" },
            is_active,
            tint,
            on_action,
        ),
        Value::Null | Value::Object(_) => rsx! {},
    }
}

/// Number / bool chip: plain toggle badge with active halo + tint.
fn scalar_chip(
    field: &str,
    value: &str,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    let spec = BadgeSpec {
        active: is_active(field, value),
        override_color: tint,
        ..BadgeSpec::new(field, value, BadgeKind::Generic)
    };

    rsx! {
        Badge { spec, on_action: move |a| on_action.call(a) }
    }
}

/// Detector cascade for one string value — port of
/// frontmatter_chip.rs::render_string. Order matters: wikilink, url,
/// status, date, ticket, then plain (long text is dropped).
fn string_chip(
    field: &str,
    s: &str,
    is_active: &dyn Fn(&str, &str) -> bool,
    tint: Option<Rgb>,
    on_action: EventHandler<BadgeAction>,
) -> Element {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return rsx! {};
    }

    if let Some((page, alias)) = parse_wikilink(trimmed) {
        let label = alias.unwrap_or_else(|| page.clone());
        let spec = BadgeSpec {
            override_color: tint,
            ..BadgeSpec::new(field, label, BadgeKind::Wikilink { resolved: true, target: page })
        };
        return rsx! {
            Badge { spec, on_action: move |a| on_action.call(a) }
        };
    }

    if is_url(trimmed) {
        let host = host_from_url(trimmed).unwrap_or_default();
        let spec = BadgeSpec {
            override_color: tint,
            ..BadgeSpec::new(field, trimmed, BadgeKind::Url { href: trimmed.to_string(), host })
        };
        return rsx! {
            Badge { spec, on_action: move |a| on_action.call(a) }
        };
    }

    // Status pill: dark fill, semantic stroke + text colour, click toggles
    // the (field, value) filter — modal/badges.rs::status_pill.
    if let Some(color) = status_color(trimmed) {
        let spec = BadgeSpec {
            small: true,
            ..BadgeSpec::new(field, trimmed, BadgeKind::Status)
        };
        return rsx! {
            Badge { spec, accent_color: Some(color.to_string()), on_action: move |a| on_action.call(a) }
        };
    }

    // Date chip: yellow stroke, click toggles — modal/badges.rs::date_badge.
    if is_iso_date(trimmed) {
        let spec = BadgeSpec {
            small: true,
            ..BadgeSpec::new(field, trimmed, BadgeKind::Date)
        };
        return rsx! {
            Badge { spec, on_action: move |a| on_action.call(a) }
        };
    }

    // Ticket chip: yellow stroke + text, click navigates to the bare
    // ticket id — modal/badges.rs::ticket_badge + the chip strip's
    // `navigate_to = ticket` routing, folded into the action stream as
    // `Navigate` so adopters keep one match.
    if let Some(ticket) = parse_ticket_id(trimmed) {
        let spec = BadgeSpec {
            small: true,
            click_kind: BadgeClickKind::Clicked,
            ..BadgeSpec::new(field, trimmed, BadgeKind::Generic)
        };
        return rsx! {
            Badge {
                spec,
                accent_color: Some("var(--color-warning)".to_string()),
                on_action: move |a| match a {
                    BadgeAction::Clicked { .. } => {
                        on_action.call(BadgeAction::Navigate { target: ticket.clone() })
                    }
                    other => on_action.call(other),
                },
            }
        };
    }

    // Long prose isn't a chip.
    if trimmed.chars().count() > 120 {
        return rsx! {};
    }

    // Plain value: the egui side renders the roomy halo Badge when the
    // filter is active and the cramped one-shot button otherwise — same
    // split here via the `small` flag.
    let active = is_active(field, trimmed);
    let spec = BadgeSpec {
        active,
        small: !active,
        override_color: if active { tint } else { None },
        ..BadgeSpec::new(field, trimmed, BadgeKind::Generic)
    };
    rsx! {
        Badge { spec, on_action: move |a| on_action.call(a) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_kinds() {
        assert!(matches!(badge_kind_for("tags"), BadgeKind::Tag));
        assert!(matches!(badge_kind_for("doctype"), BadgeKind::Doctype));
        assert!(matches!(badge_kind_for("anything"), BadgeKind::Generic));
    }
}
