//! "Initial seed" section — choose where the graph's nodes START before the
//! force sim takes over.
//!
//! This is the first reuse of the Nix-extension mechanism beyond the Generate
//! panel: a user can author a brand-new seeding strategy as a Nix expression
//! implementing the abstract seed interface (`seed : { n, ... } -> [{x;y;z;}]`,
//! documented in `tvix-wasm`'s embedded `seed.nix`) and apply it to the live
//! graph — no Rust change required.
//!
//! The picker offers three kinds of strategy:
//!   * **No seed** — apply nothing; leave the current positions untouched.
//!   * the **built-in** strategies (sphere / random / grid), which are
//!     themselves Nix expressions from `tvix_wasm::seed_demos()`.
//!   * **Custom (Nix)** — the shared [`NixExtension`] editor; the user writes a
//!     seed expression and presses Apply.
//!
//! On Apply the chosen expression is run through `tvix_wasm::eval_seed(expr, n)`
//! (where `n` is the live node count from `state.stats`). The resulting
//! positions are stashed in `state.seed.pending`; `App::update` drains them and
//! writes them straight into the live GPU positions buffer (the same buffer the
//! bootstrap / static-solve paths feed). "No seed" simply never sets `pending`.

use eframe::egui;

use super::super::nix_extension::{NixEditorState, NixExtension};
use super::super::state::{AppState, SeedStrategy};
use super::{hint_label, row, subgroup_label, subgroup_separator};

/// Indices into `tvix_wasm::seed_demos()` that are exposed as first-class
/// "built-in" strategies in the picker. The catalog also ships a "No seed" and
/// a "Custom (flat line)" entry, but those are surfaced via the dedicated
/// `No seed` and `Custom (Nix)` picker options instead, so they are excluded
/// here by name.
fn builtin_demo_indices() -> Vec<usize> {
    tvix_wasm::seed_demos()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.name != "No seed" && !d.name.starts_with("Custom"))
        .map(|(i, _)| i)
        .collect()
}

fn strategy_label(state: &AppState) -> String {
    match &state.seed.strategy {
        SeedStrategy::None => "No seed".to_string(),
        SeedStrategy::Custom => "Custom (Nix)".to_string(),
        SeedStrategy::BuiltIn(i) => tvix_wasm::seed_demos()
            .get(*i)
            .map(|d| d.name.to_string())
            .unwrap_or_else(|| "(unknown)".to_string()),
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    subgroup_label(ui, "Initial seed");
    hint_label(
        ui,
        "Place nodes before the sim runs. Pick a built-in, write a custom Nix \
         seed, or apply none.",
    );
    ui.add_space(4.0);

    let demo_indices = builtin_demo_indices();

    // ---- Strategy picker -------------------------------------------------
    let label = strategy_label(state);
    row(ui, "seed", |ui| {
        egui::ComboBox::from_id_salt("initial-seed-strategy")
            .selected_text(label)
            .show_ui(ui, |ui| {
                // No seed (first-class option).
                if ui
                    .selectable_label(
                        matches!(state.seed.strategy, SeedStrategy::None),
                        "No seed",
                    )
                    .on_hover_text("Leave the current positions untouched")
                    .clicked()
                {
                    state.seed.strategy = SeedStrategy::None;
                    state.seed.editor.error = None;
                    state.seed.editor.status = None;
                    // "No seed" = keep the current buffer: flip the active
                    // gpu-force layout into SeedMode::None so the re-init the
                    // settings change provokes doesn't re-roll a random ball.
                    set_gpu_force_seed_mode(state, true);
                }
                // Built-in Nix seeds.
                for &i in &demo_indices {
                    let d = &tvix_wasm::seed_demos()[i];
                    let selected = state.seed.strategy == SeedStrategy::BuiltIn(i);
                    if ui.selectable_label(selected, d.name).clicked() {
                        state.seed.strategy = SeedStrategy::BuiltIn(i);
                        state.seed.editor.error = None;
                        state.seed.editor.status = None;
                    }
                }
                // Custom (Nix).
                if ui
                    .selectable_label(
                        matches!(state.seed.strategy, SeedStrategy::Custom),
                        "Custom (Nix)",
                    )
                    .on_hover_text("Author a seed as a Nix expression")
                    .clicked()
                {
                    state.seed.strategy = SeedStrategy::Custom;
                    state.seed.editor.error = None;
                    state.seed.editor.status = None;
                }
            });
    });

    let n = state.stats.n_nodes as usize;

    match state.seed.strategy.clone() {
        // ── No seed ──────────────────────────────────────────────────────
        SeedStrategy::None => {
            hint_label(ui, "No seed will be applied — positions stay as-is.");
        }

        // ── A built-in Nix seed ─────────────────────────────────────────
        SeedStrategy::BuiltIn(i) => {
            let expr = tvix_wasm::seed_demos()
                .get(i)
                .map(|d| d.expr.to_string())
                .unwrap_or_default();
            apply_button(ui, state, &expr, n);
        }

        // ── Custom (Nix) — the reusable component ────────────────────────
        SeedStrategy::Custom => {
            subgroup_separator(ui);
            let component = NixExtension {
                id_salt: "seed",
                hint: "Implement seed : { n, ... } -> [ { x; y; z; } ]. `n` is \
                       bound to the live node count. Return exactly n positions \
                       (or [] for no seed).",
                editor_hint: "import /jc/src/seed.nix {} ...",
                action_label: "Apply seed",
                action_tooltip: "Evaluate the seed expression and place the nodes",
                examples: &[],
                rows: 12,
            };
            // The closure borrows `n`; the editor is borrowed mutably by the
            // component, so capture `n` by copy.
            if let Some(positions) =
                component.show(ui, &mut state.seed.editor, |src| tvix_wasm::eval_seed(src, n))
            {
                let applied = !positions.is_empty();
                set_pending(&mut state.seed.editor, &mut state.seed.pending, positions);
                if applied {
                    // The applied positions are now meaningful — keep them
                    // through the sim re-init (SeedMode::None).
                    set_gpu_force_seed_mode(state, true);
                }
            }
        }
    }
}

/// Render an "Apply seed" button for a fixed (built-in) expression, evaluating
/// `expr` against `n` on click and stashing the result in `state.seed.pending`.
fn apply_button(ui: &mut egui::Ui, state: &mut AppState, expr: &str, n: usize) {
    ui.add_space(4.0);
    if ui
        .button("Apply seed")
        .on_hover_text("Evaluate the seed and place the nodes")
        .clicked()
    {
        match tvix_wasm::eval_seed(expr, n) {
            Ok(positions) => {
                state.seed.editor.error = None;
                let applied = !positions.is_empty();
                set_pending(&mut state.seed.editor, &mut state.seed.pending, positions);
                if applied {
                    // Keep the just-applied positions through the sim re-init.
                    set_gpu_force_seed_mode(state, true);
                }
            }
            Err(err) => {
                state.seed.editor.error = Some(err);
            }
        }
    }
    if let Some(status) = &state.seed.editor.status {
        ui.label(status.as_str());
    }
    if let Some(err) = &state.seed.editor.error {
        for line in err.lines() {
            ui.colored_label(super::super::theme::accent::RED, line);
        }
    }
}

/// Write the gpu-force layout's `seed_mode` so the running force sim treats the
/// current/applied positions correctly:
///
///   * `keep == true` ("No seed", or right after an explicit seed Apply) sets
///     `seed_mode = "none"`, which makes `init_with_device` *skip* the buffer
///     upload — so the positions already in the buffer (the bootstrap sphere, a
///     just-applied seed, or a settled sim) survive the re-init that the
///     seed-mode change triggers via `apply_layout_to_gpu`.
///   * `keep == false` restores `seed_mode = "random"` (the historical default)
///     so a future *fresh* load still spreads a degenerate vault.
///
/// Only the `gpu-force` settings block is touched, and only its `seed_mode`
/// key — every other tuned field is left intact. If the block doesn't exist
/// yet, `apply_layout_to_gpu` will lazy-init it; we still record the intent so
/// the value sticks once it does.
fn set_gpu_force_seed_mode(state: &mut AppState, keep: bool) {
    let mode = if keep { "none" } else { "random" };
    // Seed a freshly-created block with size-tuned defaults so we don't strand
    // the layout on the dense-ball anchor defaults — mirrors the lazy-init in
    // `App::apply_layout_to_gpu`. (In practice the block already exists by the
    // time this section is usable, but be defensive.)
    let n = state.stats.n_nodes as usize;
    let entry = state.layout.settings.entry("gpu-force".to_string()).or_insert_with(|| {
        serde_json::to_value(graph_layouts::GpuForceOptions::for_n_nodes(n))
            .unwrap_or_else(|_| serde_json::json!({}))
    });
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("seed_mode".to_string(), serde_json::json!(mode));
    }
}

/// Common success handling: an empty result is the "no seed" sentinel (skip),
/// otherwise stash the positions and set a status line.
fn set_pending(
    editor: &mut NixEditorState,
    pending: &mut Option<Vec<[f32; 3]>>,
    positions: Vec<[f32; 3]>,
) {
    if positions.is_empty() {
        editor.status = Some("no seed applied (empty result)".to_string());
        *pending = None;
    } else {
        editor.status = Some(format!("applied {} positions", positions.len()));
        *pending = Some(positions);
    }
}
