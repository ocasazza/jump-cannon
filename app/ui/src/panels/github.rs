//! GitHub vault import panel — fully in-browser, works on GitHub Pages.
//!
//! Fetches a public GitHub repository's markdown corpus over CORS-enabled
//! endpoints, extracts wikilink edges with vault-links, and promotes the
//! resulting graph onto the canvas. No graph-api server is required.
//!
//! Panel-local state lives in `GlobalSignal`s (same pattern as generate.rs) so
//! the file is self-contained. Spec persistence and the last-imported record
//! live in `crate::github` (shared with the Settings → Importers catalog).

use dioxus::prelude::*;

use crate::github::{self, GitHubImportSpec};
use crate::graph_canvas;
use crate::Ctx;

// ── panel-local transient state (GlobalSignals, same pattern as generate.rs) ───
static STATUS: GlobalSignal<Option<String>> = Signal::global(|| None);
static ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
static IMPORTING: GlobalSignal<bool> = Signal::global(|| false);

// ── panel ----------------------------------------------------------------────

/// GitHub import panel.
///
/// Three inputs (repo, ref, path), an Import button, progress/status line,
/// and error surface. On success the resulting graph is promoted onto the
/// canvas via [`crate::replace_with_client_graph`].
pub(crate) fn panel(ctx: Ctx) -> Element {
    let persisted = github::persisted_spec().unwrap_or_default();
    let mut repo = use_signal(|| persisted.repo.clone());
    let mut git_ref = use_signal(|| persisted.git_ref.clone());
    let mut path = use_signal(|| persisted.path.clone());

    rsx! {
        div { class: "gh-panel",
            div { class: "gh-label", "Repository" }
            input {
                class: "gh-input",
                r#type: "text",
                placeholder: "owner/repo",
                value: "{repo}",
                oninput: move |e| repo.set(e.value()),
                "aria-label": "Repository (owner/repo)",
            }
            div { class: "gh-hint", "e.g. ocasazza/jump-cannon" }

            div { class: "gh-label", "Ref" }
            input {
                class: "gh-input",
                r#type: "text",
                placeholder: "branch / tag / SHA (leave empty for default)",
                value: "{git_ref}",
                oninput: move |e| git_ref.set(e.value()),
                "aria-label": "Git reference",
            }

            div { class: "gh-label", "Path" }
            input {
                class: "gh-input",
                r#type: "text",
                placeholder: "subdirectory (leave empty for repo root)",
                value: "{path}",
                oninput: move |e| path.set(e.value()),
                "aria-label": "Subdirectory path",
            }
            div { class: "gh-hint", "e.g. charts/jump-cannon/knowledge" }

            hr { class: "gh-sep" }

            div { class: "gh-actions",
                button {
                    class: "btn gh-import-btn",
                    disabled: "*IMPORTING.read()",
                    onclick: move |_| {
                        let spec = GitHubImportSpec {
                            repo: repo.peek().clone(),
                            git_ref: git_ref.peek().clone(),
                            path: path.peek().clone(),
                        };
                        spawn_import(spec, ctx);
                    },
                    if *IMPORTING.read() {
                        "Importing…"
                    } else {
                        "Import"
                    }
                }
            }

            if let Some(s) = STATUS.read().as_ref() {
                div { class: "gh-status", "{s}" }
            }
            if let Some(e) = ERROR.read().as_ref() {
                div { class: "gh-error", "{e}" }
            }

            hr { class: "gh-sep" }
            p { class: "gh-hint",
                "Search and metadata panels are limited for browser-only graphs — \
                 node metadata fetching requires a graph-api server. \
                 The graph is fully navigable on the canvas."
            }
        }
    }
}

/// Run one import with live panel status (`STATUS`/`ERROR`/`IMPORTING`).
/// Shared by the panel's Import button and the `?gh=`/github.io boot hook in
/// main.rs so a boot import reports progress in the open panel the same way.
pub(crate) fn spawn_import(spec: GitHubImportSpec, ctx: Ctx) {
    *IMPORTING.write() = true;
    *ERROR.write() = None;
    *STATUS.write() = Some("Listing files…".into());

    spawn(async move {
        // Validate first.
        if let Err(e) = spec.validate() {
            *ERROR.write() = Some(e);
            *IMPORTING.write() = false;
            return;
        }

        match github::import(&spec).await {
            Ok(load_result) => {
                let n = load_result.graph.node_count();
                let m = load_result.graph.edge_count();
                *STATUS.write() = Some(format!("Imported {n} notes, {m} edges"));

                // Persist the spec as the panel default and record it as
                // the live source (the Settings → Importers browser catalog
                // reads `LAST_IMPORT`).
                github::persist_spec(&spec);
                *github::LAST_IMPORT.write() = Some(spec.clone());

                // Convert to GraphData and promote onto the canvas.
                let graph_data = graph_canvas::graph_data_from_vault(&load_result.graph);
                crate::replace_with_client_graph(
                    ctx,
                    graph_data,
                    format!("github:{}", spec.repo),
                );

                *IMPORTING.write() = false;
            }
            Err(e) => {
                *ERROR.write() = Some(e);
                *STATUS.write() = None;
                *IMPORTING.write() = false;
            }
        }
    });
}