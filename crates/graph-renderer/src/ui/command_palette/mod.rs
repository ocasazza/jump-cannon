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

mod matching;

use std::collections::HashMap;

use eframe::egui;

use super::actions::{
    Action, ActionParameter, ActionRegistry, ParamValue, ParameterType,
};
use super::document_viewer::DocumentViewer;
use super::state::WorkspaceSettings;
use super::squircle;
use super::theme::{self, accent, palette};
use crate::proto::NodeMeta;

use matching::{
    fuzzy_score, highlighted_path, highlighted_title, preview_text_for, rank_files,
    FileMatch, MatchInfo,
};

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

    let frame = theme::floating_frame()
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE)
        .inner_margin(egui::Margin::same(0.0));
    egui::Window::new("command-palette")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, ctx.screen_rect().height() * 0.18))
        .fixed_size(egui::vec2(width, 0.0))
        .frame(frame)
        .show(ctx, |ui| {
            // Squircle backdrop on the Background layer beneath the
            // palette body (mirrors `ui/floating.rs`). Painted first so
            // every widget below renders on top.
            let rect = ui.max_rect().expand(8.0);
            let mut painter = ui.painter().clone();
            painter.set_layer_id(egui::LayerId::new(
                egui::Order::Background,
                ui.layer_id().id,
            ));
            squircle::paint_squircle(
                &painter,
                rect,
                12.0,
                theme::FLOATING_BACKDROP,
                egui::Stroke::new(1.0, palette::BORDER),
            );
            if configuring {
                outcome = render_param_form(ui, registry, workspace);
            } else {
                outcome = render_search(ui, state, registry, workspace, nodes);
            }
        });

    outcome
}

// ----- search / list mode --------------------------------------------------

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

fn ctx_screen_h(ctx: &egui::Context) -> f32 {
    ctx.screen_rect().height().max(200.0)
}

fn category_header(ui: &mut egui::Ui, label: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(label)
                .size(crate::ui::theme::font_size::SMALL)
                .strong()
                .color(egui::Color32::from_gray(140)),
        );
    });
}
