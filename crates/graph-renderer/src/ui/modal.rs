//! Phase E — metadata modal.
//!
//! Shows an `egui::Window` for the focused node:
//!   * tag list at the top (badge per tag)
//!   * frontmatter rows with type-detected badges (wikilinks, urls, dates,
//!     ticket-ids, status pills, plain values)
//!   * graph metrics row (degree, indegree, outdegree, pagerank, …)
//!
//! Clicking a badge produces a `ModalAction` that the App acts on:
//!   * `navigate_to(id)` triggers a refetch of `/node/:id` and re-opens the
//!     modal pointed at the new node — the wikilink / ticket-id flow.
//!   * `toggle_filter((field, value))` is a hook for Phase F's query model;
//!     for now the App just logs it.

use eframe::egui;
use serde_json::Value;

use crate::proto;
use crate::ui::theme::accent;

/// Modal state stored on `App` (not persisted — open-state is ephemeral).
#[derive(Default)]
pub struct ModalState {
    pub open: bool,
    /// Pinned modals stay open across hover changes (future hover-preview hook).
    pub pinned: bool,
    pub current: Option<proto::NodeMeta>,
    pub fetch_error: Option<String>,
}

/// What happened in the modal during this frame. Empty when no badge clicked.
pub struct ModalAction {
    pub navigate_to: Option<String>,
    pub toggle_filter: Option<(String, String)>,
}

impl ModalAction {
    fn empty() -> Self {
        Self {
            navigate_to: None,
            toggle_filter: None,
        }
    }
}

/// Draw the modal. No-op if `!state.open` or no current node.
pub fn show_modal(ctx: &egui::Context, state: &mut ModalState) -> ModalAction {
    let mut action = ModalAction::empty();

    if let Some(err) = state.fetch_error.clone() {
        let mut open = state.open;
        egui::Window::new("node fetch failed")
            .open(&mut open)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.colored_label(accent::RED, err);
            });
        state.open = open;
        if !state.open {
            state.fetch_error = None;
            state.current = None;
        }
        return action;
    }

    if !state.open || state.current.is_none() {
        return action;
    }
    let meta = state.current.as_ref().unwrap().clone();

    let mut open = state.open;
    egui::Window::new(if meta.title.is_empty() {
        meta.id.clone()
    } else {
        meta.title.clone()
    })
    .open(&mut open)
    .resizable(true)
    .default_width(380.0)
    .show(ctx, |ui| {
        // path subtitle
        ui.label(
            egui::RichText::new(&meta.path)
                .monospace()
                .small()
                .weak(),
        );
        if let Some(dt) = meta.doctype.as_ref() {
            ui.label(egui::RichText::new(format!("doctype: {dt}")).small().weak());
        }
        ui.separator();

        // tag list (top)
        if !meta.tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for tag in &meta.tags {
                    if tag_button(ui, tag).clicked() {
                        action.toggle_filter = Some(("tag".into(), tag.clone()));
                    }
                }
            });
            ui.separator();
        }

        // frontmatter rows
        if !meta.frontmatter_json.is_empty() {
            if let Ok(map) = serde_json::from_str::<serde_json::Map<String, Value>>(
                &meta.frontmatter_json,
            ) {
                if !map.is_empty() {
                    egui::Grid::new("modal-meta")
                        .num_columns(2)
                        .striped(false)
                        .show(ui, |ui| {
                            for (key, value) in &map {
                                label_cell(ui, key);
                                value_cell(ui, key, value, &mut action);
                                ui.end_row();
                            }
                        });
                    ui.separator();
                }
            }
        }

        // graph metrics
        egui::Grid::new("modal-metrics")
            .num_columns(2)
            .striped(false)
            .show(ui, |ui| {
                metric_row(ui, "degree", meta.degree as i64);
                metric_row(ui, "indegree", meta.indegree as i64);
                metric_row(ui, "outdegree", meta.outdegree as i64);
                metric_row_f(ui, "pagerank", meta.pagerank);
                metric_row_f(ui, "betweenness", meta.betweenness);
                metric_row(ui, "kcore", meta.kcore as i64);
                metric_row(ui, "community", meta.community as i64);
                metric_row(ui, "wcc", meta.wcc as i64);
            });
    });
    state.open = open;
    if !state.open {
        state.current = None;
    }
    action
}

// ---------- cell helpers ----------

fn label_cell(ui: &mut egui::Ui, key: &str) {
    ui.label(
        egui::RichText::new(key)
            .small()
            .weak()
            .monospace(),
    );
}

fn value_cell(
    ui: &mut egui::Ui,
    field: &str,
    value: &Value,
    action: &mut ModalAction,
) {
    match value {
        Value::String(s) => {
            render_string_value(ui, field, s, action);
        }
        Value::Array(arr) => {
            ui.horizontal_wrapped(|ui| {
                for v in arr {
                    render_one(ui, field, v, action);
                }
            });
        }
        Value::Number(n) => {
            ui.label(egui::RichText::new(n.to_string()).monospace().small());
        }
        Value::Bool(b) => {
            ui.label(if *b { "true" } else { "false" });
        }
        Value::Null => {
            ui.label(egui::RichText::new("—").weak());
        }
        Value::Object(_) => {
            ui.label(egui::RichText::new("(object)").weak().small());
        }
    }
}

/// Render a single value inside an array (avoids the wrapping horizontal layout
/// that `value_cell` does for arrays — we're already in one).
fn render_one(ui: &mut egui::Ui, field: &str, value: &Value, action: &mut ModalAction) {
    match value {
        Value::String(s) => render_string_value(ui, field, s, action),
        Value::Number(n) => {
            ui.label(egui::RichText::new(n.to_string()).monospace().small());
        }
        Value::Bool(b) => {
            ui.label(if *b { "true" } else { "false" });
        }
        Value::Null => {
            ui.label(egui::RichText::new("—").weak());
        }
        Value::Array(_) | Value::Object(_) => {
            ui.label(egui::RichText::new("(nested)").weak().small());
        }
    }
}

fn render_string_value(
    ui: &mut egui::Ui,
    field: &str,
    s: &str,
    action: &mut ModalAction,
) {
    let trimmed = s.trim();

    // wikilink: [[Page]] or [[Page|alias]]
    if let Some(target) = parse_wikilink(trimmed) {
        let (page, alias) = target;
        let label = alias.unwrap_or_else(|| page.clone());
        if wikilink_button(ui, &label).clicked() {
            action.navigate_to = Some(page);
        }
        return;
    }

    // url
    if is_url(trimmed) {
        if url_button(ui, trimmed).clicked() {
            // Open in new tab on wasm. On native this is a no-op badge — clicking
            // still doesn't open a browser, but we emit a filter intent so
            // callers can hook it.
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(window) = web_sys::window() {
                    let _ = window.open_with_url_and_target(trimmed, "_blank");
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = trimmed; // silence unused on native
            }
        }
        return;
    }

    // status pill (matches before date so words like "active" / "done" win)
    if let Some(color) = status_color(trimmed) {
        if status_pill(ui, trimmed, color).clicked() {
            action.toggle_filter = Some((field.to_string(), trimmed.to_string()));
        }
        return;
    }

    // YYYY-MM-DD date
    if is_iso_date(trimmed) {
        if date_badge(ui, trimmed).clicked() {
            action.toggle_filter = Some((field.to_string(), trimmed.to_string()));
        }
        return;
    }

    // ticket id (e.g. ITHELP-1234, JIRA-42, FOO-1: title)
    if let Some(ticket) = parse_ticket_id(trimmed) {
        if ticket_badge(ui, trimmed).clicked() {
            action.navigate_to = Some(ticket);
        }
        return;
    }

    // long text
    if trimmed.chars().count() > 120 {
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.label(egui::RichText::new(trimmed).small());
            });
        return;
    }

    // default badge — clicks toggle filter on (field, value)
    if plain_badge(ui, trimmed).clicked() {
        action.toggle_filter = Some((field.to_string(), trimmed.to_string()));
    }
}

// ---------- type detection ----------

fn parse_wikilink(s: &str) -> Option<(String, Option<String>)> {
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

fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://"))
        && !s.contains(char::is_whitespace)
        && s.len() < 2048
}

fn is_iso_date(s: &str) -> bool {
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

/// Parse a JIRA-style ticket id at the start of the string. Accepts:
///   FOO-123
///   FOO-123: subject
///   FOO-123 — subject
/// Returns the bare id (e.g. "FOO-123") or None.
fn parse_ticket_id(s: &str) -> Option<String> {
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

fn status_color(s: &str) -> Option<egui::Color32> {
    match s.to_ascii_lowercase().as_str() {
        "active" | "done" | "ok" | "ready" | "passed" => Some(accent::GREEN),
        "failed" | "blocked" | "broken" | "error" => Some(accent::RED),
        "needs-review" | "needs-fetch" | "in-progress" | "wip" => Some(accent::YELLOW),
        "draft" | "pending" => Some(accent::BLUE),
        "archived" | "deprecated" | "stale" => Some(egui::Color32::WHITE),
        _ => None,
    }
}

// ---------- badge widgets ----------

fn tag_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

fn plain_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

fn wikilink_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    // Wikilink chip: filled darker than tag badges to read distinctly.
    let txt = egui::RichText::new(format!("⟶ {label}"))
        .monospace()
        .small()
        .color(accent::BLUE);
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, accent::BLUE))
            .fill(egui::Color32::from_rgb(0x10, 0x10, 0x18))
            .small(),
    )
}

fn url_button(ui: &mut egui::Ui, url: &str) -> egui::Response {
    let display = if url.len() > 48 {
        format!("{}…", &url[..47])
    } else {
        url.to_string()
    };
    let txt = egui::RichText::new(display).monospace().small().underline();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
            .fill(egui::Color32::BLACK)
            .small(),
    )
    .on_hover_text(url)
}

fn date_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small();
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, accent::YELLOW))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

fn ticket_badge(ui: &mut egui::Ui, label: &str) -> egui::Response {
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

fn status_pill(ui: &mut egui::Ui, label: &str, color: egui::Color32) -> egui::Response {
    let txt = egui::RichText::new(label).monospace().small().color(color);
    ui.add(
        egui::Button::new(txt)
            .stroke(egui::Stroke::new(1.0, color))
            .fill(egui::Color32::BLACK)
            .small(),
    )
}

// ---------- metric rows ----------

fn metric_row(ui: &mut egui::Ui, label: &str, value: i64) {
    label_cell(ui, label);
    ui.label(egui::RichText::new(value.to_string()).monospace().small());
    ui.end_row();
}

fn metric_row_f(ui: &mut egui::Ui, label: &str, value: f32) {
    label_cell(ui, label);
    ui.label(
        egui::RichText::new(format!("{value:.4}"))
            .monospace()
            .small(),
    );
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilink_basic() {
        assert_eq!(
            parse_wikilink("[[Alpha]]"),
            Some(("Alpha".into(), None))
        );
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
