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

pub(crate) mod badges;
pub(crate) mod detect;

use eframe::egui;
use serde_json::Value;

use crate::proto;
use crate::ui::badge::{Badge, BadgeAction, BadgeClickKind, BadgeKind};
use crate::ui::theme::accent;

use badges::{date_badge, plain_badge, status_color, status_pill, ticket_badge};
use detect::{host_from_url, is_iso_date, is_url, parse_ticket_id, parse_wikilink};

/// Modal state stored on `App` (not persisted — open-state is ephemeral).
#[derive(Default)]
pub struct ModalState {
    pub open: bool,
    /// Pinned modals stay open across hover changes (future hover-preview hook).
    pub pinned: bool,
    pub current: Option<proto::NodeMeta>,
    pub fetch_error: Option<String>,
    /// Per-modal egui_commonmark layout cache. Holding it here (vs.
    /// re-allocating per frame) lets the markdown viewer reuse parsed
    /// AST + measured galleys between frames — critical for non-trivial
    /// markdown bodies, where re-parsing every frame would spike the
    /// egui thread.
    pub markdown_cache: egui_commonmark::CommonMarkCache,
}

/// What happened in the modal during this frame. Empty when no badge clicked.
pub struct ModalAction {
    pub navigate_to: Option<String>,
    pub toggle_filter: Option<(String, String)>,
    pub open_url: Option<String>,
    /// A badge body was clicked (or a wikilink resolved). The App should
    /// camera-center on this node id + sticky-focus it. Set by either
    /// non-link badge body-clicks (target = the currently-displayed
    /// modal node) or wikilink/ticket badges (target = the link target).
    pub focus_node: Option<String>,
}

impl ModalAction {
    fn empty() -> Self {
        Self {
            navigate_to: None,
            toggle_filter: None,
            open_url: None,
            focus_node: None,
        }
    }
}

/// Route a single badge's outcome into the right [`ModalAction`] slot.
/// `body_target` is the node id that a body-click should focus when the
/// badge isn't a Wikilink/Url (eg. clicking a tag chip in the modal sets
/// the camera onto the node the modal is currently showing).
fn dispatch_badge(action: BadgeAction, body_target: &str, out: &mut ModalAction) {
    match action {
        BadgeAction::Toggle { field, value } => {
            out.toggle_filter = Some((field, value));
        }
        BadgeAction::AddFilter { field, value } => {
            out.toggle_filter = Some((field, value));
        }
        BadgeAction::Clicked { .. } => {
            out.focus_node = Some(body_target.to_string());
        }
        BadgeAction::Navigate { target } => {
            // The App folds focus + sidebar update into one helper, so we
            // ship Navigate through the same focus_node channel.
            out.focus_node = Some(target.clone());
            out.navigate_to = Some(target);
        }
        BadgeAction::OpenUrl { href } => {
            out.open_url = Some(href);
        }
        BadgeAction::Hovered { .. } | BadgeAction::None => {}
    }
}

/// Draw the modal. No-op if `!state.open` or no current node.
pub fn show_modal(ctx: &egui::Context, state: &mut ModalState) -> ModalAction {
    show_modal_with(ctx, state, &crate::ui::query::ActiveFieldFilters::default(), None)
}

/// Variant that knows about the active filter set so badges paint with
/// their selected/halo state. `node_tint` is the canvas-rendered colour
/// for the focused node under the active `StyleState::color_by`; when
/// `Some`, every metadata badge inherits it so the modal reads as part
/// of the same colour cohort as the node it describes.
pub fn show_modal_with(
    ctx: &egui::Context,
    state: &mut ModalState,
    filters: &crate::ui::query::ActiveFieldFilters,
    node_tint: Option<egui::Color32>,
) -> ModalAction {
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
    .default_width(560.0)
    .min_width(360.0)
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

        // Active-color-by tinted badges: every chip for this node
        // wears the same swatch the canvas paints the node with under
        // the user's current `StyleState::color_by`. Falls back to the
        // per-kind palette when the host couldn't resolve a colour.
        let community_tint = node_tint;
        // Free fn (not closure) to avoid the `'_`-lifetime mismatch
        // when the closure tries to return Badge<'a> across two
        // anonymous lifetimes — same fix as inspector::maybe_tint.
        fn tint<'a>(b: Badge<'a>, c: Option<egui::Color32>) -> Badge<'a> {
            match c {
                Some(c) => b.override_color(c),
                None => b,
            }
        }
        // Body-click on any of these badges focuses the node the modal
        // is currently showing (camera + sidebar sync). The explicit `+`
        // affordance routes to the filter set without overloading body
        // semantics. See `dispatch_badge`.
        let body_target = meta.id.clone();
        // tag list (top)
        if !meta.tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for tag in &meta.tags {
                    let active = filters
                        .by_field
                        .get("tags")
                        .map(|s| s.contains(tag))
                        .unwrap_or(false);
                    let b = tint(Badge::new("tags", tag, BadgeKind::Tag).active(active), community_tint)
                        .with_plus(true)
                        .click_kind(BadgeClickKind::Clicked);
                    dispatch_badge(b.show(ui), &body_target, &mut action);
                }
            });
            ui.separator();
        }
        // doctype + folder badges row.
        ui.horizontal_wrapped(|ui| {
            if let Some(dt) = meta.doctype.as_ref() {
                let active = filters
                    .by_field
                    .get("doctype")
                    .map(|s| s.contains(dt))
                    .unwrap_or(false);
                let b = tint(Badge::new("doctype", dt, BadgeKind::Doctype).active(active), community_tint)
                    .with_plus(true)
                    .click_kind(BadgeClickKind::Clicked);
                dispatch_badge(b.show(ui), &body_target, &mut action);
            }
            if !meta.folder.is_empty() {
                let active = filters
                    .by_field
                    .get("folder")
                    .map(|s| s.contains(&meta.folder))
                    .unwrap_or(false);
                let b = tint(
                    Badge::new("folder", &meta.folder, BadgeKind::Folder).active(active),
                    community_tint,
                )
                .with_plus(true)
                .click_kind(BadgeClickKind::Clicked);
                dispatch_badge(b.show(ui), &body_target, &mut action);
            }
        });
        ui.separator();

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

        // Markdown body — rendered as monospaced text in a scrollable
        // panel for now. Frontmatter has already been stripped server-
        // side (see graph-api `read_body`). When `body` is empty (e.g.
        // an external node with no file, or a read failure), we just
        // hide the section rather than show a noisy placeholder.
        //
        // TODO(commonmark): swap the plain-text view for an actual
        // markdown renderer (`egui_commonmark`). Wikilinks in the body
        // should resolve via the existing `Navigate` BadgeAction path.
        if !meta.body.is_empty() {
            ui.separator();
            ui.label(
                egui::RichText::new("Body")
                    .color(crate::ui::theme::palette::TEXT)
                    .strong(),
            );
            ui.add_space(4.0);
            // CommonMark renderer: headings, lists, code blocks, links
            // all paint properly instead of the plain-text fallback. We
            // pass `state.markdown_cache` so the parsed AST + measured
            // galleys persist between frames; without it, a non-trivial
            // body would re-parse and re-layout every frame, spiking
            // the egui thread.
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui_commonmark::CommonMarkViewer::new()
                        .show(ui, &mut state.markdown_cache, &meta.body);
                });
        }
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
        let b = Badge::new(
            field,
            &label,
            BadgeKind::Wikilink {
                resolved: true,
                target: page.clone(),
            },
        );
        // body_target is unused for Wikilink (dispatch routes Navigate
        // directly via the target it carries); pass `trimmed` as a stable
        // placeholder.
        dispatch_badge(b.show(ui), trimmed, action);
        return;
    }

    // url
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
        dispatch_badge(b.show(ui), trimmed, action);
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
