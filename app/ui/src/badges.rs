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
use panel_kit::badge::{Badge, BadgeAction, BadgeClickKind, BadgeKind, Rgb};
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

/// Status string → CSS colour — port of modal/badges.rs::status_color onto
/// the panel-kit theme variables (egui accent::BLUE has no theme
/// counterpart, so it keeps its hex).
pub(crate) fn status_color(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "active" | "done" | "ok" | "ready" | "passed" => Some("var(--green)"),
        "failed" | "blocked" | "broken" | "error" => Some("var(--red)"),
        "needs-review" | "needs-fetch" | "in-progress" | "wip" => Some("var(--yellow)"),
        "draft" | "pending" => Some("#3b9bff"),
        "archived" | "deprecated" | "stale" => Some("var(--fg)"),
        _ => None,
    }
}

// --- pure-string detectors (modal/detect.rs port) ----------------------------------

pub(crate) fn parse_wikilink(s: &str) -> Option<(String, Option<String>)> {
    let s = s.trim();
    if !(s.starts_with("[[") && s.ends_with("]]") && s.len() >= 5) {
        return None;
    }
    let inner = &s[2..s.len() - 2];
    if inner.is_empty() {
        return None;
    }
    if let Some((page, alias)) = inner.split_once('|') {
        Some((page.trim().to_string(), Some(alias.trim().to_string())))
    } else {
        Some((inner.trim().to_string(), None))
    }
}

pub(crate) fn host_from_url(s: &str) -> Option<String> {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let host = rest
        .split(|c: char| c == '/' || c == ':' || c == '?')
        .next()?;
    Some(host.to_string())
}

pub(crate) fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://"))
        && !s.contains(char::is_whitespace)
        && s.len() < 2048
}

pub(crate) fn is_iso_date(s: &str) -> bool {
    // YYYY-MM-DD with optional time tail. Strict on the date head.
    if s.len() < 10 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
        && (s.len() == 10 || matches!(bytes[10], b'T' | b' '))
}

/// Parse a JIRA-style ticket id at the start of the string. Accepts
/// `FOO-123`, `FOO-123: subject`, `FOO-123 — subject`. Returns the bare id.
pub(crate) fn parse_ticket_id(s: &str) -> Option<String> {
    let head: &str = s.split(|c: char| c == ':' || c == ' ').next()?;
    if head.len() < 3 || head.len() > 24 {
        return None;
    }
    let (prefix, rest) = head.split_once('-')?;
    if prefix.is_empty() || prefix.len() > 16 {
        return None;
    }
    let mut chars = prefix.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return None;
    }
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(head.to_string())
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
            Badge {
                field: "tags",
                value: "{tag}",
                kind: BadgeKind::Tag,
                active: is_active("tags", tag),
                with_plus: true,
                click_kind: BadgeClickKind::Clicked,
                override_color: tint,
                on_action: move |a| on_action.call(a),
            }
        }
        if let Some(dt) = meta.doctype.as_ref() {
            Badge {
                field: "doctype",
                value: "{dt}",
                kind: BadgeKind::Doctype,
                active: is_active("doctype", dt),
                with_plus: true,
                click_kind: BadgeClickKind::Clicked,
                override_color: tint,
                on_action: move |a| on_action.call(a),
            }
        }
        if !meta.folder.is_empty() {
            Badge {
                field: "folder",
                value: "{meta.folder}",
                kind: BadgeKind::Folder,
                active: is_active("folder", &meta.folder),
                with_plus: true,
                click_kind: BadgeClickKind::Clicked,
                override_color: tint,
                on_action: move |a| on_action.call(a),
            }
        }
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
    rsx! {
        Badge {
            field: "{field}",
            value: "{value}",
            kind: BadgeKind::Generic,
            active: is_active(field, value),
            override_color: tint,
            on_action: move |a| on_action.call(a),
        }
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
        return rsx! {
            Badge {
                field: "{field}",
                value: "{label}",
                kind: BadgeKind::Wikilink { resolved: true, target: page },
                override_color: tint,
                on_action: move |a| on_action.call(a),
            }
        };
    }

    if is_url(trimmed) {
        let host = host_from_url(trimmed).unwrap_or_default();
        return rsx! {
            Badge {
                field: "{field}",
                value: "{trimmed}",
                kind: BadgeKind::Url { href: trimmed.to_string(), host },
                override_color: tint,
                on_action: move |a| on_action.call(a),
            }
        };
    }

    // Status pill: dark fill, semantic stroke + text colour, click toggles
    // the (field, value) filter — modal/badges.rs::status_pill.
    if let Some(color) = status_color(trimmed) {
        return rsx! {
            Badge {
                field: "{field}",
                value: "{trimmed}",
                kind: BadgeKind::Status,
                accent_color: color,
                small: true,
                on_action: move |a| on_action.call(a),
            }
        };
    }

    // Date chip: yellow stroke, click toggles — modal/badges.rs::date_badge.
    if is_iso_date(trimmed) {
        return rsx! {
            Badge {
                field: "{field}",
                value: "{trimmed}",
                kind: BadgeKind::Date,
                small: true,
                on_action: move |a| on_action.call(a),
            }
        };
    }

    // Ticket chip: yellow stroke + text, click navigates to the bare
    // ticket id — modal/badges.rs::ticket_badge + the chip strip's
    // `navigate_to = ticket` routing, folded into the action stream as
    // `Navigate` so adopters keep one match.
    if let Some(ticket) = parse_ticket_id(trimmed) {
        return rsx! {
            Badge {
                field: "{field}",
                value: "{trimmed}",
                kind: BadgeKind::Generic,
                accent_color: "var(--yellow)",
                small: true,
                click_kind: BadgeClickKind::Clicked,
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
    rsx! {
        Badge {
            field: "{field}",
            value: "{trimmed}",
            kind: BadgeKind::Generic,
            active,
            small: !active,
            override_color: if active { tint } else { None },
            on_action: move |a| on_action.call(a),
        }
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

    #[test]
    fn wikilink_basic() {
        assert_eq!(parse_wikilink("[[Alpha]]"), Some(("Alpha".into(), None)));
        assert_eq!(
            parse_wikilink("[[Alpha|alias]]"),
            Some(("Alpha".into(), Some("alias".into())))
        );
        assert_eq!(parse_wikilink("Alpha"), None);
        assert_eq!(parse_wikilink("[[]]"), None);
    }

    #[test]
    fn url_detect() {
        assert!(is_url("https://example.com"));
        assert!(is_url("http://luna.local:3000/d/x"));
        assert!(!is_url("example.com"));
        assert!(!is_url("not a url"));
    }

    #[test]
    fn iso_date_detect() {
        assert!(is_iso_date("2026-04-30"));
        assert!(is_iso_date("2026-04-30T12:00:00Z"));
        assert!(!is_iso_date("April 30"));
        assert!(!is_iso_date("2026-4-1"));
    }

    #[test]
    fn ticket_id_detect() {
        assert_eq!(parse_ticket_id("ITHELP-1234"), Some("ITHELP-1234".into()));
        assert_eq!(parse_ticket_id("JIRA-42: subject"), Some("JIRA-42".into()));
        assert_eq!(parse_ticket_id("FOO-7 — body"), Some("FOO-7".into()));
        assert_eq!(parse_ticket_id("not-a-ticket"), None);
        assert_eq!(parse_ticket_id("foo-1"), None);
    }

    #[test]
    fn field_kinds() {
        assert!(matches!(badge_kind_for("tags"), BadgeKind::Tag));
        assert!(matches!(badge_kind_for("doctype"), BadgeKind::Doctype));
        assert!(matches!(badge_kind_for("anything"), BadgeKind::Generic));
    }
}
