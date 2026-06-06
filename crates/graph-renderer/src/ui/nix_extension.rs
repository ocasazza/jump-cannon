//! Reusable Nix-extension egui component.
//!
//! The "Generate (tvix)" panel pioneered a small UI shape: a monospace Nix
//! editor + an Evaluate button + an examples/preset picker + an inline error
//! area. That shape is the user-facing half of the tvix-as-extension-mechanism
//! vision (see `docs/tvix-graph-generation-and-plugins.md`): any pipeline stage
//! that wants a user-authored Nix hook can drop this widget in.
//!
//! This module factors that machinery out so other sections (e.g. the Layout
//! panel's "Initial seed") can reuse it. It is generic over the evaluated
//! result type `T` (a graph, a list of seed positions, …) via a typed
//! `evaluate` callback supplied by the caller, so the component owns the
//! editor/picker/error chrome while the caller owns what "evaluate" means and
//! what to do with the result.
//!
//! State the caller must own (so it can persist or `#[serde(skip)]` as it
//! likes) lives in [`NixEditorState`]: the source buffer, the last error, and
//! a free-form status line. The component mutates these in place.

use eframe::egui;

use super::sections::{hint_label, subgroup_label, subgroup_separator};
use super::theme::accent;

/// Caller-owned, persistable state for one embedded Nix editor.
///
/// Kept deliberately small and `Default`-able so a section can hang it off its
/// own state struct (persisted or `#[serde(skip)]`). The component never holds
/// state of its own across frames.
///
/// `source` is a genuine user PARAMETER (the authored Nix expression) and so
/// `Serialize`/`Deserialize`: it must round-trip through an exported state /
/// share link so an agent can hand a generator/seed expression in headlessly.
/// `error` / `status` are transient one-shot display scratch — `#[serde(skip)]`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct NixEditorState {
    /// The editable Nix expression. User-facing parameter — round-trips.
    #[serde(default)]
    pub source: String,
    /// Most recent evaluation error, rendered as red labels. `None` on success
    /// (or before the first evaluation). Transient — never persisted.
    #[serde(skip)]
    pub error: Option<String>,
    /// A short success/status line (e.g. "12 nodes, 11 edges" or "applied 200
    /// positions"). `None` until the caller sets one. The component renders it
    /// dimmed when present. Transient — never persisted.
    #[serde(skip)]
    pub status: Option<String>,
}

impl NixEditorState {
    /// Construct with a prefilled source expression.
    pub fn with_source(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            ..Default::default()
        }
    }
}

/// A named example expression for the picker. Mirrors `tvix_wasm::Demo` but is
/// owned by the UI so callers can pass either the embedded catalog or their own
/// inline list.
#[derive(Clone, Copy, Debug)]
pub struct NixExample {
    pub name: &'static str,
    pub expr: &'static str,
}

impl From<tvix_wasm::Demo> for NixExample {
    fn from(d: tvix_wasm::Demo) -> Self {
        NixExample {
            name: d.name,
            expr: d.expr,
        }
    }
}

/// Configuration for one rendering of the component. Cheap to build per frame.
pub struct NixExtension<'a> {
    /// Salt for the example ComboBox + editor ids (must be unique per section).
    pub id_salt: &'a str,
    /// One-line description shown above the editor.
    pub hint: &'a str,
    /// Placeholder shown in an empty editor.
    pub editor_hint: &'a str,
    /// Label on the primary action button.
    pub action_label: &'a str,
    /// Hover tooltip for the action button.
    pub action_tooltip: &'a str,
    /// Examples offered by the picker. Selecting one overwrites the source.
    pub examples: &'a [NixExample],
    /// Number of visible rows in the editor.
    pub rows: usize,
}

impl<'a> NixExtension<'a> {
    /// Render the editor + picker + action button + error/status area.
    ///
    /// `evaluate` is the typed callback invoked when the action button is
    /// clicked; it receives the current source and returns either a typed
    /// result `T` (success) or an error string (shown inline). On success the
    /// component clears the error and hands the caller the result via the return
    /// value so the caller can do whatever the result means (push a graph, apply
    /// seed positions, …). The caller is responsible for setting `state.status`.
    ///
    /// Returns `Some(result)` exactly on the frame the user clicked Evaluate and
    /// evaluation succeeded; `None` otherwise.
    pub fn show<T>(
        &self,
        ui: &mut egui::Ui,
        state: &mut NixEditorState,
        evaluate: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<T> {
        hint_label(ui, self.hint);
        subgroup_separator(ui);

        // ---- Example picker ---------------------------------------------
        // Loads a built-in example into the editor (the user then presses the
        // action button). Stateless: picking an entry just overwrites the
        // source buffer.
        if !self.examples.is_empty() {
            subgroup_label(ui, "Examples");
            egui::ComboBox::from_id_salt(format!("{}-example-picker", self.id_salt))
                .selected_text("Load an example…")
                .show_ui(ui, |ui| {
                    for ex in self.examples {
                        if ui.selectable_label(false, ex.name).clicked() {
                            state.source = ex.expr.to_string();
                            state.error = None;
                            state.status = None;
                        }
                    }
                });
            subgroup_separator(ui);
        }

        // ---- Expression editor ------------------------------------------
        subgroup_label(ui, "Nix expression");
        ui.add(
            egui::TextEdit::multiline(&mut state.source)
                .id_salt(format!("{}-editor", self.id_salt))
                .font(egui::TextStyle::Monospace)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(self.rows)
                .hint_text(self.editor_hint),
        );

        // ---- Action -----------------------------------------------------
        let mut result = None;
        ui.horizontal(|ui| {
            let has_src = !state.source.trim().is_empty();
            if ui
                .add_enabled(has_src, egui::Button::new(self.action_label))
                .on_hover_text(self.action_tooltip)
                .clicked()
            {
                match evaluate(&state.source) {
                    Ok(value) => {
                        state.error = None;
                        result = Some(value);
                    }
                    Err(err) => {
                        state.error = Some(err);
                        state.status = None;
                    }
                }
            }
        });

        // ---- Status / count readout -------------------------------------
        if let Some(status) = &state.status {
            ui.label(status.as_str());
        }

        // ---- Error area -------------------------------------------------
        if let Some(err) = &state.error {
            subgroup_separator(ui);
            subgroup_label(ui, "Evaluation error");
            for line in err.lines() {
                ui.colored_label(accent::RED, line);
            }
        }

        result
    }
}
