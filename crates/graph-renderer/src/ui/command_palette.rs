//! Ctrl+P command palette. Ports `archive/nuxt/components/CommandPalette.vue`.
//!
//! Two render modes:
//!   * search/list mode — fuzzy-matched action list with breadcrumb +
//!     category root; arrow-key navigation, Enter to descend or execute.
//!   * parameter form mode — driven by `ActionRegistry::configuring`,
//!     walks one parameter at a time with per-param validation.
//!
//! Outcome: an `Execute { action_id, params }` is bubbled up to the App,
//! which runs the actual handler (it owns AppState + GraphPipelines).

use std::collections::HashMap;

use eframe::egui;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use super::actions::{
    Action, ActionParameter, ActionRegistry, ParamValue, ParameterType,
};
use super::document_viewer::DocumentViewer;
use super::state::WorkspaceSettings;
use super::theme::{accent, palette};
use crate::proto::NodeMeta;

/// Maximum number of file/node matches surfaced under the action list.
const FILE_MATCH_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteState {
    pub open: bool,
    pub query: String,
    /// Stack of parent action ids ("settings" → "node-operations" etc.).
    pub breadcrumb: Vec<String>,
    pub selected_idx: usize,
    /// Set by the App after toggling open so we focus the input next frame.
    pub focus_input: bool,
    /// Set when a row is clicked mid-render so the outer return path can
    /// surface it after the ScrollArea closure returns.
    pub(crate) activated: Option<PaletteOutcome>,
    /// File/node id whose preview the palette would like fetched. The host
    /// (App) drains this every frame, kicks off a `/node/:id` fetch if not
    /// cached, and writes the result into `preview_cache`.
    pub pending_preview_id: Option<String>,
    /// Successful previews keyed by node id.
    pub preview_cache: HashMap<String, NodeMeta>,
    /// Failed-fetch ids → error message; lets us avoid re-fetching forever.
    pub preview_errors: HashMap<String, String>,
}

impl CommandPaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.breadcrumb.clear();
        self.selected_idx = 0;
        self.focus_input = true;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.breadcrumb.clear();
        self.selected_idx = 0;
    }
    pub fn toggle(&mut self) {
        if self.open { self.close() } else { self.open() }
    }
}

#[derive(Debug, Clone)]
pub enum PaletteOutcome {
    None,
    Execute {
        action_id: String,
        params: HashMap<String, ParamValue>,
    },
    /// User chose a fuzzy-matched vault file/node — host should open the
    /// node-detail modal for it (existing `/node/:id` flow).
    OpenNode {
        id: String,
    },
}

pub fn show(
    ctx: &egui::Context,
    state: &mut CommandPaletteState,
    registry: &mut ActionRegistry,
    workspace: &WorkspaceSettings,
    nodes: &[String],
) -> PaletteOutcome {
    if !state.open {
        return PaletteOutcome::None;
    }

    // Esc closes (above the modal Esc handler — palette wins if open).
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if registry.configuring.is_some() {
            registry.cancel_configuring();
        } else {
            state.close();
        }
        return PaletteOutcome::None;
    }

    let mut outcome = PaletteOutcome::None;

    // Wider window when we'll be showing the file-preview pane.
    let configuring = registry.configuring.is_some();
    let two_pane = !configuring && !state.query.trim().is_empty() && !nodes.is_empty();
    // Clamp to ~90% of the viewport so the palette doesn't overflow off
    // the screen on narrow windows. egui_dock + sidebars eat horizontal
    // space, and the prior 980px constant pushed the preview pane off
    // the right edge on typical laptop widths.
    let screen_w = ctx.screen_rect().width();
    let width = if two_pane {
        980.0_f32.min(screen_w * 0.9)
    } else {
        600.0_f32.min(screen_w * 0.9)
    };

    egui::Window::new("command-palette")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, ctx.screen_rect().height() * 0.18))
        .fixed_size(egui::vec2(width, 0.0))
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::from_rgb(0x10, 0x12, 0x16))
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .inner_margin(egui::Margin::same(0.0)),
        )
        .show(ctx, |ui| {
            if configuring {
                outcome = render_param_form(ui, registry, workspace);
            } else {
                outcome = render_search(ui, state, registry, workspace, nodes);
            }
        });

    outcome
}

// ----- search / list mode --------------------------------------------------

/// One ranked vault node + the byte indices in its id that matched.
#[derive(Debug, Clone)]
struct FileMatch {
    id: String,
    score: i64,
    indices: Vec<usize>,
}

fn rank_files(query: &str, nodes: &[String]) -> Vec<FileMatch> {
    if query.trim().is_empty() || nodes.is_empty() {
        return Vec::new();
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<FileMatch> = nodes
        .iter()
        .filter_map(|id| {
            matcher
                .fuzzy_indices(id, query)
                .map(|(score, indices)| FileMatch {
                    id: id.clone(),
                    score,
                    indices,
                })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score));
    scored.truncate(FILE_MATCH_LIMIT);
    scored
}

/// Synthesize a previewable "document" from a NodeMeta. We don't yet have
/// a file-body endpoint (`/node/:id` only returns metadata), so the
/// document viewer renders the metadata as YAML-ish text. When a body
/// endpoint lands later, swap this for the body string + extension-derived
/// language hint and the rest of the palette flow stays unchanged.
fn preview_text_for(meta: &NodeMeta) -> String {
    let mut s = String::new();
    s.push_str(&format!("id:        {}\n", meta.id));
    s.push_str(&format!("title:     {}\n", meta.title));
    s.push_str(&format!("path:      {}\n", meta.path));
    s.push_str(&format!("folder:    {}\n", meta.folder));
    if let Some(dt) = &meta.doctype {
        s.push_str(&format!("doctype:   {}\n", dt));
    }
    if !meta.tags.is_empty() {
        s.push_str(&format!("tags:      [{}]\n", meta.tags.join(", ")));
    }
    s.push_str("\n# metrics\n");
    s.push_str(&format!("degree:    {}\n", meta.degree));
    s.push_str(&format!("indegree:  {}\n", meta.indegree));
    s.push_str(&format!("outdegree: {}\n", meta.outdegree));
    s.push_str(&format!("pagerank:  {:.6}\n", meta.pagerank));
    s.push_str(&format!("kcore:     {}\n", meta.kcore));
    s.push_str(&format!("community: {}\n", meta.community));
    s.push_str(&format!("wcc:       {}\n", meta.wcc));
    if !meta.frontmatter_json.is_empty() && meta.frontmatter_json != "{}" {
        s.push_str("\n# frontmatter\n");
        // Pretty-print best-effort.
        match serde_json::from_str::<serde_json::Value>(&meta.frontmatter_json) {
            Ok(v) => match serde_json::to_string_pretty(&v) {
                Ok(pp) => s.push_str(&pp),
                Err(_) => s.push_str(&meta.frontmatter_json),
            },
            Err(_) => s.push_str(&meta.frontmatter_json),
        }
    }
    s
}

fn render_search(
    ui: &mut egui::Ui,
    state: &mut CommandPaletteState,
    registry: &mut ActionRegistry,
    workspace: &WorkspaceSettings,
    nodes: &[String],
) -> PaletteOutcome {
    // Breadcrumb.
    if !state.breadcrumb.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let mut pop_to: Option<usize> = None;
            for (idx, parent_id) in state.breadcrumb.clone().iter().enumerate() {
                let title = registry
                    .get(parent_id)
                    .map(|a| a.title.clone())
                    .unwrap_or_else(|| parent_id.clone());
                if ui.link(&title).clicked() {
                    pop_to = Some(idx);
                }
                if idx + 1 < state.breadcrumb.len() {
                    ui.label(" / ");
                }
            }
            if let Some(idx) = pop_to {
                state.breadcrumb.truncate(idx + 1);
                state.query.clear();
                state.selected_idx = 0;
            }
        });
        ui.separator();
    }

    // Search input. When the file-preview pane is in play, keep the input
    // pinned to the left half so the right half is reserved for the preview.
    let input_id = egui::Id::new("command-palette-input");
    let two_pane = !state.query.trim().is_empty() && !nodes.is_empty();
    let input_width = if two_pane {
        (ui.available_width() * 0.5) - 16.0
    } else {
        ui.available_width() - 16.0
    };
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let resp = ui.add_sized(
            [input_width, 28.0],
            egui::TextEdit::singleline(&mut state.query)
                .id(input_id)
                .hint_text("Type a command or fuzzy-search vault files…"),
        );
        if state.focus_input {
            resp.request_focus();
            state.focus_input = false;
        }
    });

    // Filter actions for the current scope.
    let scope_actions: Vec<Action> = current_scope(registry, &state.breadcrumb)
        .into_iter()
        .cloned()
        .collect();
    let ranked: Vec<(Action, MatchInfo)> = if state.query.trim().is_empty() {
        scope_actions
            .into_iter()
            .map(|a| (a, MatchInfo::default()))
            .collect()
    } else {
        let mut scored: Vec<(Action, MatchInfo)> = scope_actions
            .into_iter()
            .filter_map(|a| fuzzy_score(&a, &state.query).map(|m| (a, m)))
            .collect();
        scored.sort_by(|x, y| y.1.score.cmp(&x.1.score));
        scored
    };

    // File / node fuzzy matches (skim, FZF-style). Only on a non-empty
    // query and only when there's no breadcrumb (drilled-in scopes are
    // action-only).
    let file_matches: Vec<FileMatch> = if state.breadcrumb.is_empty() {
        rank_files(&state.query, nodes)
    } else {
        Vec::new()
    };
    let total_rows = ranked.len() + file_matches.len();

    // Keyboard navigation.
    let key_down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
    let key_up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
    let key_enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let key_tab = ui.input(|i| i.key_pressed(egui::Key::Tab));
    let key_backspace_empty =
        state.query.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Backspace));

    if total_rows > 0 {
        if key_down {
            state.selected_idx = (state.selected_idx + 1) % total_rows;
        }
        if key_up {
            state.selected_idx = (state.selected_idx + total_rows - 1) % total_rows;
        }
        if state.selected_idx >= total_rows {
            state.selected_idx = 0;
        }
    }
    if key_backspace_empty && !state.breadcrumb.is_empty() {
        state.breadcrumb.pop();
        state.selected_idx = 0;
    }

    // Tab completes query with selected title (actions) or file id.
    if key_tab && total_rows > 0 {
        if state.selected_idx < ranked.len() {
            state.query = ranked[state.selected_idx].0.title.clone();
        } else {
            state.query = file_matches[state.selected_idx - ranked.len()].id.clone();
        }
    }

    // Resolve which file id (if any) is selected for preview, and request a
    // fetch if its NodeMeta isn't cached yet.
    let selected_file_id: Option<String> = if state.selected_idx >= ranked.len()
        && !file_matches.is_empty()
    {
        let i = state.selected_idx - ranked.len();
        file_matches.get(i).map(|f| f.id.clone())
    } else {
        None
    };
    if let Some(id) = &selected_file_id {
        if !state.preview_cache.contains_key(id)
            && !state.preview_errors.contains_key(id)
            && state.pending_preview_id.as_deref() != Some(id.as_str())
        {
            state.pending_preview_id = Some(id.clone());
        }
    }

    // Group root entries by category when the user hasn't typed anything
    // and isn't drilled into a child scope (mirrors the Vue palette's
    // `category-section` block).
    let group_by_category = state.breadcrumb.is_empty() && state.query.trim().is_empty();

    // Render list (left) + preview (right) when there are file matches.
    ui.add_space(4.0);
    let max_h = ctx_screen_h(ui.ctx()) * 0.6;
    let total_w = ui.available_width();
    let two_pane = !file_matches.is_empty();
    let list_w = if two_pane { (total_w * 0.5).max(280.0) } else { total_w };

    ui.horizontal_top(|ui| {
        // ----- left: list -----
        ui.allocate_ui_with_layout(
            egui::vec2(list_w, max_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("command-palette-list")
                    .max_height(max_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if total_rows == 0 {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No matching actions or files")
                                        .color(egui::Color32::GRAY),
                                );
                            });
                            ui.add_space(16.0);
                            return;
                        }

                        let mut row_idx = 0usize;

                        // --- actions ---
                        if !ranked.is_empty() {
                            if group_by_category {
                                // Stable category order; uncategorised last.
                                let mut by_cat: Vec<(String, Vec<(Action, MatchInfo)>)> =
                                    Vec::new();
                                let mut uncategorised: Vec<(Action, MatchInfo)> = Vec::new();
                                for (a, mi) in &ranked {
                                    match &a.category {
                                        Some(c) => {
                                            if let Some(slot) =
                                                by_cat.iter_mut().find(|(name, _)| name == c)
                                            {
                                                slot.1.push((a.clone(), mi.clone()));
                                            } else {
                                                by_cat.push((
                                                    c.clone(),
                                                    vec![(a.clone(), mi.clone())],
                                                ));
                                            }
                                        }
                                        None => uncategorised.push((a.clone(), mi.clone())),
                                    }
                                }
                                for (cat, items) in &by_cat {
                                    category_header(ui, cat);
                                    for (a, mi) in items {
                                        render_action_row_mut(
                                            ui, &mut row_idx, a, mi, state, registry,
                                            workspace,
                                        );
                                    }
                                }
                                if !uncategorised.is_empty() {
                                    category_header(ui, "Other");
                                    for (a, mi) in &uncategorised {
                                        render_action_row_mut(
                                            ui, &mut row_idx, a, mi, state, registry,
                                            workspace,
                                        );
                                    }
                                }
                            } else {
                                for (a, mi) in &ranked {
                                    render_action_row_mut(
                                        ui, &mut row_idx, a, mi, state, registry, workspace,
                                    );
                                }
                            }
                        }

                        // --- file matches (FZF-style: actions first, then files) ---
                        if !file_matches.is_empty() {
                            if !ranked.is_empty() {
                                ui.add_space(2.0);
                                ui.separator();
                            }
                            category_header(ui, "Files / Nodes");
                            for fm in &file_matches {
                                let active = row_idx == state.selected_idx;
                                let resp = render_file_row(ui, fm, active);
                                if resp.hovered() {
                                    state.selected_idx = row_idx;
                                }
                                if resp.clicked() {
                                    state
                                        .activated
                                        .replace(PaletteOutcome::OpenNode { id: fm.id.clone() });
                                }
                                row_idx += 1;
                            }
                        }
                    });
            },
        );

        // ----- right: preview pane -----
        if two_pane {
            ui.separator();
            let preview_w = ui.available_width().max(200.0);
            ui.allocate_ui_with_layout(
                egui::vec2(preview_w, max_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    render_preview_pane(ui, state, &selected_file_id, max_h);
                },
            );
        }
    });

    if key_enter && total_rows > 0 {
        if state.selected_idx < ranked.len() {
            let action = ranked[state.selected_idx].0.clone();
            let outcome = enter_action(&action, state, registry, workspace);
            if !matches!(outcome, PaletteOutcome::None) {
                return outcome;
            }
        } else {
            let i = state.selected_idx - ranked.len();
            if let Some(fm) = file_matches.get(i) {
                state.close();
                return PaletteOutcome::OpenNode { id: fm.id.clone() };
            }
        }
    }

    state.activated.take().unwrap_or(PaletteOutcome::None)
}

fn render_action_row_mut(
    ui: &mut egui::Ui,
    row_idx: &mut usize,
    action: &Action,
    mi: &MatchInfo,
    state: &mut CommandPaletteState,
    registry: &mut ActionRegistry,
    workspace: &WorkspaceSettings,
) {
    let active = *row_idx == state.selected_idx;
    let resp = render_action_row(ui, action, mi, active);
    if resp.hovered() {
        state.selected_idx = *row_idx;
    }
    if resp.clicked() {
        let outcome = enter_action(action, state, registry, workspace);
        if !matches!(outcome, PaletteOutcome::None) {
            state.activated.replace(outcome);
        }
    }
    *row_idx += 1;
}

fn render_file_row(ui: &mut egui::Ui, fm: &FileMatch, active: bool) -> egui::Response {
    let bg = if active {
        egui::Color32::from_rgb(0x22, 0x28, 0x34)
    } else {
        egui::Color32::TRANSPARENT
    };
    let frame = egui::Frame::none()
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(12.0, 6.0));
    let resp = frame
        .show(ui, |ui| {
            // The filename component (last `/`) is the "primary" label,
            // the rest of the path is the secondary line.
            let (folder, name) = match fm.id.rsplit_once('/') {
                Some((dir, n)) => (dir, n),
                None => ("", fm.id.as_str()),
            };
            ui.vertical(|ui| {
                ui.label(highlighted_path(&fm.id, &fm.indices, name.len(), active));
                if !folder.is_empty() {
                    ui.label(
                        egui::RichText::new(folder)
                            .color(egui::Color32::from_gray(120))
                            .size(10.0),
                    );
                }
            });
        })
        .response;
    resp.interact(egui::Sense::click())
}

/// Render the full path with skim-match indices highlighted. The `_name_len`
/// hint isn't used yet but lets future work emphasise the filename portion.
fn highlighted_path(path: &str, hits: &[usize], _name_len: usize, focused: bool) -> egui::WidgetText {
    // Same focus-aware contrast rule as highlighted_title.
    let base = if focused { egui::Color32::WHITE } else { palette::TEXT };
    use egui::text::LayoutJob;
    let mut job = LayoutJob::default();
    let bytes = path.as_bytes();
    let mut i = 0usize;
    let in_hits = |idx: usize| hits.iter().any(|&h| h == idx);
    while i < bytes.len() {
        let start = i;
        let hit_now = in_hits(i);
        while i < bytes.len() && in_hits(i) == hit_now {
            i += 1;
        }
        let chunk = &path[start..i];
        let mut fmt = egui::TextFormat::default();
        fmt.font_id = egui::FontId::monospace(12.0);
        fmt.color = if hit_now {
            accent::YELLOW
        } else {
            base
        };
        if hit_now {
            fmt.background = egui::Color32::from_rgba_unmultiplied(0xff, 0xd5, 0x4a, 0x30);
        }
        job.append(chunk, 0.0, fmt);
    }
    egui::WidgetText::LayoutJob(job)
}

fn render_preview_pane(
    ui: &mut egui::Ui,
    state: &CommandPaletteState,
    selected_id: &Option<String>,
    max_h: f32,
) {
    let Some(id) = selected_id else {
        ui.add_space(8.0);
        ui.weak("(no file selected)");
        return;
    };
    if let Some(err) = state.preview_errors.get(id) {
        ui.add_space(8.0);
        ui.colored_label(accent::RED, format!("preview error: {err}"));
        return;
    }
    let Some(meta) = state.preview_cache.get(id) else {
        ui.add_space(8.0);
        ui.weak("loading…");
        return;
    };
    let body = preview_text_for(meta);
    // Match-position highlighting in the preview is reserved for when we
    // have file bodies; for the metadata fallback, the path on the left
    // already shows the fuzzy-match hits, so we pass an empty match list.
    DocumentViewer::new(&body)
        .language("yaml")
        .matches(&[])
        .max_height(max_h)
        .line_numbers(false)
        .wrap(false)
        .show(ui);
}

fn current_scope<'a>(reg: &'a ActionRegistry, breadcrumb: &[String]) -> Vec<&'a Action> {
    if let Some(parent) = breadcrumb.last() {
        reg.child_actions(parent)
    } else {
        reg.root_actions()
    }
}

fn enter_action(
    action: &Action,
    state: &mut CommandPaletteState,
    registry: &mut ActionRegistry,
    workspace: &WorkspaceSettings,
) -> PaletteOutcome {
    if !action.children_ids.is_empty() {
        // Drill down into children.
        state.breadcrumb.push(action.id.clone());
        state.query.clear();
        state.selected_idx = 0;
        state.focus_input = true;
        return PaletteOutcome::None;
    }
    if action.parameters.is_empty() {
        // Execute immediately.
        state.close();
        return PaletteOutcome::Execute {
            action_id: action.id.clone(),
            params: HashMap::new(),
        };
    }
    // Parameterized: enter form mode, smart-default from workspace for
    // settings actions.
    let initial = if action.parent_id.as_deref() == Some("settings") {
        workspace_initial_for(action, workspace)
    } else {
        HashMap::new()
    };
    registry.start_configuring(&action.id, &initial);
    PaletteOutcome::None
}

fn workspace_initial_for(
    action: &Action,
    workspace: &WorkspaceSettings,
) -> HashMap<String, ParamValue> {
    let mut m = HashMap::new();
    for p in &action.parameters {
        match p.id.as_str() {
            "font_size" => {
                m.insert(p.id.clone(), ParamValue::Number(workspace.font_size as f64));
            }
            "font_family" => {
                let s = match workspace.font_family {
                    super::state::FontFamilyChoice::Monospace => "monospace",
                    super::state::FontFamilyChoice::SansSerif => "sans-serif",
                    super::state::FontFamilyChoice::Serif => "serif",
                };
                m.insert(p.id.clone(), ParamValue::Selected(vec![s.into()]));
            }
            "show_line_numbers" => {
                m.insert(p.id.clone(), ParamValue::Boolean(workspace.show_line_numbers));
            }
            _ => {}
        }
    }
    m
}

// ----- parameter form mode -------------------------------------------------

fn render_param_form(
    ui: &mut egui::Ui,
    registry: &mut ActionRegistry,
    _workspace: &WorkspaceSettings,
) -> PaletteOutcome {
    let cfg_snapshot = registry.configuring.clone();
    let Some(cfg) = cfg_snapshot else {
        return PaletteOutcome::None;
    };
    let action = match registry.get(&cfg.action_id) {
        Some(a) => a.clone(),
        None => {
            registry.cancel_configuring();
            return PaletteOutcome::None;
        }
    };
    let n_params = action.parameters.len();
    let idx = cfg.current_param_index.min(n_params.saturating_sub(1));
    let is_last = idx + 1 == n_params;

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!("Configure {}", action.title))
                    .strong()
                    .size(13.0),
            );
            ui.label(
                egui::RichText::new(&action.description)
                    .color(egui::Color32::GRAY)
                    .size(10.0),
            );
            if n_params > 1 {
                ui.label(
                    egui::RichText::new(format!("Parameter {} of {}", idx + 1, n_params))
                        .color(egui::Color32::GRAY)
                        .size(10.0),
                );
            }
            ui.add_space(6.0);
        });
    });
    ui.separator();

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.vertical(|ui| {
            if let Some(param) = action.parameters.get(idx) {
                render_param_widget(ui, param, registry);
            }
        });
    });

    // Validate every frame for the live disabled-state of the apply button.
    let valid = registry.validate_param(idx);

    ui.add_space(8.0);
    // Right-aligned button row. Buttons appear in visual order
    // [Cancel | Previous | Next/Apply] thanks to the right_to_left layout
    // emitting in reverse.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if is_last {
            let apply_resp = ui.add_enabled(valid, egui::Button::new("Apply"));
            if apply_resp.clicked() {
                if let Some((id, params)) = registry.take_finished_form() {
                    return Some(PaletteOutcome::Execute { action_id: id, params });
                }
            }
        } else {
            let next_resp = ui.add_enabled(valid, egui::Button::new("Next"));
            if next_resp.clicked() {
                if let Some(c) = registry.configuring.as_mut() {
                    c.current_param_index = (idx + 1).min(n_params - 1);
                }
            }
            let prev_resp = ui.add_enabled(idx > 0, egui::Button::new("Previous"));
            if prev_resp.clicked() {
                if let Some(c) = registry.configuring.as_mut() {
                    c.current_param_index = idx.saturating_sub(1);
                }
            }
        }
        if ui.button("Cancel").clicked() {
            registry.cancel_configuring();
        }
        None::<PaletteOutcome>
    });

    // Enter on the last param applies; on earlier params advances.
    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        if valid {
            if is_last {
                if let Some((id, params)) = registry.take_finished_form() {
                    return PaletteOutcome::Execute { action_id: id, params };
                }
            } else if let Some(c) = registry.configuring.as_mut() {
                c.current_param_index = (idx + 1).min(n_params - 1);
            }
        }
    }

    PaletteOutcome::None
}

fn render_param_widget(
    ui: &mut egui::Ui,
    param: &ActionParameter,
    registry: &mut ActionRegistry,
) {
    ui.label(
        egui::RichText::new(if param.required {
            format!("{} *", param.name)
        } else {
            param.name.clone()
        })
        .strong(),
    );
    ui.label(
        egui::RichText::new(&param.description)
            .size(10.0)
            .color(egui::Color32::GRAY),
    );
    ui.add_space(4.0);

    let Some(cfg) = registry.configuring.as_mut() else { return };
    let value = cfg
        .form_values
        .entry(param.id.clone())
        .or_insert_with(|| ParamValue::default_for(param.kind));

    match param.kind {
        ParameterType::String => {
            if let ParamValue::String(s) = value {
                ui.add(
                    egui::TextEdit::singleline(s)
                        .desired_width(ui.available_width() - 24.0),
                );
            }
        }
        ParameterType::Number => {
            if let ParamValue::Number(n) = value {
                let mut dv = egui::DragValue::new(n).speed(0.1);
                if let Some(min) = param.validation.min {
                    if let Some(max) = param.validation.max {
                        dv = dv.range(min..=max);
                    } else {
                        dv = dv.range(min..=f64::INFINITY);
                    }
                } else if let Some(max) = param.validation.max {
                    dv = dv.range(f64::NEG_INFINITY..=max);
                }
                ui.add(dv);
            }
        }
        ParameterType::Boolean => {
            if let ParamValue::Boolean(b) = value {
                ui.checkbox(b, "Enable");
            }
        }
        ParameterType::Select => {
            if let ParamValue::Selected(items) = value {
                let current = items.first().cloned().unwrap_or_default();
                let label = param
                    .options
                    .iter()
                    .find(|o| o.value == current)
                    .map(|o| o.label.clone())
                    .unwrap_or_else(|| current.clone());
                egui::ComboBox::from_id_salt(format!("param-select-{}", param.id))
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for opt in &param.options {
                            let mut chosen = current.clone();
                            if ui.selectable_value(&mut chosen, opt.value.clone(), &opt.label).clicked() {
                                *items = vec![chosen];
                            }
                        }
                    });
            }
        }
        ParameterType::MultiSelect => {
            if let ParamValue::Selected(items) = value {
                for opt in &param.options {
                    let mut on = items.iter().any(|v| v == &opt.value);
                    if ui.checkbox(&mut on, &opt.label).changed() {
                        if on {
                            if !items.iter().any(|v| v == &opt.value) {
                                items.push(opt.value.clone());
                            }
                        } else {
                            items.retain(|v| v != &opt.value);
                        }
                    }
                }
                if param.options.is_empty() {
                    ui.label(
                        egui::RichText::new("(no options available)")
                            .italics()
                            .color(egui::Color32::GRAY),
                    );
                }
            }
        }
    }

    if let Some(err) = cfg.validation_errors.get(&param.id) {
        ui.label(
            egui::RichText::new(err)
                .color(accent::RED)
                .size(10.0),
        );
    }
}

// ----- fuzzy match ---------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct MatchInfo {
    score: i32,
    /// Byte indices in `title` that matched (used for highlighting).
    title_hits: Vec<usize>,
}

/// Case-insensitive subsequence match across title + description + keywords.
/// Returns `None` if every character in the query couldn't be placed.
/// Score rewards consecutive matches and earlier positions.
fn fuzzy_score(action: &Action, query: &str) -> Option<MatchInfo> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Some(MatchInfo::default());
    }
    // Title is the primary haystack; description + keywords broaden hits.
    let title_lc = action.title.to_lowercase();
    let mut title_hits: Vec<usize> = Vec::new();
    let title_score = subsequence_score(&title_lc, &q, Some(&mut title_hits));

    let extras = format!(
        "{} {} {}",
        action.description.to_lowercase(),
        action.keywords.join(" ").to_lowercase(),
        action.id.to_lowercase()
    );
    let extra_score = subsequence_score(&extras, &q, None);

    if title_score.is_none() && extra_score.is_none() {
        return None;
    }
    let title = title_score.unwrap_or(0);
    let extra = extra_score.unwrap_or(0);
    Some(MatchInfo {
        // Title matches weighted 3x — name typing should dominate.
        score: title * 3 + extra,
        title_hits,
    })
}

fn subsequence_score(haystack: &str, needle: &str, mut hits: Option<&mut Vec<usize>>) -> Option<i32> {
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut hi = 0;
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    let mut ni = 0;
    while ni < n_bytes.len() {
        let nb = n_bytes[ni];
        let mut found = None;
        while hi < h_bytes.len() {
            if h_bytes[hi] == nb {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let pos = found?;
        if let Some(h) = hits.as_deref_mut() {
            h.push(pos);
        }
        // Reward consecutive hits, prefer earlier matches.
        if Some(pos.wrapping_sub(1)) == last_match {
            score += 5;
        } else {
            score += 1;
        }
        if pos < 8 {
            score += 2;
        }
        last_match = Some(pos);
        ni += 1;
    }
    Some(score)
}

fn render_action_row(
    ui: &mut egui::Ui,
    action: &Action,
    mi: &MatchInfo,
    active: bool,
) -> egui::Response {
    let bg = if active {
        egui::Color32::from_rgb(0x22, 0x28, 0x34)
    } else {
        egui::Color32::TRANSPARENT
    };
    let frame = egui::Frame::none()
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(12.0, 8.0));
    let resp = frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    // Title with fuzzy-match highlight. Focused row keeps
                    // max-contrast WHITE; non-focused rows soften to
                    // TEXT (off-white).
                    let title_layout = highlighted_title(&action.title, &mi.title_hits, active);
                    ui.label(title_layout);
                    if !action.description.is_empty() {
                        ui.label(
                            egui::RichText::new(&action.description)
                                .color(egui::Color32::GRAY)
                                .size(10.0),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !action.children_ids.is_empty() {
                        ui.label("›");
                    }
                    ui.label(
                        egui::RichText::new(action.kind.label())
                            .size(9.0)
                            .color(egui::Color32::GRAY),
                    );
                    if let Some(sc) = &action.shortcut {
                        ui.label(
                            egui::RichText::new(sc)
                                .monospace()
                                .size(10.0)
                                .color(egui::Color32::from_gray(180)),
                        );
                    }
                });
            });
        })
        .response;
    resp.interact(egui::Sense::click())
}

fn highlighted_title(title: &str, hits: &[usize], focused: bool) -> egui::WidgetText {
    // Focused row keeps WHITE (max contrast on the focused row's tinted
    // bg). Non-focused rows pick the body TEXT colour so the list
    // doesn't read as a wall of LED-on-black.
    let base = if focused { egui::Color32::WHITE } else { palette::TEXT };
    if hits.is_empty() {
        return egui::RichText::new(title).color(base).into();
    }
    use egui::text::LayoutJob;
    let mut job = LayoutJob::default();
    let bytes = title.as_bytes();
    let mut i = 0;
    let in_hits = |idx: usize| hits.contains(&idx);
    while i < bytes.len() {
        let start = i;
        let hit_now = in_hits(i);
        while i < bytes.len() && in_hits(i) == hit_now {
            i += 1;
        }
        let chunk = &title[start..i];
        let mut fmt = egui::TextFormat::default();
        if hit_now {
            fmt.color = accent::BLUE;
            fmt.font_id = egui::FontId::proportional(12.0);
        } else {
            fmt.color = base;
            fmt.font_id = egui::FontId::proportional(12.0);
        }
        job.append(chunk, 0.0, fmt);
    }
    egui::WidgetText::LayoutJob(job)
}

fn ctx_screen_h(ctx: &egui::Context) -> f32 {
    ctx.screen_rect().height().max(200.0)
}

fn category_header(ui: &mut egui::Ui, label: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(label.to_uppercase())
                .size(9.0)
                .strong()
                .color(egui::Color32::from_gray(140)),
        );
    });
}
