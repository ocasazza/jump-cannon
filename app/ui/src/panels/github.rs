//! GitHub vault import panel — fully in-browser, works on GitHub Pages.
//!
//! Fetches a public GitHub repository's markdown corpus over CORS-enabled
//! endpoints, extracts wikilink edges with vault-links, and promotes the
//! resulting graph onto the canvas. No graph-api server is required.
//!
//! Panel-local state lives in `GlobalSignal`s (same pattern as generate.rs) so
//! the file is self-contained. Settings persist to localStorage under
//! `"jc_github_spec"`.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};

use crate::github::{self, GitHubImportSpec};
use crate::graph_canvas;
use crate::Ctx;

use serde::{Deserialize, Serialize};
// ── localStorage key for persisting the last-used spec ─────────────────────────
const STORAGE_KEY: &str = "jc_github_spec";

/// Panel state persisted across sessions.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
struct PersistedSpec {
    repo: String,
    git_ref: String,
    path: String,
}

impl PersistedSpec {
    fn load() -> Self {
        LocalStorage::get(STORAGE_KEY).unwrap_or_default()
    }

    fn save(&self) {
        let _ = LocalStorage::set(STORAGE_KEY, self);
    }
}

impl From<&GitHubImportSpec> for PersistedSpec {
    fn from(spec: &GitHubImportSpec) -> Self {
        Self {
            repo: spec.repo.clone(),
            git_ref: spec.git_ref.clone(),
            path: spec.path.clone(),
        }
    }
}

impl From<&PersistedSpec> for GitHubImportSpec {
    fn from(p: &PersistedSpec) -> Self {
        GitHubImportSpec {
            repo: p.repo.clone(),
            git_ref: p.git_ref.clone(),
            path: p.path.clone(),
        }
    }
}

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
    let persisted = PersistedSpec::load();
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

                // Persist the spec.
                PersistedSpec::from(&spec).save();

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