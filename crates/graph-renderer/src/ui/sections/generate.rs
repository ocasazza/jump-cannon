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

use super::super::examples;
use super::super::nix_extension::{NixExample, NixExtension};
use super::super::state::AppState;
use super::subgroup_separator;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Generate".into());

    // ── Example UI-states (self-assembly demos) ──────────────────────────
    // A one-click loader for the full Brownian self-assembly demos: each entry
    // builds a complete AppState (validated bonding regime + Geometric (GPU) +
    // soup generator + matching seed + camera/style) and REPLACES the live
    // state, mirroring the share-link / YAML import path.
    examples_picker(ui, state);
    subgroup_separator(ui);

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

/// Examples picker: a combo of the self-assembly example UI-states. Selecting
/// one builds its full `AppState` and replaces the live state (preserving the
/// in-memory snapshot ring), then stamps a labelled timeline entry — exactly the
/// share-link / YAML load contract.
fn examples_picker(ui: &mut egui::Ui, state: &mut AppState) {
    super::subgroup_label(ui, "Examples (self-assembly)");
    super::hint_label(
        ui,
        "Load a full demo state: Geometric (GPU) + validated bonding regime + \
         soup generator + seed. After loading, press Evaluate to spawn the soup.",
    );

    let mut to_load: Option<&'static examples::Example> = None;
    egui::ComboBox::from_id_salt("examples-picker")
        .selected_text("Load an example…")
        .width(f32::INFINITY)
        .show_ui(ui, |ui| {
            for ex in examples::catalog() {
                if ui
                    .selectable_label(false, ex.name)
                    .on_hover_text(ex.description)
                    .clicked()
                {
                    to_load = Some(ex);
                }
            }
        });

    if let Some(ex) = to_load {
        let imported = ex.build_state();
        // Preserve the in-memory snapshot ring across the swap (it is
        // `#[serde(skip)]` and would otherwise be wiped), then stamp the load.
        let ring = std::mem::take(&mut state.snapshots);
        *state = imported;
        state.snapshots = ring;
        state.snapshot_now(format!("example: {}", ex.name));
    }
}
