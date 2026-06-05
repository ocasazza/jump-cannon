//! Generate (tvix) section — author a graph as a Nix expression.
//!
//! The user types a Nix expression in a monospace editor. On "Evaluate",
//! `tvix_wasm::eval_graph` runs it (inline on the egui thread) against the
//! embedded graph library and yields a typed `GeneratedGraph` (a projection of
//! `toGraphJSON`'s `{ nodes, links }` shape).
//!
//! The editor / examples-picker / error chrome is the shared
//! [`crate::ui::nix_extension::NixExtension`] component — this section just
//! supplies the prefilled examples and the `eval_graph` callback, then handles
//! the typed result.
//!
//! This panel owns no access to the renderer's `SharedLoad` / GPU pipelines, so
//! it communicates through one-shot `AppState` fields: it stashes the evaluated
//! graph in `state.generate.pending`, and `App::update` drains it, converts via
//! [`crate::generate::bootstrap_from_generated`], pushes the `Bootstrap` into
//! `LoadState::Ready`, and resets the GPU load latch so the live graph is
//! replaced.

use eframe::egui;

use super::super::nix_extension::{NixExample, NixExtension};
use super::super::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Generate".into());

    // Built-in examples come straight from the embedded demo catalog. Every
    // entry is verified to evaluate by `tvix_wasm`'s `all_demos_evaluate` test.
    let examples: Vec<NixExample> = tvix_wasm::demos().iter().copied().map(Into::into).collect();

    let component = NixExtension {
        id_salt: "generate",
        hint: "Write a Nix expression that evaluates to toGraphJSON's \
               { nodes = [...]; links = [...]; } shape. Evaluating replaces \
               the live graph.",
        editor_hint: "import /jc/src/graph.nix {} ...",
        action_label: "Evaluate",
        action_tooltip: "Evaluate the expression and replace the live graph",
        examples: &examples,
        rows: 14,
    };

    if let Some(graph) =
        component.show(ui, &mut state.generate.editor, |src| tvix_wasm::eval_graph(src))
    {
        // Success: record a count readout and hand the graph to App::update for
        // promotion to the GPU.
        state.generate.editor.status =
            Some(format!("{} nodes, {} edges", graph.nodes.len(), graph.edges.len()));
        state.generate.pending = Some(graph);
    }
}
