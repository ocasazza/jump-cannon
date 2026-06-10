//! Document panel — Dioxus port of crates/graph-renderer/src/ui/page_viewer.rs
//! at commit 723af10 (the editable Obsidian-page viewer), plus the
//! frontmatter chip strip the egui anchored panel painted above the body.
//!
//! Two modes, same as the egui viewer:
//!   * `Rendered` — markdown preview (pulldown-cmark here, egui_commonmark
//!     there) over the dirty buffer when edits exist, else the wire body;
//!   * `Source`  — a line-number-guttered textarea over the raw body, with
//!     a Cmd/Ctrl+F find strip, `Tab` → four-space soft indent, Cmd/Ctrl+S
//!     save, and a live `Ln, Col | lines, chars` status readout.
//!
//! ## Frontmatter handling
//!
//! `meta.body` arrives from `/node/:id` with the YAML frontmatter already
//! stripped server-side (`graph-api::server::read_body`). Source mode
//! therefore edits *body-only* markdown; the matching `PUT /vault/page`
//! preserves the on-disk frontmatter block verbatim and replaces only the
//! body. Frontmatter editing flows through the chip strip above the editor
//! (the same `crate::badges` surface the inspector renders).
//! `split_frontmatter` is retained as a defensive helper for the preview.
//!
//! ## Editing semantics (PageViewerState port)
//!
//! Per-node editor state lives in a module `GlobalSignal` map so switching
//! between pages preserves each one's in-progress edits — the egui App's
//! `HashMap<NodeId, PageViewerState>`. Node switch re-seeds the buffer; a
//! host body refresh (fresh fetch / post-save write-back) adopts the new
//! baseline silently only when the user hasn't started editing; a save
//! completion refreshes the baseline WITHOUT trampling keystrokes typed
//! while the PUT was in flight (`note_saved`). Save failures park the error
//! on the node's state with a retry affordance and survive a navigation
//! round-trip, clearing on the next save attempt. Deliberately not
//! persisted to localStorage: the egui PageViewerState was in-memory only.

use std::collections::HashMap;

use dioxus::events::{Key, KeyboardEvent, Modifiers};
use dioxus::prelude::*;
use panel_kit::Spinner;
use wasm_bindgen::JsCast;

use crate::panels::filter;
use crate::panels::inspector::badge_dispatch;
use crate::{api, badges, proto, Ctx};

/// DOM id of the source textarea — the cursor/selection helpers reach it
/// through `document.getElementById` (one Document panel per workspace).
const EDIT_ID: &str = "jc-doc-src";

// --- per-node editor state (PageViewerState port) -----------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ViewMode {
    #[default]
    Rendered,
    Source,
}

#[derive(Clone, Default, PartialEq)]
struct DocState {
    mode: ViewMode,
    /// Editable body buffer, seeded from `meta.body`.
    buffer: String,
    /// Original `meta.body` (post-server-strip) the buffer is diffed
    /// against to compute `dirty`. Updated on successful save.
    original: String,
    dirty: bool,
    /// In-flight save indicator.
    saving: bool,
    /// Last save error, or `None` after a successful save.
    last_save_error: Option<String>,
    /// When the last save completed (`Date::now()` ms). Keys the transient
    /// green "✓ saved" affordance — a CSS fade replaces the egui
    /// per-frame alpha ramp.
    saved_at_ms: Option<u64>,
    /// Find strip state — toggled by Cmd/Ctrl+F in Source mode, closed by
    /// Escape.
    find_open: bool,
    find_query: String,
    /// Last byte offset used as the "from" position for `Find next`, so
    /// repeated Enter walks through hits.
    find_last_jump: usize,
    /// Last known cursor position (UTF-16 units, as the DOM reports) for
    /// the `Ln, Col` readout.
    cursor_u16: usize,
}

impl DocState {
    fn recompute_dirty(&mut self) {
        self.dirty = self.buffer != self.original;
    }

    /// Mark a successful save: refresh the baseline, flush the dirty bit —
    /// WITHOUT overwriting `buffer`, so keystrokes typed while the save
    /// was in flight re-flag dirty instead of vanishing.
    fn note_saved(&mut self, new_body: String) {
        self.original = new_body;
        self.saving = false;
        self.last_save_error = None;
        self.saved_at_ms = Some(js_sys::Date::now() as u64);
        self.recompute_dirty();
    }

    fn note_save_error(&mut self, err: String) {
        self.saving = false;
        self.last_save_error = Some(err);
        self.saved_at_ms = None;
    }
}

/// Keyed by node id — the egui App's `page_viewer_states` map.
static DOCS: GlobalSignal<HashMap<String, DocState>> = Signal::global(HashMap::new);

fn with_doc<R>(id: &str, f: impl FnOnce(&mut DocState) -> R) -> R {
    f(DOCS.write().entry(id.to_string()).or_default())
}

// --- detection + text helpers (page_viewer.rs ports) ----------------------------------

/// Detect obsidian-page nodes. There is no `"obsidian_page"` sentinel
/// server-side; the only special doctype is `"external"` (the `/node/:id`
/// stub fallback). Rule: doctype != "external" AND path non-empty.
fn is_obsidian_page(meta: &proto::NodeMeta) -> bool {
    !meta.path.is_empty() && meta.doctype.as_deref() != Some("external")
}

/// Returns `(had_frontmatter, body)` from a raw markdown source — the
/// defensive `split_frontmatter` port. The wire strips frontmatter before
/// sending, so this normally passes the input through.
fn split_frontmatter(source: &str) -> &str {
    let Some(after_open) =
        source.strip_prefix("---\n").or_else(|| source.strip_prefix("---\r\n"))
    else {
        return source;
    };
    let mut start = 0usize;
    while start < after_open.len() {
        let line_end =
            after_open[start..].find('\n').map(|i| start + i).unwrap_or(after_open.len());
        if after_open[start..line_end].trim_end_matches('\r') == "---" {
            let mut body_start = line_end;
            if after_open[body_start..].starts_with('\n') {
                body_start += 1;
            }
            return &after_open[body_start..];
        }
        start = line_end + 1;
    }
    // Unclosed frontmatter: treat the whole thing as body.
    source
}

/// (line, col, total_lines, total_chars) for a byte offset into `buf` —
/// line/col 1-based for human display. Port of `cursor_stats`.
fn cursor_stats(buf: &str, byte_off: usize) -> (usize, usize, usize, usize) {
    let off = byte_off.min(buf.len());
    // Clamp to a char boundary (the UTF-16 → byte conversion already lands
    // on one, but stay defensive).
    let off = (0..=off).rev().find(|&i| buf.is_char_boundary(i)).unwrap_or(0);
    let head = &buf[..off];
    let line = head.bytes().filter(|b| *b == b'\n').count() + 1;
    let col = head.rsplit('\n').next().map(|s| s.chars().count() + 1).unwrap_or(1);
    let total_lines =
        if buf.is_empty() { 1 } else { buf.bytes().filter(|b| *b == b'\n').count() + 1 };
    (line, col, total_lines, buf.chars().count())
}

/// UTF-16 offset (what the DOM's `selectionStart` reports) → byte offset.
fn u16_to_byte(s: &str, u16_idx: usize) -> usize {
    let mut u16s = 0usize;
    for (b, ch) in s.char_indices() {
        if u16s >= u16_idx {
            return b;
        }
        u16s += ch.len_utf16();
    }
    s.len()
}

/// Byte offset → UTF-16 offset.
fn byte_to_u16(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx.min(s.len())].encode_utf16().count()
}

// --- DOM cursor/selection helpers ------------------------------------------------------

/// The web-sys feature set doesn't include HtmlTextAreaElement, so the
/// selection API goes through js-sys reflection on the raw element — still
/// all-Rust, no JS shims.
fn edit_el() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.get_element_by_id(EDIT_ID)
}

/// Read `selectionStart` (UTF-16 units) from the source textarea.
fn dom_cursor_u16() -> usize {
    edit_el()
        .and_then(|el| js_sys::Reflect::get(&el, &"selectionStart".into()).ok())
        .and_then(|v| v.as_f64())
        .map(|f| f as usize)
        .unwrap_or(0)
}

/// `setSelectionRange(start, end)` + `focus()` on the source textarea.
fn dom_set_selection_u16(start: usize, end: usize) {
    let Some(el) = edit_el() else { return };
    if let Ok(f) = js_sys::Reflect::get(&el, &"focus".into()) {
        if let Some(func) = f.dyn_ref::<js_sys::Function>() {
            let _ = func.call0(&el);
        }
    }
    if let Ok(f) = js_sys::Reflect::get(&el, &"setSelectionRange".into()) {
        if let Some(func) = f.dyn_ref::<js_sys::Function>() {
            let _ = func.call2(&el, &(start as f64).into(), &(end as f64).into());
        }
    }
}

// --- editing actions --------------------------------------------------------------------

/// Insert `s` at the textarea cursor — the Tab → 4-spaces shim. The
/// controlled-textarea value rewrite resets the DOM cursor, so the new
/// position is restored on the next tick.
fn insert_at_cursor(node_id: &str, s: &str) {
    let cur = dom_cursor_u16();
    with_doc(node_id, |d| {
        let cur = cur.min(d.buffer.encode_utf16().count());
        let byte = u16_to_byte(&d.buffer, cur);
        d.buffer.insert_str(byte, s);
        d.recompute_dirty();
        d.cursor_u16 = cur + s.encode_utf16().count();
    });
    let target = cur + s.encode_utf16().count();
    spawn(async move {
        gloo_timers::future::TimeoutFuture::new(0).await;
        dom_set_selection_u16(target, target);
    });
}

/// Walk the buffer for the next find hit after `find_last_jump`, wrapping
/// to the start when exhausted; select it in the textarea — port of
/// `jump_to_next_match`.
fn jump_to_next_match(node_id: &str) {
    let sel = with_doc(node_id, |d| {
        if d.find_query.is_empty() {
            return None;
        }
        let from = d.find_last_jump.min(d.buffer.len());
        let hit = d.buffer[from..]
            .find(d.find_query.as_str())
            .map(|i| from + i)
            .or_else(|| d.buffer.find(d.find_query.as_str()))?;
        d.find_last_jump = hit + d.find_query.len();
        let start = byte_to_u16(&d.buffer, hit);
        let end = byte_to_u16(&d.buffer, hit + d.find_query.len());
        d.cursor_u16 = end;
        Some((start, end))
    });
    if let Some((start, end)) = sel {
        dom_set_selection_u16(start, end);
    }
}

/// Spawn the `PUT /vault/page` save — the egui `kick_off_page_save` +
/// drain pair. `force` is the retry path (egui's retry button skips the
/// dirty gate). On success the saved body is written back into the cached
/// `ctx.meta` so re-renders pick up the saved content.
fn try_save(ctx: Ctx, node_id: String, path: String, force: bool) {
    let Some(body) = with_doc(&node_id, |d| {
        if d.saving || (!d.dirty && !force) {
            return None;
        }
        d.saving = true;
        d.last_save_error = None;
        Some(d.buffer.clone())
    }) else {
        return;
    };
    let mut meta = ctx.meta;
    spawn(async move {
        let res = api::put_page(&path, &body).await;
        let ok = matches!(&res, Ok(r) if r.ok);
        with_doc(&node_id, |d| match res {
            Ok(r) if r.ok => d.note_saved(body.clone()),
            Ok(r) => d.note_save_error(r.error.unwrap_or_else(|| "rejected".into())),
            Err(e) => d.note_save_error(e),
        });
        if ok {
            if let Some(m) = meta.write().as_mut() {
                if m.id == node_id {
                    m.body = body;
                }
            }
        }
    });
}

// --- markdown --------------------------------------------------------------------------

fn render_markdown(md: &str) -> String {
    let parser = pulldown_cmark::Parser::new(md);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

// --- panel -----------------------------------------------------------------------------

pub(crate) fn panel(ctx: Ctx) -> Element {
    let Ctx { meta, meta_busy, selected, .. } = ctx;
    if *meta_busy.read() {
        return rsx! { div { class: "skeleton", Spinner { label: "loading node…" } } };
    }
    let m_guard = meta.read();
    let Some(m) = m_guard.as_ref() else {
        return rsx! { div { class: "empty",
            if selected.read().is_some() { "node failed to load" } else { "select a node" }
        } };
    };
    if !is_obsidian_page(m) {
        // Stub/external nodes have no file on disk — nothing to edit.
        // Show the body read-only if the server sent one anyway.
        let body = m.body.clone();
        return rsx! { div { class: "docv",
            div { class: "empty", "external node — no file on disk" }
            if !body.is_empty() {
                div { class: "rendered-md", dangerous_inner_html: render_markdown(&body) }
            }
        } };
    }

    let node_id = m.id.clone();
    let path = m.path.clone();

    // Seed / refresh the per-node editor state (show_in_panel's head):
    // first visit seeds the buffer; a host body update (fresh fetch,
    // post-save write-back) re-baselines only when there are no edits to
    // trample. Render-time signal write — guarded so it settles in one
    // extra pass instead of looping.
    {
        let need = match DOCS.read().get(&node_id) {
            None => true,
            Some(d) => !d.dirty && d.original != m.body,
        };
        if need {
            let body = m.body.clone();
            with_doc(&node_id, move |d| {
                d.buffer = body.clone();
                d.original = body;
                d.recompute_dirty();
                d.find_last_jump = 0;
                // Keep last_save_error: a failed save the user navigated
                // away from is still useful context if they come back.
            });
        }
    }
    let st = DOCS.read().get(&node_id).cloned().unwrap_or_default();

    // Frontmatter chip strip — the surface the egui anchored panel painted
    // above the page viewer (tags/doctype/folder + frontmatter chips,
    // shared routing with the inspector).
    let chip_strip: Element = {
        let q = filter::QUERY.read();
        let is_active = |f: &str, v: &str| q.is_filter_active(f, v);
        let on_action = badge_dispatch(ctx, node_id.clone());
        let has_frontmatter = !m.frontmatter_json.is_empty()
            && m.frontmatter_json != "{}"
            && m.frontmatter_json != "null";
        if m.tags.is_empty() && m.folder.is_empty() && m.doctype.is_none() && !has_frontmatter {
            rsx! {}
        } else {
            rsx! {
                div { class: "tags",
                    { badges::node_badges(m, &is_active, None, on_action) }
                    if has_frontmatter {
                        { badges::frontmatter_chips(&m.frontmatter_json, &is_active, None, on_action) }
                    }
                }
            }
        }
    };

    // Status strip (tab-row right edge): saving spinner / unsaved dot /
    // the transient green "saved" flash. The egui per-frame alpha ramp is
    // a 2s CSS fade here, keyed on the save timestamp so every save
    // restarts the animation.
    let status_el: Element = if st.saving {
        rsx! {
            Spinner {}
            span { class: "doc-dim", "saving" }
        }
    } else if st.dirty {
        rsx! { span { class: "doc-unsaved", "● unsaved" } }
    } else if let Some(ts) = st.saved_at_ms {
        rsx! { span { key: "{ts}", class: "doc-saved", "✓ saved" } }
    } else {
        rsx! {}
    };

    // Save row data.
    let save_enabled = st.dirty && !st.saving;
    let byte_cursor = u16_to_byte(&st.buffer, st.cursor_u16);
    let (line, col, total_lines, total_chars) = cursor_stats(&st.buffer, byte_cursor);

    let body_el: Element = match st.mode {
        ViewMode::Rendered => {
            // Preview the dirty buffer when edits exist, else the wire body.
            let source = if st.dirty { st.buffer.clone() } else { m.body.clone() };
            let body_text = split_frontmatter(&source).to_string();
            if body_text.is_empty() {
                rsx! { div { class: "ins-note", "(empty body)" } }
            } else {
                rsx! { div { class: "rendered-md", dangerous_inner_html: render_markdown(&body_text) } }
            }
        }
        ViewMode::Source => source_editor(ctx, &node_id, &path, &st, total_lines),
    };

    let (id_t1, id_t2) = (node_id.clone(), node_id.clone());
    let (id_save, path_save) = (node_id.clone(), path.clone());
    let (id_retry, path_retry) = (node_id.clone(), path.clone());
    rsx! {
        div { class: "docv",
            { chip_strip }

            // 1. Tab strip + at-a-glance status.
            div { class: "doc-tabs",
                button {
                    class: if st.mode == ViewMode::Rendered { "doc-tab active" } else { "doc-tab" },
                    onclick: move |_| with_doc(&id_t1, |d| d.mode = ViewMode::Rendered),
                    "Rendered"
                }
                button {
                    class: if st.mode == ViewMode::Source { "doc-tab active" } else { "doc-tab" },
                    onclick: move |_| with_doc(&id_t2, |d| d.mode = ViewMode::Source),
                    "Source"
                }
                span { class: "doc-status", { status_el } }
            }

            // 2. Save / status row.
            div { class: "doc-saverow",
                button { class: "btn", disabled: !save_enabled,
                    onclick: move |_| try_save(ctx, id_save.clone(), path_save.clone(), false),
                    "Save (Cmd+S)"
                }
                if let Some(err) = st.last_save_error.clone() {
                    button { class: "btn doc-retry", title: "{err}",
                        onclick: move |_| try_save(ctx, id_retry.clone(), path_retry.clone(), true),
                        "retry"
                    }
                    span { class: "doc-err", "save failed: {err}" }
                }
                span { class: "doc-stats",
                    "Ln {line}, Col {col}  |  {total_lines} lines, {total_chars} chars"
                }
            }

            // 3. Body.
            div { class: "doc-body",
                { body_el }
            }
        }
    }
}

/// Source tab — find strip + line-number gutter + markdown textarea.
///
/// PARITY GAP: no syntect markdown syntax highlighting in the editor (the
/// egui Source tab ran a syntect layouter through egui_extras); syntect is
/// not a dependency of this crate and new deps are out of scope, so the
/// source renders as plain monospace.
fn source_editor(ctx: Ctx, node_id: &str, path: &str, st: &DocState, total_lines: usize) -> Element {
    // Same logical-vs-visual line caveat as the egui gutter: long wrapped
    // lines diverge — accepted, markdown body lines are usually short.
    let gutter: String = (1..=total_lines).map(|n| format!("{n}\n")).collect();
    let rows = total_lines.max(20);

    let match_count = if st.find_query.is_empty() {
        0
    } else {
        st.buffer.matches(st.find_query.as_str()).count()
    };

    let id_in = node_id.to_string();
    let id_kd = node_id.to_string();
    let id_cur = node_id.to_string();
    let id_cur2 = node_id.to_string();
    let id_fq = node_id.to_string();
    let id_fkd = node_id.to_string();
    let id_fnext = node_id.to_string();
    let path_kd = path.to_string();
    let find_query = st.find_query.clone();
    let buffer = st.buffer.clone();

    rsx! {
        if st.find_open {
            div { class: "doc-find",
                span { class: "doc-dim", "Find" }
                input {
                    class: "filter doc-findin",
                    value: "{find_query}",
                    oninput: move |e| with_doc(&id_fq, |d| {
                        d.find_query = e.value();
                        d.find_last_jump = 0;
                    }),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Enter {
                            jump_to_next_match(&id_fkd);
                        }
                    },
                }
                span { class: "doc-dim", "{match_count} matches" }
                button { class: "btn", title: "Enter while focused in the find field",
                    onclick: move |_| jump_to_next_match(&id_fnext),
                    "Find next"
                }
            }
        }
        div { class: "doc-srcwrap",
            pre { class: "doc-gutter", "{gutter}" }
            textarea {
                id: EDIT_ID,
                class: "doc-src",
                spellcheck: false,
                rows: "{rows}",
                value: "{buffer}",
                oninput: move |e| {
                    let v = e.value();
                    let cur = dom_cursor_u16();
                    with_doc(&id_in, |d| {
                        d.buffer = v;
                        d.recompute_dirty();
                        // Clamp the find-walk position to the new length so
                        // an in-progress "Find next" doesn't snap to the top
                        // on every keystroke.
                        d.find_last_jump = d.find_last_jump.min(d.buffer.len());
                        d.cursor_u16 = cur;
                    });
                },
                // Cursor readout tracking — click + arrow keys move the
                // caret without an input event.
                onclick: move |_| {
                    let cur = dom_cursor_u16();
                    with_doc(&id_cur, |d| d.cursor_u16 = cur);
                },
                onkeyup: move |_| {
                    let cur = dom_cursor_u16();
                    with_doc(&id_cur2, |d| d.cursor_u16 = cur);
                },
                onkeydown: move |e: KeyboardEvent| {
                    let mods = e.modifiers();
                    let cmd = mods.contains(Modifiers::META) || mods.contains(Modifiers::CONTROL);
                    match e.key() {
                        // Tab → four-space soft indent (egui consumed the
                        // raw Tab before the TextEdit saw it).
                        Key::Tab if !cmd => {
                            e.prevent_default();
                            insert_at_cursor(&id_kd, "    ");
                        }
                        Key::Escape => with_doc(&id_kd, |d| d.find_open = false),
                        Key::Character(c) if cmd && c.eq_ignore_ascii_case("s") => {
                            e.prevent_default();
                            try_save(ctx, id_kd.clone(), path_kd.clone(), false);
                        }
                        Key::Character(c) if cmd && c.eq_ignore_ascii_case("f") => {
                            e.prevent_default();
                            with_doc(&id_kd, |d| d.find_open = !d.find_open);
                        }
                        _ => {}
                    }
                },
            }
        }
    }
}
