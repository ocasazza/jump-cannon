//! Generate (tvix) section — author a graph as a Nix expression.
//!
//! The user types a Nix expression in a monospace [`egui::TextEdit`]. On
//! "Evaluate", `tvix_wasm::eval_graph` runs it (inline on the egui thread)
//! against the embedded graph library and yields a typed `GeneratedGraph`
//! (a projection of `toGraphJSON`'s `{ nodes, links }` shape).
//!
//! This panel owns no access to the renderer's `SharedLoad` / GPU
//! pipelines, so — like the Metrics and Layout-remote sections — it
//! communicates through one-shot `AppState` fields: it stashes the
//! evaluated graph in `state.generate.pending`, and `App::update` drains
//! it, converts via [`crate::generate::bootstrap_from_generated`], pushes
//! the `Bootstrap` into `LoadState::Ready`, and resets the GPU load latch
//! so the live graph is replaced.
//!
//! Errors are flattened to text and rendered as red labels (no Monaco /
//! `{line,col}` structured diagnostics — egui owns the UI).

use eframe::egui;

use super::super::state::{AppState, GENERATE_DEMO_EXPR};
use super::super::theme::accent;
use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Generate".into());

    hint_label(
        ui,
        "Write a Nix expression that evaluates to toGraphJSON's \
         { nodes = [...]; links = [...]; } shape. Evaluating replaces the \
         live graph.",
    );
    subgroup_separator(ui);

    // ---- Example picker --------------------------------------------------
    // Loads a built-in example into the editor (the user then presses
    // Evaluate). Stateless: no persisted selection — picking an entry just
    // overwrites the source. Every example is verified to evaluate by
    // `tvix_wasm`'s `all_demos_evaluate` test.
    subgroup_label(ui, "Examples");
    egui::ComboBox::from_id_salt("generate-demo-picker")
        .selected_text("Load an example…")
        .show_ui(ui, |ui| {
            for demo in tvix_wasm::demos() {
                if ui.selectable_label(false, demo.name).clicked() {
                    state.generate.source = demo.expr.to_string();
                    state.generate.error = None;
                    state.generate.last_counts = None;
                }
            }
        });

    subgroup_separator(ui);

    // ---- Expression editor ----------------------------------------------
    subgroup_label(ui, "Nix expression");
    ui.add(
        egui::TextEdit::multiline(&mut state.generate.source)
            .font(egui::TextStyle::Monospace)
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(14)
            .hint_text("import /jc/src/graph.nix {} ..."),
    );

    // ---- Actions ---------------------------------------------------------
    ui.horizontal(|ui| {
        let has_src = !state.generate.source.trim().is_empty();
        if ui
            .add_enabled(has_src, egui::Button::new("Evaluate"))
            .on_hover_text("Evaluate the expression and replace the live graph")
            .clicked()
        {
            match tvix_wasm::eval_graph(&state.generate.source) {
                Ok(graph) => {
                    state.generate.error = None;
                    state.generate.last_counts = Some((graph.nodes.len(), graph.edges.len()));
                    // Hand the graph to App::update for promotion to the GPU.
                    state.generate.pending = Some(graph);
                }
                Err(err) => {
                    state.generate.error = Some(err);
                    state.generate.last_counts = None;
                    state.generate.pending = None;
                }
            }
        }
        if ui
            .button("Reset to demo")
            .on_hover_text("Restore the prefilled star-graph example")
            .clicked()
        {
            state.generate.source = GENERATE_DEMO_EXPR.to_string();
            state.generate.error = None;
            state.generate.last_counts = None;
        }
    });

    subgroup_separator(ui);

    // ---- Count readout ---------------------------------------------------
    match state.generate.last_counts {
        Some((nodes, edges)) => {
            ui.label(format!("{nodes} nodes, {edges} edges"));
        }
        None => {
            hint_label(ui, "no graph generated yet");
        }
    }

    // ---- Error area ------------------------------------------------------
    if let Some(err) = &state.generate.error {
        subgroup_separator(ui);
        subgroup_label(ui, "Evaluation error");
        for line in err.lines() {
            ui.colored_label(accent::RED, line);
        }
    }
}
