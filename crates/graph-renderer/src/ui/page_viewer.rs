//! Editable Obsidian-page viewer.
//!
//! Replaces the inline markdown body that `render_anchored_panel` paints
//! for obsidian-page nodes. Two modes:
//!   * `Rendered` — `egui_commonmark::CommonMarkViewer` over the markdown.
//!   * `Source`   — `egui::TextEdit::multiline` editing the raw body.
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

use eframe::egui::{self, Color32, Key};

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
    /// Has the editor been initialised for this `meta.id` yet? Used to
    /// detect a node switch and re-seed `source_buffer` without
    /// trampling user edits between node visits.
    initialised_for: Option<String>,
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
/// isn't in the in-memory `VaultGraph` (server.rs:203). Every other
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
            if state.dirty {
                ui.label(
                    egui::RichText::new("unsaved")
                        .small()
                        .color(theme::accent::YELLOW),
                );
            }
        });
    });

    // 2. Save / status row.
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
            // Mark saving immediately so the UI shows the spinner; the
            // host will clear this when the fetch completes.
            state.saving = true;
            (actions.on_save)(&meta.path, &state.source_buffer);
        }
        if state.saving {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(egui::RichText::new("saving…").small().weak());
        }
        if let Some(err) = state.last_save_error.as_deref() {
            ui.label(
                egui::RichText::new(format!("save failed: {err}"))
                    .small()
                    .color(theme::accent::RED),
            );
        }
    });

    ui.separator();

    // 3. Body.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(360.0)
        .show(ui, |ui| match state.mode {
            ViewMode::Rendered => {
                // `meta.body` is already frontmatter-stripped server-
                // side. But: if the user toggles back to Rendered while
                // their edited `source_buffer` includes a leading
                // `---\n…---\n` block (e.g. they pasted one in), trim
                // it before handing to the viewer so the preview
                // matches what would be on disk.
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
                let edit = egui::TextEdit::multiline(&mut state.source_buffer)
                    .font(theme::mono(font_size::BODY))
                    .code_editor()
                    .desired_rows(20)
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .text_color(palette::TEXT);
                let resp = ui.add(edit);
                if resp.changed() {
                    state.recompute_dirty();
                }
            }
        });

    let _ = Color32::TRANSPARENT; // suppress unused-import lint when palette swaps.
}
