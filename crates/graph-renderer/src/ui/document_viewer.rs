//! Reusable document viewer egui widget.
//!
//! Used by the command palette for file-preview, intended to be reused by
//! node-detail tabs / a future log viewer / a file-preview tab.
//!
//! Features:
//!   * Optional syntax highlighting via `syntect` (uses the pure-Rust
//!     `fancy-regex` engine so the same dep builds on wasm32).
//!   * Optional `[start..start+len)` range highlighting (used to surface
//!     fuzzy-match positions returned by `SkimMatcherV2::fuzzy_indices`).
//!   * Optional line numbers, soft-wrap, scroll-area cap.
//!
//! Lazy `OnceLock<SyntaxSet>` + `OnceLock<ThemeSet>` keep the (~MB-scale)
//! parsing assets out of the per-frame path. Themes are loaded once.
//!
//! If syntect ever stops compiling on wasm32, the `highlight_lines` helper
//! is the only place that needs swapping out — `show_*` paths fall back to
//! plain monospaced text with match highlighting only.

use std::sync::OnceLock;

use eframe::egui;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// A byte-range into the document text that should be highlighted.
/// Typically populated from `SkimMatcherV2::fuzzy_indices` (positions are
/// char indices there; we treat them as byte indices in this widget's API
/// for callers that already collapsed multi-byte runs — for plain ASCII
/// vault paths and short snippets they're identical).
#[derive(Debug, Clone, Copy)]
pub struct DocMatch {
    pub start: usize,
    pub len: usize,
}

/// Builder-style egui widget. Cheap to construct per frame.
pub struct DocumentViewer<'a> {
    text: &'a str,
    language: Option<&'a str>,
    matches: &'a [DocMatch],
    max_height: Option<f32>,
    line_numbers: bool,
    wrap: bool,
}

impl<'a> DocumentViewer<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            language: None,
            matches: &[],
            max_height: None,
            line_numbers: false,
            wrap: true,
        }
    }
    pub fn language(mut self, lang: &'a str) -> Self {
        self.language = Some(lang);
        self
    }
    pub fn matches(mut self, m: &'a [DocMatch]) -> Self {
        self.matches = m;
        self
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }
    pub fn line_numbers(mut self, on: bool) -> Self {
        self.line_numbers = on;
        self
    }
    pub fn wrap(mut self, on: bool) -> Self {
        self.wrap = on;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let mut inner_resp = None;
        let outer = if let Some(h) = self.max_height {
            egui::ScrollArea::vertical()
                .max_height(h)
                .auto_shrink([false, false])
                .show(ui, |ui| inner_resp = Some(self.render_body(ui)))
                .inner_rect
        } else {
            inner_resp = Some(self.render_body(ui));
            ui.min_rect()
        };
        // We don't really need the response value beyond a placeholder so
        // callers can chain `.on_hover_*` etc.
        inner_resp.unwrap_or_else(|| ui.allocate_rect(outer, egui::Sense::hover()))
    }

    fn render_body(self, ui: &mut egui::Ui) -> egui::Response {
        let job = build_layout_job(self.text, self.language, self.matches, self.line_numbers);
        let label = egui::Label::new(job).wrap_mode(if self.wrap {
            egui::TextWrapMode::Wrap
        } else {
            egui::TextWrapMode::Extend
        });
        ui.add(label)
    }
}

// --- syntect cache ---------------------------------------------------------

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

fn pick_theme() -> &'static Theme {
    static T: OnceLock<&'static Theme> = OnceLock::new();
    T.get_or_init(|| {
        let ts = theme_set();
        // Prefer a dark theme to match the jump-cannon UI; fall back to
        // whatever's first in the bundled set.
        ts.themes
            .get("base16-ocean.dark")
            .or_else(|| ts.themes.get("Solarized (dark)"))
            .or_else(|| ts.themes.values().next())
            .expect("syntect ships at least one default theme")
    })
}

fn pick_syntax<'s>(ss: &'s SyntaxSet, lang: Option<&str>) -> Option<&'s SyntaxReference> {
    let lang = lang?;
    // Try as extension first (e.g. "rs", "md", "py"), then as token / name.
    ss.find_syntax_by_extension(lang)
        .or_else(|| ss.find_syntax_by_token(lang))
        .or_else(|| ss.find_syntax_by_name(lang))
}

// --- layout job builder ----------------------------------------------------

/// Build an egui `LayoutJob` that combines syntect colour spans with
/// match-range underlines. Done in a single pass: for every syntect span we
/// further split it at any match-range boundary that intersects it so the
/// per-character attributes layer cleanly without re-laying-out twice.
fn build_layout_job(
    text: &str,
    language: Option<&str>,
    matches: &[DocMatch],
    line_numbers: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.break_anywhere = false;

    let ss = syntax_set();
    let theme = pick_theme();
    let syntax = pick_syntax(ss, language);

    // Pre-build a sorted list of match-range boundaries for fast splits.
    let mut boundaries: Vec<usize> = Vec::with_capacity(matches.len() * 2);
    for m in matches {
        boundaries.push(m.start);
        boundaries.push(m.start.saturating_add(m.len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let in_match = |byte: usize| matches.iter().any(|m| byte >= m.start && byte < m.start + m.len);

    let mono = egui::FontId::monospace(12.0);
    let mut byte_cursor = 0usize;
    let mut line_no: usize = 1;
    let total_lines = text.lines().count().max(1);
    let gutter_width = total_lines.to_string().len();

    if let Some(syn) = syntax {
        let mut hl = HighlightLines::new(syn, theme);
        for line in LinesWithEndings::from(text) {
            if line_numbers {
                push_gutter(&mut job, line_no, gutter_width, &mono);
            }
            let regions = hl
                .highlight_line(line, ss)
                .unwrap_or_else(|_| vec![(SyntectStyle::default(), line)]);
            for (style, piece) in regions {
                let piece_start = byte_cursor;
                let piece_end = piece_start + piece.len();
                emit_with_match_splits(
                    &mut job, piece, piece_start, piece_end, &boundaries, &in_match,
                    &mono, syntect_to_egui(style.foreground),
                );
                byte_cursor = piece_end;
            }
            line_no += 1;
        }
    } else {
        // Plain text fallback: no per-token colours, but still split for
        // match highlighting.
        for line in LinesWithEndings::from(text) {
            if line_numbers {
                push_gutter(&mut job, line_no, gutter_width, &mono);
            }
            let line_start = byte_cursor;
            let line_end = line_start + line.len();
            emit_with_match_splits(
                &mut job, line, line_start, line_end, &boundaries, &in_match,
                &mono, egui::Color32::from_gray(220),
            );
            byte_cursor = line_end;
            line_no += 1;
        }
    }

    job
}

fn push_gutter(job: &mut egui::text::LayoutJob, line_no: usize, width: usize, mono: &egui::FontId) {
    let mut fmt = egui::TextFormat::default();
    fmt.font_id = mono.clone();
    fmt.color = egui::Color32::from_gray(110);
    let s = format!("{:>w$}  ", line_no, w = width);
    job.append(&s, 0.0, fmt);
}

fn emit_with_match_splits(
    job: &mut egui::text::LayoutJob,
    piece: &str,
    piece_start: usize,
    piece_end: usize,
    boundaries: &[usize],
    in_match: &dyn Fn(usize) -> bool,
    mono: &egui::FontId,
    fg: egui::Color32,
) {
    // Walk boundary positions inside [piece_start, piece_end) and emit
    // sub-slices flagged with the match-style if their first byte is inside
    // any match range.
    let mut cuts: Vec<usize> = boundaries
        .iter()
        .copied()
        .filter(|&b| b > piece_start && b < piece_end)
        .collect();
    cuts.push(piece_end);
    let mut start = piece_start;
    for cut in cuts {
        let local_start = start - piece_start;
        let local_end = cut - piece_start;
        if local_start >= local_end {
            continue;
        }
        let sub = &piece[local_start..local_end];
        let highlighted = in_match(start);
        let mut fmt = egui::TextFormat::default();
        fmt.font_id = mono.clone();
        if highlighted {
            fmt.color = egui::Color32::from_rgb(0xff, 0xd5, 0x4a);
            fmt.background = egui::Color32::from_rgba_unmultiplied(0xff, 0xd5, 0x4a, 0x30);
        } else {
            fmt.color = fg;
        }
        job.append(sub, 0.0, fmt);
        start = cut;
    }
}

fn syntect_to_egui(c: syntect::highlighting::Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}
