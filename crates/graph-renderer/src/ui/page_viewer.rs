//! Editable Obsidian-page viewer.
//!
//! Replaces the inline markdown body that `render_anchored_panel` paints
//! for obsidian-page nodes. Two modes:
//!   * `Rendered` — `egui_commonmark::CommonMarkViewer` over the markdown.
//!   * `Source`   — `egui::TextEdit::multiline` editing the raw body,
//!     with a syntax-highlighted layouter (markdown grammar via syntect
//!     through `egui_extras::syntax_highlighting`), a line-number gutter,
//!     a Cmd/Ctrl+F find strip, and `Tab` → four-space soft indent.
//!
//! ## Frontmatter handling
//!
//! `meta.body` arrives from `/node/:id` with the YAML frontmatter
//! already stripped server-side (`graph-api::server::read_body`). The
//! `Source` mode therefore edits *body-only* markdown, NOT the full
//! `---\n…---\n` block + body. The matching `PUT /vault/page` endpoint
//! reads the existing file, preserves its frontmatter block verbatim,
//! and replaces only the body. This keeps the editor's surface focused
//! on prose while frontmatter editing continues to flow through the
//! chip strip surface (rendered above the body in the anchored panel).
//!
//! `split_frontmatter` is retained as a defensive helper — if the wire
//! contract ever changes to ship raw markdown, the rendered preview can
//! call it to trim the YAML before handing to the CommonMark viewer.

use eframe::egui::{self, text::CCursor, text::CCursorRange, Key, Modifiers};
use web_time::Instant;

use crate::proto;
use crate::ui::theme::{self, font_size, palette};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ViewMode {
    Rendered,
    Source,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode::Rendered
    }
}

/// Per-node editor state. Owned by the App in a
/// `HashMap<NodeId, PageViewerState>` so switching between obsidian
/// pages preserves each one's in-progress edits.
#[derive(Default)]
pub struct PageViewerState {
    pub mode: ViewMode,
    /// Editable body buffer. Lazily populated from `meta.body` on the
    /// first frame we enter Source mode for this node.
    pub source_buffer: String,
    /// Original `meta.body` (post-server-strip) the source buffer is
    /// diffed against to compute `dirty`. Updated on successful save.
    pub original_body: String,
    /// Set by the host (App) when the source buffer differs from
    /// `original_body`.
    pub dirty: bool,
    /// In-flight save indicator. Host clears on completion.
    pub saving: bool,
    /// Last save error, or `None` after a successful save.
    pub last_save_error: Option<String>,
    /// When the last save completed successfully. Drives the
    /// transient green "Saved" affordance in the status row; fades
    /// out 2s after the timestamp.
    pub last_saved_at: Option<Instant>,
    /// Has the editor been initialised for this `meta.id` yet? Used to
    /// detect a node switch and re-seed `source_buffer` without
    /// trampling user edits between node visits.
    initialised_for: Option<String>,
    /// Find strip state. Toggled by Cmd/Ctrl+F while focus is in the
    /// Source tab; closed by Escape.
    find_open: bool,
    find_query: String,
    /// Last byte offset used as the "from" position for `Find next`.
    /// Lets repeated Enter walk through hits without re-reading the
    /// TextEdit cursor every frame.
    find_last_jump: usize,
}

impl PageViewerState {
    /// Recompute `dirty` after a buffer or `original_body` change.
    fn recompute_dirty(&mut self) {
        self.dirty = self.source_buffer != self.original_body;
    }

    /// Mark a successful save: clear the error, refresh the baseline,
    /// flush the dirty bit.
    pub fn note_saved(&mut self, new_body: String) {
        self.original_body = new_body.clone();
        // Don't overwrite source_buffer — the user might have kept
        // typing while the save was in flight. If they did, the next
        // recompute_dirty() will flag the buffer as dirty again. If
        // they didn't, source_buffer == original_body and dirty stays
        // false.
        self.saving = false;
        self.last_save_error = None;
        self.last_saved_at = Some(Instant::now());
        self.recompute_dirty();
        // Note: callers should also write `new_body` back to the
        // App-cached `meta.body` so re-renders pick up the saved
        // content; this struct only tracks editor-local state.
        let _ = new_body;
    }

    /// Mark a failed save.
    pub fn note_save_error(&mut self, err: String) {
        self.saving = false;
        self.last_save_error = Some(err);
        self.last_saved_at = None;
    }
}

/// Caller-supplied hooks. `markdown_cache` is reused across frames so
/// the CommonMark viewer keeps its parsed AST + galleys between paints.
/// `on_save` is invoked when the user clicks Save (or hits Cmd/Ctrl+S)
/// and the buffer is dirty + no save is in flight; the App is expected
/// to spawn the HTTP PUT and update `state.saving`.
pub struct PageViewerActions<'a> {
    pub markdown_cache: &'a mut egui_commonmark::CommonMarkCache,
    pub on_save: &'a mut dyn FnMut(&str, &str),
}

/// Detect obsidian-page nodes.
///
/// There is no `"obsidian_page"` sentinel server-side (verified by
/// grepping `graph-api/src/*.rs`). The only special doctype is
/// `"external"`, used by the `/node/:id` stub fallback when an id
/// isn't in the in-memory `VaultGraph` (server.rs:436). Every other
/// node was sourced from a real `.md` file in the vault and is an
/// editable obsidian page.
///
/// Rule: doctype != `Some("external")` AND path is non-empty.
pub fn is_obsidian_page(meta: &proto::NodeMeta) -> bool {
    if meta.path.is_empty() {
        return false;
    }
    meta.doctype.as_deref() != Some("external")
}

/// Returns `(frontmatter_yaml, body)` from a raw markdown source. When
/// there is no leading `---\n…---\n` block, returns `(None, source)`.
///
/// Used defensively — see the module docstring. The wire format strips
/// frontmatter before sending so this normally returns `(None, body)`.
pub fn split_frontmatter(source: &str) -> (Option<&str>, &str) {
    let after_open = match source.strip_prefix("---\n").or_else(|| source.strip_prefix("---\r\n"))
    {
        Some(s) => s,
        None => return (None, source),
    };
    // Scan for a closing `---` line.
    let mut start = 0usize;
    while start < after_open.len() {
        let line_end = after_open[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(after_open.len());
        let line = after_open[start..line_end].trim_end_matches('\r');
        if line == "---" {
            let yaml = &after_open[..start.saturating_sub(0)];
            // `after_open[..start]` is the YAML body BEFORE the closing
            // fence; trim trailing newline so the caller sees clean YAML.
            let yaml = yaml.trim_end_matches('\n').trim_end_matches('\r');
            // Body starts after the closing `---` line + its newline.
            let mut body_start = line_end;
            if after_open[body_start..].starts_with('\n') {
                body_start += 1;
            }
            return (Some(yaml), &after_open[body_start..]);
        }
        start = line_end + 1;
    }
    // Unclosed frontmatter: treat the whole thing as body.
    (None, source)
}

/// Compute (line_index, col_index, total_lines, total_chars) for a
/// 0-based byte offset into `buf`. `line_index` and `col_index` are
/// 1-based for human display. Line breaks counted as `\n`.
fn cursor_stats(buf: &str, byte_off: usize) -> (usize, usize, usize, usize) {
    let off = byte_off.min(buf.len());
    let head = &buf[..off];
    let line = head.bytes().filter(|b| *b == b'\n').count() + 1;
    let col = head
        .rsplit('\n')
        .next()
        .map(|s| s.chars().count() + 1)
        .unwrap_or(1);
    let total_lines = if buf.is_empty() {
        1
    } else {
        buf.bytes().filter(|b| *b == b'\n').count() + 1
    };
    let total_chars = buf.chars().count();
    (line, col, total_lines, total_chars)
}

/// Convert a char index (from a `CCursor`) into a byte offset into `s`,
/// clamped to `s.len()`.
fn char_idx_to_byte(s: &str, char_idx: usize) -> usize {
    let mut bytes = 0usize;
    for (i, ch) in s.chars().enumerate() {
        if i == char_idx {
            return bytes;
        }
        bytes += ch.len_utf8();
    }
    s.len()
}

/// Render the page viewer inside an existing `ui` (typically the body
/// of an anchored panel). The caller is responsible for the frontmatter
/// chip strip / metadata above this block — `show_in_panel` only owns
/// the tab strip + save row + body.
pub fn show_in_panel(
    ui: &mut egui::Ui,
    state: &mut PageViewerState,
    meta: &proto::NodeMeta,
    actions: &mut PageViewerActions,
) {
    // Re-seed source buffer on node switch. Preserve in-progress edits
    // when re-rendering the same node across frames.
    let switched = state.initialised_for.as_deref() != Some(meta.id.as_str());
    if switched {
        state.source_buffer = meta.body.clone();
        state.original_body = meta.body.clone();
        state.initialised_for = Some(meta.id.clone());
        state.dirty = false;
        state.find_open = false;
        state.find_last_jump = 0;
        // Don't clear last_save_error on switch — a failed save the
        // user navigated away from is still useful context if they
        // come back. It clears on the next successful save attempt.
    } else if state.original_body != meta.body && !state.dirty {
        // The host updated `meta.body` (e.g. fresh fetch) and the user
        // hasn't started editing — adopt the new baseline silently.
        state.source_buffer = meta.body.clone();
        state.original_body = meta.body.clone();
    }

    // 1. Tab strip.
    ui.horizontal(|ui| {
        let mut mode = state.mode;
        ui.selectable_value(&mut mode, ViewMode::Rendered, "Rendered");
        ui.selectable_value(&mut mode, ViewMode::Source, "Source");
        state.mode = mode;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // At-a-glance status indicator (full status row lives below).
            if state.saving {
                ui.add(egui::Spinner::new().size(10.0));
                ui.label(egui::RichText::new("saving").small().weak());
            } else if state.dirty {
                ui.label(
                    egui::RichText::new("\u{25CF} unsaved")
                        .small()
                        .color(theme::accent::YELLOW),
                );
            } else if let Some(t) = state.last_saved_at {
                let secs = t.elapsed().as_secs_f32();
                if secs < 2.0 {
                    let a = (1.0 - secs / 2.0).clamp(0.0, 1.0);
                    let c = palette::GOOD;
                    let faded = egui::Color32::from_rgba_unmultiplied(
                        c.r(),
                        c.g(),
                        c.b(),
                        (a * 255.0) as u8,
                    );
                    ui.label(
                        egui::RichText::new("\u{2713} saved").small().color(faded),
                    );
                    ui.ctx().request_repaint();
                }
            }
        });
    });

    // 2. Save / status row.
    let edit_id = ui.make_persistent_id(("page-viewer-source", meta.id.as_str()));
    ui.horizontal(|ui| {
        let save_enabled = state.dirty && !state.saving;
        let resp = ui.add_enabled(
            save_enabled,
            egui::Button::new(
                egui::RichText::new("Save (Cmd+S)").color(palette::TEXT),
            ),
        );
        let cmd_s = ui.input(|i| i.key_pressed(Key::S) && i.modifiers.command);
        if save_enabled && (resp.clicked() || cmd_s) {
            state.saving = true;
            (actions.on_save)(&meta.path, &state.source_buffer);
        }
        if let Some(err) = state.last_save_error.clone() {
            if ui
                .small_button(egui::RichText::new("retry").color(theme::accent::RED))
                .on_hover_text(err.clone())
                .clicked()
            {
                state.saving = true;
                state.last_save_error = None;
                (actions.on_save)(&meta.path, &state.source_buffer);
            }
            ui.label(
                egui::RichText::new(format!("save failed: {err}"))
                    .small()
                    .color(theme::accent::RED),
            );
        }

        // Right-aligned live cursor + buffer stats. Cheap; recomputed
        // each frame.
        let cursor_off = egui::TextEdit::load_state(ui.ctx(), edit_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| char_idx_to_byte(&state.source_buffer, r.primary.index))
            .unwrap_or(0);
        let (line, col, total_lines, total_chars) =
            cursor_stats(&state.source_buffer, cursor_off);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Ln {line}, Col {col}  |  {total_lines} lines, {total_chars} chars"
                ))
                .small()
                .weak()
                .monospace(),
            );
        });
    });

    ui.separator();

    // 3. Body.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(360.0)
        .show(ui, |ui| match state.mode {
            ViewMode::Rendered => {
                let source = if state.dirty {
                    state.source_buffer.as_str()
                } else {
                    meta.body.as_str()
                };
                let (_, body_text) = split_frontmatter(source);
                if body_text.is_empty() {
                    ui.label(
                        egui::RichText::new("(empty body)")
                            .small()
                            .weak()
                            .italics(),
                    );
                } else {
                    egui_commonmark::CommonMarkViewer::new()
                        .show(ui, actions.markdown_cache, body_text);
                }
            }
            ViewMode::Source => {
                source_editor(ui, state, edit_id);
            }
        });
}

/// Source tab — find strip + gutter + syntax-highlighted markdown
/// TextEdit. Factored out of `show_in_panel` so the per-mode branch
/// stays readable.
fn source_editor(ui: &mut egui::Ui, state: &mut PageViewerState, edit_id: egui::Id) {
    // Find-strip toggle: Ctrl/Cmd+F opens, Esc closes. Consumed before
    // the TextEdit sees the event so the editor doesn't insert an 'f'.
    let toggle_find = ui.input_mut(|i| {
        i.consume_key(Modifiers::COMMAND, Key::F) || i.consume_key(Modifiers::CTRL, Key::F)
    });
    if toggle_find {
        state.find_open = !state.find_open;
    }
    if state.find_open && ui.input(|i| i.key_pressed(Key::Escape)) {
        state.find_open = false;
    }

    if state.find_open {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Find").small().weak());
            let find_resp = ui.add(
                egui::TextEdit::singleline(&mut state.find_query)
                    .desired_width(180.0)
                    .font(theme::mono(font_size::BODY)),
            );
            let match_count = if state.find_query.is_empty() {
                0
            } else {
                state
                    .source_buffer
                    .matches(state.find_query.as_str())
                    .count()
            };
            ui.label(
                egui::RichText::new(format!("{match_count} matches"))
                    .small()
                    .weak(),
            );
            let next_clicked = ui
                .small_button("Find next")
                .on_hover_text("Enter while focused in the find field")
                .clicked();
            let enter_in_find =
                find_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (next_clicked || enter_in_find) && !state.find_query.is_empty() {
                jump_to_next_match(ui.ctx(), edit_id, state);
            }
        });
    }

    // Tab → 4 spaces. Has to run BEFORE the TextEdit so the editor
    // doesn't get the raw Tab event. Only intercept while focused.
    let editor_focused = ui.memory(|m| m.has_focus(edit_id));
    if editor_focused {
        let tab_pressed = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
        if tab_pressed {
            insert_at_cursor(ui.ctx(), edit_id, state, "    ");
        }
    }

    // Build the layouter. `highlight` is memoised inside egui_extras
    // per `(font, theme, code, lang)` so calling it every layout pass
    // is cheap.
    let code_theme =
        egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
    let mut layouter = |ui: &egui::Ui, source: &str, wrap_width: f32| {
        let mut job = egui_extras::syntax_highlighting::highlight(
            ui.ctx(),
            ui.style(),
            &code_theme,
            source,
            "md",
        );
        job.wrap.max_width = wrap_width;
        ui.fonts(|f| f.layout_job(job))
    };

    // Body = gutter + TextEdit side by side. The outer ScrollArea
    // already scrolls both as one column, so no scroll-sync needed.
    // Limitation: long source lines that wrap will cause the gutter's
    // logical-line count to diverge from the editor's visual-line
    // count — accept it (markdown body lines are usually short).
    let total_lines = state.source_buffer.bytes().filter(|b| *b == b'\n').count() + 1;
    let digit_width = total_lines.to_string().len();
    let gutter_width = (digit_width as f32 * 7.5).max(20.0) + 10.0;

    egui::Frame::none()
        .fill(palette::EDITOR_BG)
        .inner_margin(egui::Margin::symmetric(0.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                egui::Frame::none()
                    .fill(palette::EDITOR_GUTTER_BG)
                    .inner_margin(egui::Margin::symmetric(4.0, 0.0))
                    .show(ui, |ui| {
                        ui.set_width(gutter_width);
                        ui.set_min_height(0.0);
                        let mut buf =
                            String::with_capacity(total_lines * (digit_width + 1));
                        for n in 1..=total_lines {
                            use std::fmt::Write;
                            let _ = writeln!(buf, "{:>width$}", n, width = digit_width);
                        }
                        let buf = buf.trim_end_matches('\n').to_string();
                        ui.label(
                            egui::RichText::new(buf)
                                .small()
                                .monospace()
                                .color(palette::GREY),
                        );
                    });

                let edit = egui::TextEdit::multiline(&mut state.source_buffer)
                    .id(edit_id)
                    .font(theme::mono(font_size::BODY))
                    .code_editor()
                    .desired_rows(20)
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .text_color(palette::TEXT)
                    .layouter(&mut layouter);
                let resp = ui.add(edit);
                if resp.changed() {
                    state.recompute_dirty();
                    // Buffer changed → previous `find_last_jump` byte
                    // offset may no longer be valid. Clamp to the new
                    // buffer length instead of resetting to 0, so an
                    // in-progress "Find next" walk doesn't snap back
                    // to the top on every keystroke.
                    state.find_last_jump = state
                        .find_last_jump
                        .min(state.source_buffer.len());
                }
            });
        });
}

/// Insert `s` at the current cursor of the TextEdit identified by
/// `edit_id`. Used by the Tab → 4-spaces shim.
fn insert_at_cursor(
    ctx: &egui::Context,
    edit_id: egui::Id,
    state: &mut PageViewerState,
    s: &str,
) {
    let mut text_state =
        egui::TextEdit::load_state(ctx, edit_id).unwrap_or_default();
    let cursor_idx = text_state
        .cursor
        .char_range()
        .map(|r| r.primary.index.min(state.source_buffer.chars().count()))
        .unwrap_or_else(|| state.source_buffer.chars().count());
    let byte_off = char_idx_to_byte(&state.source_buffer, cursor_idx);
    state.source_buffer.insert_str(byte_off, s);
    state.recompute_dirty();
    let new_idx = cursor_idx + s.chars().count();
    let new_cursor = CCursor::new(new_idx);
    text_state
        .cursor
        .set_char_range(Some(CCursorRange::one(new_cursor)));
    text_state.store(ctx, edit_id);
}

/// Walk `source_buffer` for the next occurrence of `find_query` after
/// `find_last_jump`; wrap to the start if there are no further hits.
/// Updates the TextEdit cursor + `find_last_jump`.
fn jump_to_next_match(
    ctx: &egui::Context,
    edit_id: egui::Id,
    state: &mut PageViewerState,
) {
    if state.find_query.is_empty() {
        return;
    }
    let from = state.find_last_jump.min(state.source_buffer.len());
    let hit = state.source_buffer[from..]
        .find(state.find_query.as_str())
        .map(|i| from + i)
        .or_else(|| state.source_buffer.find(state.find_query.as_str()));
    let Some(byte_idx) = hit else {
        return;
    };
    let char_idx = state.source_buffer[..byte_idx].chars().count();
    let char_end = char_idx + state.find_query.chars().count();
    let mut text_state =
        egui::TextEdit::load_state(ctx, edit_id).unwrap_or_default();
    let primary = CCursor::new(char_end);
    let secondary = CCursor::new(char_idx);
    text_state.cursor.set_char_range(Some(CCursorRange {
        primary,
        secondary,
    }));
    text_state.store(ctx, edit_id);
    state.find_last_jump = byte_idx + state.find_query.len();
    ctx.memory_mut(|m| m.request_focus(edit_id));
}
