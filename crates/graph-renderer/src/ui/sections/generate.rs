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

    // ── Execution backend picker ─────────────────────────────────────────
    // Routes WHERE the (potentially long) eval runs. Auto = Server when
    // graph-api is reachable, else a local fallback — the non-freeze default
    // on WASM (server-side eval over async HTTP). Mirrors the layout-engine
    // local-vs-remote picker.
    backend_picker(ui, state);
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

    // The evaluation no longer runs inline on this click-handler — that froze
    // the tab for a large graph. Instead the button records a one-shot
    // `request` (the source to evaluate); `App::update` spawns a
    // `crate::job::BackgroundJob` to run `tvix_wasm::eval_graph` off the
    // click-handler (native: a real thread; WASM: paint-first-then-run, with
    // coarse progress in the footer + debug console). The component's
    // `evaluate` callback therefore just hands back the source string and
    // never errors here — real eval errors arrive later via the job and are
    // written back into `editor.error` by `App`.
    let already_running = state.generate.request.is_some();
    if let Some(src) = component.show(ui, &mut state.generate.editor, |src| {
        Ok::<String, String>(src.to_string())
    }) {
        if !already_running {
            state.generate.editor.status = Some("queued…".into());
            state.generate.request = Some(src);
        }
    }
}

/// Execution-backend picker: a combo over [`GenerateBackendChoice`]. Selecting
/// a backend routes where the eval runs (Server = async HTTP to graph-api, the
/// WASM non-freeze path; Inline = local; Auto = reachability-based default).
fn backend_picker(ui: &mut egui::Ui, state: &mut AppState) {
    use super::super::state::GenerateBackendChoice as Choice;
    super::subgroup_label(ui, "Execution backend");
    super::hint_label(
        ui,
        "Where the expression is evaluated. Server (async HTTP to graph-api) \
         keeps the browser responsive for large graphs. Auto uses Server when \
         reachable, else a local fallback.",
    );

    let label = |c: Choice| match c {
        Choice::Auto => "Auto (server if reachable)",
        Choice::Server => "Server (graph-api)",
        Choice::Inline => "Inline (local)",
        Choice::LocalWorker => "Local worker",
    };

    let cur = state.generate.backend;
    egui::ComboBox::from_id_salt("generate-backend-picker")
        .selected_text(label(cur))
        .width(f32::INFINITY)
        .show_ui(ui, |ui| {
            for choice in [Choice::Auto, Choice::Server, Choice::Inline, Choice::LocalWorker] {
                let resp = ui.selectable_value(&mut state.generate.backend, choice, label(choice));
                if choice == Choice::LocalWorker {
                    resp.on_hover_text(
                        "Offline Web Worker eval. Currently falls back to the local \
                         executor — the trunk worker bundle is feasibility-gated \
                         (see tvix-worker).",
                    );
                }
            }
        });
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
