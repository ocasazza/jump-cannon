//! Importers panel — browse, edit, copy, and live-preview runtime importer
//! packages (crates/importer `format_version = 3` TOML manifests).
//!
//! Three regions: a package list (server catalog from `GET /importers` plus
//! browser-local custom packages persisted in localStorage), a Monaco editor
//! column (full TOML manifest + a pest-grammar-only view), and a preview pane
//! that parses a sample input through `crate::pest_worker` — the Web Worker
//! is the CPU sandbox, since pest_vm has no fuel counter and an untrusted
//! grammar must never run on the UI thread.
//!
//! Grammar-view edits splice back into the TOML signal, but only for the
//! `grammar = '''` literal-string form the package template uses; any other
//! shape renders the grammar view read-only and the TOML view stays the edit
//! surface. Panel-local state lives in `GlobalSignal`s (same pattern as
//! generate.rs) so the file is self-contained.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use panel_kit::editor::{MonacoEditor, PANEL_KIT_DARK_THEME};
use wasm_bindgen::{JsCast, JsValue};

use crate::pest_worker::{parse_in_worker, ParsePreview};
use crate::{api, reload_graph, Ctx};

// --- persistence ---------------------------------------------------------------

/// localStorage map of browser-local package id → manifest TOML.
const PACKAGES_KEY: &str = "jump-cannon.importer.packages";
/// Per-package sample input, prefixed with the package id.
const SAMPLE_PREFIX: &str = "jump-cannon.importer.sample.";

fn load_packages() -> BTreeMap<String, String> {
    LocalStorage::get(PACKAGES_KEY).unwrap_or_default()
}

fn persist_packages() {
    let _ = LocalStorage::set(PACKAGES_KEY, &*PACKAGES.read());
}

fn sample_key(id: &str) -> String {
    format!("{SAMPLE_PREFIX}{id}")
}

// --- panel-local state -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    /// Server catalog profile id; an httpjson entry's package file is served
    /// and written through `/importers/:id/definition`.
    Catalog(String),
    /// Browser-local package id (key into PACKAGES).
    Local(String),
    /// Draft of a new server catalog source (`POST /importers`).
    NewCatalog,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EditorView {
    #[default]
    Manifest,
    Grammar,
}

#[derive(Clone, PartialEq)]
enum CatalogState {
    Idle,
    Loading,
    Ready(api::ImporterCatalog),
    Unavailable(String),
}

/// The authored package file behind the selected catalog source
/// (`GET`/`PUT /importers/:id/definition`).
#[derive(Clone, PartialEq)]
enum ServerPackage {
    /// Not requested for the current selection.
    Idle,
    Loading,
    /// Text as the server last served or accepted.
    Ready(api::ImporterDefinition),
    Failed(String),
}

/// `POST /importers` draft. The package body is the editor's MANIFEST
/// buffer, so it gets Monaco and the sandbox preview like any other package.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NewSourceDraft {
    id: String,
    name: String,
    endpoint: String,
    package: String,
    variables: Vec<(String, String)>,
}

impl Default for NewSourceDraft {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            endpoint: String::new(),
            package: "my-importer.toml".to_string(),
            variables: vec![(String::new(), String::new())],
        }
    }
}

#[derive(Clone)]
enum PreviewState {
    Idle,
    Done(ParsePreview),
    Failed { message: String, timeout: bool },
}

#[derive(Clone, PartialEq)]
pub(crate) enum ApplyState {
    Building {
        elapsed_secs: u64,
        /// Latest progress label naming the applied source, when the server
        /// produces one.
        stage: Option<String>,
        /// Latest `set_progress` fraction from the build's event log, when
        /// the running stage reports one (0.0–1.0).
        fraction: Option<f32>,
    },
    Ok {
        nodes: u32,
        edges: u32,
    },
    Error(String),
}

#[derive(Clone, PartialEq)]
pub(crate) struct ApplyStatus {
    /// Catalog id whose summary and row carry the status — the selection at
    /// Apply time, which is not the applied source for a reset.
    anchor: String,
    /// What is being applied, for display.
    pub(crate) target: String,
    /// The apply clears the session selection; no inline reset is offered.
    reset: bool,
    pub(crate) state: ApplyState,
}

static PACKAGES: GlobalSignal<BTreeMap<String, String>> = Signal::global(load_packages);
static SELECTION: GlobalSignal<Option<Selection>> = Signal::global(|| None);
static MANIFEST: GlobalSignal<String> = Signal::global(String::new);
static GRAMMAR: GlobalSignal<String> = Signal::global(String::new);
/// True when the manifest's grammar is in the spliceable `grammar = '''` form.
static GRAMMAR_SPLICEABLE: GlobalSignal<bool> = Signal::global(|| false);
static EDITOR_VIEW: GlobalSignal<EditorView> = Signal::global(EditorView::default);
static SAMPLE: GlobalSignal<String> = Signal::global(String::new);
static PREVIEW: GlobalSignal<PreviewState> = Signal::global(|| PreviewState::Idle);
static PREVIEW_RUNNING: GlobalSignal<bool> = Signal::global(|| false);
static STATUS: GlobalSignal<Option<String>> = Signal::global(|| None);
static CATALOG: GlobalSignal<CatalogState> = Signal::global(|| CatalogState::Idle);
/// Session-scoped source override (mirrors sessionStorage via api::source_id).
static VIEWING: GlobalSignal<Option<String>> = Signal::global(api::source_id);
static SERVER_PACKAGE: GlobalSignal<ServerPackage> = Signal::global(|| ServerPackage::Idle);
static NEW_SOURCE: GlobalSignal<NewSourceDraft> = Signal::global(NewSourceDraft::default);
/// A `PUT`/`POST` is in flight; the editor's write actions stay disabled.
static BUSY: GlobalSignal<bool> = Signal::global(|| false);
/// Last server rejection (validation text, authorization, read-only dir).
static ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
/// Live state of the last source Apply, anchored to a catalog row. Read by
/// the graph-area loading overlay while a build is in flight.
pub(crate) static APPLY: GlobalSignal<Option<ApplyStatus>> = Signal::global(|| None);
/// Bumped by every Apply; the tracker tasks of superseded applies exit
/// instead of overwriting the current status.
static APPLY_GEN: GlobalSignal<u64> = Signal::global(|| 0);

// --- manifest text surgery (pure functions; unit-tested below) --------------------

/// Extract the `[parser]` inline pest grammar when it is written in the
/// template's `grammar = '''` … `'''` literal-multiline form. Any other
/// shape (basic string, json engine) returns `None` and the grammar view
/// degrades to read-only.
fn extract_grammar(manifest: &str) -> Option<String> {
    let mut in_parser = false;
    let mut lines = manifest.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.starts_with('[') {
            in_parser = t == "[parser]";
            continue;
        }
        if in_parser && t == "grammar = '''" {
            let mut body = Vec::new();
            for inner in lines.by_ref() {
                if inner.trim() == "'''" {
                    return Some(body.join("\n"));
                }
                body.push(inner);
            }
            return None; // unterminated literal string — invalid TOML
        }
    }
    None
}

/// Replace the `[parser]` grammar with `grammar`, preserving the rest of the
/// manifest byte-for-byte. Fails when the manifest is not in the spliceable
/// form or the grammar itself contains a line that would terminate the
/// literal string early.
fn splice_grammar(manifest: &str, grammar: &str) -> Result<String, String> {
    if grammar.contains("'''") {
        return Err("grammar contains '''; edit the manifest TOML directly".into());
    }
    let had_trailing_newline = manifest.ends_with('\n');
    let mut out: Vec<&str> = Vec::new();
    let mut in_parser = false;
    let mut lines = manifest.lines();
    let mut replaced = false;
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.starts_with('[') {
            in_parser = t == "[parser]";
            out.push(line);
            continue;
        }
        if in_parser && !replaced && t == "grammar = '''" {
            out.push(line);
            out.extend(grammar.lines());
            for inner in lines.by_ref() {
                if inner.trim() == "'''" {
                    out.push(inner);
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                return Err("manifest grammar literal is unterminated".into());
            }
            continue;
        }
        out.push(line);
    }
    if !replaced {
        return Err(
            "no grammar = ''' literal found under [parser]; edit the manifest TOML directly"
                .into(),
        );
    }
    let mut joined = out.join("\n");
    if had_trailing_newline {
        joined.push('\n');
    }
    Ok(joined)
}

/// Rewrite `[metadata] id = "…"` for duplicated packages; no-op when the
/// line is absent (validation will surface the mismatch on parse).
fn rewrite_metadata_id(manifest: &str, new_id: &str) -> String {
    let mut out = Vec::new();
    let mut in_metadata = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_metadata = t == "[metadata]";
            out.push(line.to_string());
            continue;
        }
        if in_metadata && t.starts_with("id") && t.contains('=') {
            out.push(format!("id = \"{new_id}\""));
            continue;
        }
        out.push(line.to_string());
    }
    let mut joined = out.join("\n");
    if manifest.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Package-id charset per data_loader::identity::validate_source_id:
/// lowercase ASCII letters, digits, '.', '-', '_'.
fn sanitize_id(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_') {
                c
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() {
        "package".to_string()
    } else {
        trimmed.chars().take(128).collect()
    }
}

/// Seed manifest for a new local package — the pest engine's line-graph
/// example from crates/importer's pest tests.
fn template_manifest(id: &str, name: &str) -> String {
    format!(
        r#"format_version = 3

[metadata]
id = "{id}"
name = "{name}"
version = "0.1.0"
description = "Browser-edited importer package"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''
document = {{ SOI ~ (record ~ NEWLINE?)* ~ EOI }}
record = _{{ node | edge }}
node = {{ "N|" ~ node_id ~ "|" ~ title ~ "|" ~ kind ~ "|" ~ tags ~ "|" ~ properties }}
node_id = @{{ field }}
title = @{{ field }}
kind = @{{ field }}
tags = _{{ (tag ~ ("," ~ tag)*)? }}
tag = @{{ atom }}
properties = _{{ (property ~ (";" ~ property)*)? }}
property = {{ key ~ "=" ~ value }}
key = @{{ atom }}
value = @{{ atom }}
edge = {{ "E|" ~ source ~ "|" ~ target }}
source = @{{ field }}
target = @{{ field }}
field = _{{ (!("|" | NEWLINE) ~ ANY)+ }}
atom = _{{ (!("," | ";" | "=" | "|" | NEWLINE) ~ ANY)+ }}
'''

[parser.captures]
node = "node"
id = "node_id"
title = "title"
kind = "kind"
tag = "tag"
property = "property"
key = "key"
value = "value"
edge = "edge"
source = "source"
target = "target"
"#
    )
}

/// The default sample input matching the template grammar.
const TEMPLATE_SAMPLE: &str = "N|alpha|Alpha|note|docs|owner=ops\nN|beta|Beta|note||\nE|alpha|beta\n";

/// ssh/grpc connectors execute inside graph-api (native); the browser can
/// only preview pest grammars.
fn is_native_kind(kind: &str) -> bool {
    matches!(kind, "ssh" | "grpc")
}

// --- actions ---------------------------------------------------------------------

fn set_manifest(text: String) {
    let grammar = extract_grammar(&text);
    *GRAMMAR_SPLICEABLE.write() = grammar.is_some();
    *GRAMMAR.write() = grammar.unwrap_or_default();
    if let Some(Selection::Local(id)) = SELECTION.peek().clone() {
        PACKAGES.write().insert(id, text.clone());
        persist_packages();
    }
    *MANIFEST.write() = text;
}
/// Manifest write from the grammar editor: the editor already displays the
/// typed grammar, so GRAMMAR must not be re-extracted and written back —
/// echoing keystrokes into the bound signal mid-edit corrupts the edit.
fn set_manifest_from_grammar(text: String) {
    if let Some(Selection::Local(id)) = SELECTION.peek().clone() {
        PACKAGES.write().insert(id, text.clone());
        persist_packages();
    }
    *MANIFEST.write() = text;
}

fn select(sel: Selection) {
    *SERVER_PACKAGE.write() = ServerPackage::Idle;
    *ERROR.write() = None;
    match &sel {
        Selection::Local(id) => {
            let manifest = PACKAGES.peek().get(id).cloned().unwrap_or_default();
            *SAMPLE.write() = LocalStorage::get(sample_key(id)).unwrap_or_default();
            *SELECTION.write() = Some(sel);
            set_manifest(manifest);
        }
        Selection::Catalog(_) => {
            *SELECTION.write() = Some(sel);
            set_manifest(String::new());
        }
        Selection::NewCatalog => {
            *SELECTION.write() = Some(sel);
            set_manifest(template_manifest("custom.new-source", "New source"));
        }
    }
    *PREVIEW.write() = PreviewState::Idle;
    *STATUS.write() = None;
}

/// Start a `POST /importers` draft: a fresh catalog entry plus the package
/// template as its body.
fn new_catalog_source() {
    *NEW_SOURCE.write() = NewSourceDraft::default();
    select(Selection::NewCatalog);
}

/// Fetch the selected catalog source's authored package into the editor.
fn load_server_package(id: String) {
    *SERVER_PACKAGE.write() = ServerPackage::Loading;
    *ERROR.write() = None;
    spawn(async move {
        match api::importer_definition(&id).await {
            Ok(definition) => {
                set_manifest(definition.source.clone());
                *SERVER_PACKAGE.write() = ServerPackage::Ready(definition);
            }
            Err(message) => *SERVER_PACKAGE.write() = ServerPackage::Failed(message),
        }
    });
}

/// Write the editor buffer back. The server validates before it writes, so a
/// rejection leaves the deployed package untouched and is shown verbatim.
fn save_server_package(id: String) {
    if *BUSY.peek() {
        return;
    }
    let source = MANIFEST.peek().clone();
    *BUSY.write() = true;
    *ERROR.write() = None;
    *STATUS.write() = Some("validating and saving…".into());
    spawn(async move {
        match api::put_importer_definition(&id, &source).await {
            Ok(definition) => {
                *STATUS.write() =
                    Some("saved — applying this source rebuilds it from the new package".into());
                *SERVER_PACKAGE.write() = ServerPackage::Ready(definition);
            }
            Err(message) => {
                *STATUS.write() = None;
                *ERROR.write() = Some(message);
            }
        }
        *BUSY.write() = false;
    });
}

/// `POST /importers`: the draft's catalog entry plus the editor buffer as its
/// package file. On success the catalog is refetched and the new source
/// selected, so the same editor now edits it.
fn create_catalog_source() {
    if *BUSY.peek() {
        return;
    }
    let draft = NEW_SOURCE.peek().clone();
    let importer = api::NewImporter {
        id: draft.id.trim().to_string(),
        name: draft.name.trim().to_string(),
        description: String::new(),
        package: draft.package.trim().to_string(),
        source: MANIFEST.peek().clone(),
        endpoint: draft.endpoint.trim().to_string(),
        variables: draft
            .variables
            .iter()
            .filter(|(key, _)| !key.trim().is_empty())
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect(),
    };
    *BUSY.write() = true;
    *ERROR.write() = None;
    *STATUS.write() = Some("validating and creating…".into());
    spawn(async move {
        match api::post_importer(&importer).await {
            Ok(profile) => {
                *CATALOG.write() = match api::importers().await {
                    Ok(catalog) => CatalogState::Ready(catalog),
                    Err(error) => CatalogState::Unavailable(error),
                };
                select(Selection::Catalog(profile.id));
            }
            Err(message) => {
                *STATUS.write() = None;
                *ERROR.write() = Some(message);
            }
        }
        *BUSY.write() = false;
    });
}

fn unique_local_id(base: &str) -> String {
    if !PACKAGES.peek().contains_key(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !PACKAGES.peek().contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn new_package() {
    let id = unique_local_id("custom.new-package");
    let manifest = template_manifest(&id, "New package");
    PACKAGES.write().insert(id.clone(), manifest);
    persist_packages();
    let _ = LocalStorage::set(sample_key(&id), TEMPLATE_SAMPLE);
    select(Selection::Local(id));
}

fn duplicate_local(id: &str) {
    let Some(source) = PACKAGES.peek().get(id).cloned() else {
        return;
    };
    let new_id = unique_local_id(&format!("{id}-copy"));
    PACKAGES
        .write()
        .insert(new_id.clone(), rewrite_metadata_id(&source, &new_id));
    persist_packages();
    select(Selection::Local(new_id));
}

fn duplicate_catalog(profile: &api::ImporterProfile) {
    let id = unique_local_id(&format!("custom.{}", sanitize_id(&profile.id)));
    let name = format!("{} (copy)", profile.display_name);
    PACKAGES
        .write()
        .insert(id.clone(), template_manifest(&id, &name));
    persist_packages();
    let _ = LocalStorage::set(sample_key(&id), TEMPLATE_SAMPLE);
    select(Selection::Local(id));
}

fn delete_local(id: &str) {
    let confirmed = web_sys::window()
        .and_then(|w| w.confirm_with_message(&format!("Delete local package '{id}'?")).ok())
        .unwrap_or(false);
    if !confirmed {
        return;
    }
    PACKAGES.write().remove(id);
    persist_packages();
    let _ = LocalStorage::delete(sample_key(id));
    if *SELECTION.peek() == Some(Selection::Local(id.to_string())) {
        *SELECTION.write() = None;
        *MANIFEST.write() = String::new();
        *GRAMMAR.write() = String::new();
        *PREVIEW.write() = PreviewState::Idle;
    }
}

fn js_get(obj: &JsValue, key: &str) -> Option<JsValue> {
    js_sys::Reflect::get(obj, &JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

/// `navigator.clipboard.writeText(text)` via Reflect — fire-and-forget, same
/// approach as the Instances panel's share-link copy.
fn copy_to_clipboard(text: &str) {
    let Some(win) = web_sys::window() else { return };
    let win: &JsValue = win.as_ref();
    let Some(clip) = js_get(win, "navigator").and_then(|n| js_get(&n, "clipboard")) else {
        return;
    };
    if let Some(f) = js_get(&clip, "writeText").and_then(|f| f.dyn_into::<js_sys::Function>().ok())
    {
        let _ = f.call1(&clip, &JsValue::from_str(text));
    }
}

// --- source apply tracking -------------------------------------------------------

/// Switch the session's graph view and track the resulting load. `target` is
/// the catalog id to view, or `None` to clear the session selection and return
/// to the deployment default. `anchor` is the catalog row the status renders
/// under (the currently selected summary), which differs from `target` when a
/// reset is triggered from a failed alternate's summary.
fn apply_source(ctx: Ctx, anchor: String, target: Option<String>) {
    match &target {
        Some(id) => {
            api::set_source_id(id);
            *VIEWING.write() = Some(id.clone());
        }
        None => {
            api::clear_source_id();
            VIEWING.write().take();
        }
    }
    let generation = APPLY_GEN.peek().wrapping_add(1);
    *APPLY_GEN.write() = generation;
    *APPLY.write() = Some(ApplyStatus {
        anchor,
        target: target
            .clone()
            .unwrap_or_else(|| "the deployment default".to_string()),
        reset: target.is_none(),
        state: ApplyState::Building {
            elapsed_secs: 0,
            stage: None,
            fraction: None,
        },
    });
    spawn(track_apply(ctx, generation));
}



/// Build a client-side [`GraphData`] from a pest parse preview — the same
/// shape as the Generate panel's `graph_data_from_generated`, with positions
/// from the Layout panel's seed strategy and neutral metric defaults.
fn graph_data_from_preview(p: &crate::pest_worker::ParsePreview) -> crate::GraphData {
    use std::collections::HashMap;

    let n = p.node_ids.len();
    let ids: Vec<String> = p.node_ids.clone();
    let id_to_idx: HashMap<String, u32> = ids
        .iter()
        .enumerate()
        .map(|(idx, id)| (id.clone(), idx as u32))
        .collect();
    let mut edges: Vec<u32> = Vec::with_capacity(p.edge_pairs.len() * 2);
    for (source, target) in &p.edge_pairs {
        let (Some(&s), Some(&t)) = (id_to_idx.get(source), id_to_idx.get(target)) else {
            continue;
        };
        edges.push(s);
        edges.push(t);
    }
    let n_edges = (edges.len() / 2) as u32;
    let positions = super::layout::seed_positions_for_generated(n);
    let metrics: HashMap<String, Vec<f32>> = HashMap::new();
    let colors = crate::render::data::colors_from_metric("community", &metrics, n);
    let sizes = crate::render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);
    // Weakly-connected-component count over the mounted (capped) subgraph —
    // cheap BFS, mirrors the generated-graph path.
    let mut seen = vec![false; n];
    let mut num_wcc = 0u32;
    for start in 0..n {
        if seen[start] {
            continue;
        }
        num_wcc += 1;
        seen[start] = true;
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            for pair in edges.chunks(2) {
                if pair.len() == 2 {
                    let (a, b) = (pair[0] as usize, pair[1] as usize);
                    if a == node && !seen[b] {
                        seen[b] = true;
                        stack.push(b);
                    } else if b == node && !seen[a] {
                        seen[a] = true;
                        stack.push(a);
                    }
                }
            }
        }
    }
    crate::GraphData {
        graph_revision: None,
        n_nodes: n as u32,
        n_edges,
        num_communities: 0,
        num_wcc,
        ids,
        id_to_idx,
        scene: crate::render::Scene {
            positions,
            edges,
            colors,
            sizes,
        },
    }
}

/// Retry the graph load until it settles, and record the outcome.
///
/// Selecting a source graph-api has not built yet makes the first fetch
/// answer 503 + `Retry-After` (`building importer source …`) while a
/// background task imports the corpus — the fetch is fast, the build is not.
/// Loop: reload, and while the failure names an in-flight build, wait a beat
/// and retry. All requests stay short (nothing blocks on the build), the UI
/// keeps rendering, and `track_stages` keeps the apply status fed from the
/// build's live progress log.
async fn track_apply(ctx: Ctx, generation: u64) {
    spawn(track_stages(generation));
    loop {
        reload_graph(ctx).await;
        if *APPLY_GEN.peek() != generation {
            return;
        }
        let error = ctx.load_error.peek().clone();
        let state = match (&error, ctx.graph.peek().as_ref()) {
            (Some(error), _) if api::is_building_error(error) => {
                // Still building: retry shortly. The apply status stays in
                // its Building state, refreshed by `track_stages`.
                gloo_timers::future::TimeoutFuture::new(1000).await;
                if *APPLY_GEN.peek() != generation {
                    return;
                }
                continue;
            }
            (Some(error), _) => ApplyState::Error(error.clone()),
            (None, Some(graph)) => ApplyState::Ok {
                nodes: graph.n_nodes,
                edges: graph.n_edges,
            },
            (None, None) => {
                ApplyState::Error("another graph load superseded this one".into())
            }
        };
        if let Some(status) = APPLY.write().as_mut() {
            status.state = state;
        }
        return;
    }
}

/// Refresh the apply status once a second from the applied source's live
/// progress log.
///
/// `/progress` carries the source-selection header (see `api::get`), so
/// after this session applied an alternate it returns that source's own
/// event log — including while the import build runs, because the progress
/// route never blocks behind a build. Stage labels and `set_progress`
/// fractions fold into the `Building` status shown on the anchored row, the
/// graph-area overlay, and (via the app's shared progress sink) the
/// Progress panel.
async fn track_stages(generation: u64) {
    let started = js_sys::Date::now();
    let mut since = api::progress(0)
        .await
        .map(|resp| resp.next_seq)
        .unwrap_or(0);
    // Task-id → current label, so a bare `SetProgress`/`Finish` on a known
    // task can be attributed to its stage.
    let mut labels: std::collections::HashMap<u64, String> =
        std::collections::HashMap::new();
    let mut stage = None;
    let mut fraction = None;
    loop {
        gloo_timers::future::TimeoutFuture::new(1000).await;
        if *APPLY_GEN.peek() != generation
            || !matches!(
                APPLY.peek().as_ref().map(|s| &s.state),
                Some(ApplyState::Building { .. })
            )
        {
            return;
        }
        if let Ok(resp) = api::progress(since).await {
            since = resp.next_seq;
            for stamped in &resp.events {
                match &stamped.event {
                    api::ProgressEvent::Start { id, label, .. } => {
                        labels.insert(*id, label.clone());
                        stage = Some(label.clone());
                        fraction = None;
                    }
                    api::ProgressEvent::UpdateLabel { id, label } => {
                        labels.insert(*id, label.clone());
                        stage = Some(label.clone());
                    }
                    api::ProgressEvent::SetProgress { id, progress } => {
                        fraction = Some(*progress);
                        if let Some(label) = labels.get(id) {
                            stage = Some(label.clone());
                        }
                    }
                    api::ProgressEvent::Finish { id } | api::ProgressEvent::Fail { id, .. } => {
                        labels.remove(id);
                        if labels.is_empty() {
                            fraction = None;
                        }
                    }
                    api::ProgressEvent::Log { message, .. } => {
                        stage = Some(message.clone());
                    }
                }
            }
        }
        if *APPLY_GEN.peek() != generation {
            return;
        }
        let elapsed_secs = ((js_sys::Date::now() - started) / 1000.0) as u64;
        if let Some(status) = APPLY.write().as_mut() {
            if matches!(status.state, ApplyState::Building { .. }) {
                status.state = ApplyState::Building {
                    elapsed_secs,
                    stage: stage.clone(),
                    fraction,
                };
            }
        }
    }
}


/// Inline escape from a failed alternate: clearing the session's own selection
/// is session-local and allowed whatever the deployment's runtime-switch
/// posture, so this is offered even where Apply is not.
fn return_to_default(ctx: Ctx, anchor: String) -> Element {
    rsx! {
        button {
            class: "btn imp-mini",
            r#type: "button",
            "data-action": "return-to-default",
            onclick: move |_| apply_source(ctx, anchor.clone(), None),
            "Return to default"
        }
    }
}

/// The `[data-field=apply-status]` block under the anchored catalog summary.
fn apply_status_view(ctx: Ctx, status: &ApplyStatus) -> Element {
    let target = status.target.clone();
    let anchor = status.anchor.clone();
    let offer_reset = !status.reset;
    match &status.state {
        ApplyState::Building {
            elapsed_secs,
            stage,
            fraction,
        } => rsx! {
            div {
                class: "imp-apply building",
                role: "status",
                "data-field": "apply-status",
                "data-outcome": "building",
                "data-elapsed": "{elapsed_secs}",
                span { class: "imp-apply-line", "building {target}… {elapsed_secs}s" }
                if let Some(stage) = stage {
                    span { class: "imp-apply-stage", "data-field": "apply-stage", "{stage}" }
                }
                div { class: "imp-progress",
                    role: "progressbar",
                    if let Some(fraction) = fraction {
                        div {
                            class: "imp-progress-fill",
                            style: format!("width: {:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
                        }
                    } else {
                        div { class: "imp-progress-fill indeterminate" }
                    }
                }
                span { class: "imp-note",
                    "the server imports, measures, and indexes this source in the background; stages stream below and in the Progress panel"
                }
            }
        },
        ApplyState::Ok { nodes, edges } => rsx! {
            div {
                class: "imp-apply ok",
                role: "status",
                "data-field": "apply-status",
                "data-outcome": "ok",
                span { class: "imp-apply-line",
                    "now viewing {target} — "
                    strong { "data-field": "apply-nodes", "{nodes}" }
                    " nodes · "
                    strong { "data-field": "apply-edges", "{edges}" }
                    " edges"
                }
            }
        },
        ApplyState::Error(message) => rsx! {
            div {
                class: "imp-apply error",
                role: "alert",
                "data-field": "apply-status",
                "data-outcome": "error",
                span { class: "imp-apply-line", "{target} failed to load — {message}" }
                if offer_reset {
                    {return_to_default(ctx, anchor.clone())}
                }
            }
        },
    }
}

// --- panel ------------------------------------------------------------------------

pub fn panel(ctx: Ctx) -> Element {
    // Kick the catalog fetch once. Pure-browser deployments (GitHub Pages,
    // ?gh= deep links) have no /importers endpoint; skip the 404 and say so.
    let browser_hosted =
        crate::github::boot_spec().is_some() && !ctx.graph_session.read().is_server_backed();
    if matches!(&*CATALOG.read(), CatalogState::Idle) {
        spawn(async move {
            if browser_hosted {
                *CATALOG.write() =
                    CatalogState::Unavailable("browser-hosted deployment (no graph-api)".into());
                return;
            }
            *CATALOG.write() = CatalogState::Loading;
            *CATALOG.write() = match api::importers().await {
                Ok(catalog) => CatalogState::Ready(catalog),
                Err(error) => CatalogState::Unavailable(error),
            };
        });
    }

    let selection = SELECTION.read().clone();
    let packages = PACKAGES.read().clone();
    let catalog = CATALOG.read().clone();
    let viewing = VIEWING.read().clone();
    let apply = APPLY.read().clone();
    let editor_view = *EDITOR_VIEW.read();
    let preview = PREVIEW.read().clone();
    let preview_running = *PREVIEW_RUNNING.read();
    let status = STATUS.read().clone();
    let spliceable = *GRAMMAR_SPLICEABLE.read();
    let server_package = SERVER_PACKAGE.read().clone();
    let busy = *BUSY.read();
    let error = ERROR.read().clone();
    let manifest_empty = MANIFEST.read().trim().is_empty();
    let switch_allowed = catalog_runtime_switch(&catalog);

    let run_preview = move |_| {
        if *PREVIEW_RUNNING.read() {
            return;
        }
        let manifest = MANIFEST.peek().clone();
        let input = SAMPLE.peek().clone();
        *PREVIEW_RUNNING.write() = true;
        *PREVIEW.write() = PreviewState::Idle;
        *STATUS.write() = Some("parsing in the sandbox worker…".into());
        spawn(async move {
            match parse_in_worker(manifest, input).await {
                Ok(p) => {
                    *STATUS.write() = None;
                    *PREVIEW.write() = PreviewState::Done(p);
                }
                Err(e) => {
                    let timeout = e.contains("timed out");
                    *STATUS.write() = None;
                    *PREVIEW.write() = PreviewState::Failed {
                        message: e,
                        timeout,
                    };
                }
            }
            *PREVIEW_RUNNING.write() = false;
        });
    };

    rsx! {
        div { class: "importers-panel", "data-panel": "importers",
            // ── left: package list ──────────────────────────────────────
            div { class: "imp-list",
                div { class: "imp-list-head",
                    span { "Server catalog" }
                    if matches!(catalog, CatalogState::Ready(_)) {
                        button {
                            class: "btn imp-mini",
                            r#type: "button",
                            "data-action": "new-source",
                            onclick: move |_| new_catalog_source(),
                            "+ New source"
                        }
                        button {
                            class: "btn imp-mini",
                            r#type: "button",
                            "data-action": "refresh-catalog",
                            onclick: move |_| {
                                spawn(async move {
                                    *CATALOG.write() = CatalogState::Loading;
                                    *CATALOG.write() = match api::importers().await {
                                        Ok(c) => CatalogState::Ready(c),
                                        Err(e) => CatalogState::Unavailable(e),
                                    };
                                });
                            },
                            "↻"
                        }
                    }
                }
                div { class: "imp-rows", role: "listbox", aria_label: "Server catalog",
                    match &catalog {
                        CatalogState::Idle | CatalogState::Loading => rsx! {
                            div { class: "imp-note", role: "status", "loading catalog…" }
                        },
                        CatalogState::Unavailable(reason) => rsx! {
                            div { class: "imp-note", "data-field": "catalog-unavailable",
                                "server catalog unavailable — {reason}"
                            }
                        },
                        CatalogState::Ready(catalog) => rsx! {
                            if catalog.sources.is_empty() {
                                div { class: "imp-note", "catalog is empty" }
                            }
                            if !catalog.runtime_switch.enabled {
                                div { class: "imp-note", "data-field": "switch-posture",
                                    "runtime switching is disabled by this deployment; apply requires a rollout"
                                }
                            } else if !catalog.runtime_switch.allowed {
                                div { class: "imp-note", "data-field": "switch-posture",
                                    "viewing other sources requires authorization"
                                }
                            }
                            for profile in &catalog.sources {
                                {
                                    let id = profile.id.clone();
                                    let click_id = id.clone();
                                    let selected = selection == Some(Selection::Catalog(id.clone()));
                                    let native = is_native_kind(&profile.kind);
                                    let is_viewing = viewing.as_deref() == Some(profile.id.as_str())
                                        || (viewing.is_none() && profile.selected);
                                    let apply_chip = apply
                                        .as_ref()
                                        .filter(|status| status.anchor == id)
                                        .map(|status| match &status.state {
                                            ApplyState::Building { .. } => ("building", "building"),
                                            ApplyState::Ok { .. } => ("ok", "ready"),
                                            ApplyState::Error(_) => ("error", "failed"),
                                        });
                                    // Every runnable row carries its own Load
                                    // action, so loading a source's graph is one
                                    // click from the list — while clicking the
                                    // row itself only selects it, keeping
                                    // catalog browsing free of server-side
                                    // imports. The default row's Load returns
                                    // the session to the deployment default.
                                    let load_target = if switch_allowed
                                        && (profile.selected || profile.runnable)
                                        && !is_viewing
                                    {
                                        if profile.selected {
                                            Some(None)
                                        } else {
                                            Some(Some(id.clone()))
                                        }
                                    } else {
                                        None
                                    };
                                    let loadable = load_target.is_some();
                                    let load_id = id.clone();
                                    rsx! {
                                        div { key: "{id}", class: "imp-row-wrap", role: "presentation",
                                            button {
                                                class: if selected { "imp-row selected" } else { "imp-row" },
                                                r#type: "button",
                                                role: "option",
                                                aria_selected: if selected { "true" } else { "false" },
                                                "data-package-id": "{profile.id}",
                                                "data-source": "server",
                                                "data-kind": "{profile.kind}",
                                                "data-native": if native { "true" } else { "false" },
                                                "data-viewing": if is_viewing { "true" } else { "false" },
                                                "data-loadable": if loadable { "true" } else { "false" },
                                                onclick: move |_| select(Selection::Catalog(click_id.clone())),
                                                span { class: "imp-row-name", "{profile.display_name}" }
                                                span { class: "imp-row-chips",
                                                    span { class: "imp-chip", "{profile.kind}" }
                                                    if native {
                                                        span { class: "imp-chip native",
                                                            title: "native-only connector: runs inside graph-api, not in the browser",
                                                            "native"
                                                        }
                                                    }
                                                    if is_viewing {
                                                        span { class: "imp-chip viewing", "viewing" }
                                                    }
                                                    if let Some((outcome, label)) = apply_chip {
                                                        span {
                                                            class: "imp-chip apply",
                                                            "data-field": "apply-chip",
                                                            "data-outcome": "{outcome}",
                                                            "{label}"
                                                        }
                                                    }
                                                }
                                            }
                                            if let Some(target) = load_target {
                                                button {
                                                    class: "btn imp-row-load",
                                                    r#type: "button",
                                                    "data-action": "load-row",
                                                    "data-package-id": "{profile.id}",
                                                    aria_label: if target.is_none() {
                                                        "Return to the deployment default"
                                                    } else {
                                                        "Load this source's graph"
                                                    },
                                                    title: if target.is_none() {
                                                        "return this session to the deployment default"
                                                    } else {
                                                        "load this source's graph (imports it if the server has not built it yet)"
                                                    },
                                                    onclick: move |_| {
                                                        select(Selection::Catalog(load_id.clone()));
                                                        apply_source(ctx, load_id.clone(), target.clone());
                                                    },
                                                    if target.is_none() { "⟲" } else { "▶" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
                div { class: "imp-list-head",
                    span { "Local packages" }
                    button {
                        class: "btn imp-mini",
                        r#type: "button",
                        "data-action": "new-package",
                        onclick: move |_| new_package(),
                        "+ New package"
                    }
                }
                div { class: "imp-rows", role: "listbox", aria_label: "Local packages",
                    if packages.is_empty() {
                        div { class: "imp-note", "no local packages yet" }
                    }
                    for id in packages.keys() {
                        {
                            let id = id.clone();
                            let click_id = id.clone();
                            let selected = selection == Some(Selection::Local(id.clone()));
                            rsx! {
                                button {
                                    key: "{id}",
                                    class: if selected { "imp-row selected" } else { "imp-row" },
                                    r#type: "button",
                                    role: "option",
                                    aria_selected: if selected { "true" } else { "false" },
                                    "data-package-id": "{id}",
                                    "data-source": "local",
                                    onclick: move |_| select(Selection::Local(click_id.clone())),
                                    span { class: "imp-row-name", "{id}" }
                                    span { class: "imp-row-chips",
                                        span { class: "imp-chip local", "local" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── center: manifest / grammar editors ─────────────────────
            div { class: "imp-editor",
                {match &selection {
                    None => rsx! {
                        div { class: "imp-note imp-center-hint",
                            "select a package to view or edit it"
                        }
                    },
                    Some(Selection::Catalog(pid)) => {
                        let profile = match &catalog {
                            CatalogState::Ready(c) => {
                                c.sources.iter().find(|p| &p.id == pid).cloned()
                            }
                            _ => None,
                        };
                        match profile {
                            Some(profile) => {
                                let is_default = profile.selected;
                                let is_viewing = viewing.as_deref() == Some(profile.id.as_str())
                                    || (viewing.is_none() && is_default);
                                let native = is_native_kind(&profile.kind);
                                let apply_allowed = switch_allowed
                                    && (is_default || profile.runnable)
                                    && !is_viewing;
                                // Clearing the session's own selection is session-local
                                // and needs no deployment authorization — always offer it
                                // on the default profile while viewing elsewhere.
                                let reset_offered = is_default && viewing.is_some() && !is_viewing;
                                let apply_label = if is_default {
                                    "Return to default"
                                } else {
                                    "Apply (view this source)"
                                };
                                let apply_id = profile.id.clone();
                                // Only httpjson entries name one package file the server
                                // can serve and rewrite through /importers/:id/definition.
                                let has_package_file = profile.kind == "httpjson";
                                let load_id = profile.id.clone();
                                let save_id = profile.id.clone();
                                let editor_key = profile.id.clone();
                                let display_name = profile.display_name.clone();
                                let package_id = profile.id.clone();
                                let kind = profile.kind.clone();
                                let description = profile.description.clone();
                                let apply_anchor = profile.id.clone();
                                let apply_here = apply
                                    .as_ref()
                                    .filter(|status| status.anchor == profile.id)
                                    .cloned();
                                rsx! {
                                    div { class: "imp-summary",
                                        div { class: "imp-summary-title", "{display_name}" }
                                        dl { class: "imp-facts",
                                            div { class: "imp-fact",
                                                dt { "package" }
                                                dd { "data-field": "package-id", "{package_id}" }
                                            }
                                            div { class: "imp-fact",
                                                dt { "kind" }
                                                dd { "data-field": "package-kind", "{kind}" }
                                            }
                                            if !description.is_empty() {
                                                div { class: "imp-fact",
                                                    dt { "description" }
                                                    dd { "{description}" }
                                                }
                                            }
                                            if let ServerPackage::Ready(definition) = &server_package {
                                                div { class: "imp-fact",
                                                    dt { "file" }
                                                    dd { "data-field": "package-file", "{definition.package}" }
                                                }
                                            }
                                        }
                                        if native {
                                            div { class: "imp-note",
                                                title: "native-only connector",
                                                "native-only connector: ssh/grpc acquisition runs inside graph-api; the browser cannot preview it"
                                            }
                                        }
                                        if !has_package_file {
                                            div { class: "imp-note",
                                                "this source kind names no single package file; duplicate the entry to edit a local copy"
                                            }
                                        }
                                        div { class: "imp-actions",
                                            button {
                                                class: "btn",
                                                r#type: "button",
                                                "data-action": "duplicate",
                                                onclick: move |_| duplicate_catalog(&profile),
                                                "Duplicate to local"
                                            }
                                            if apply_allowed || reset_offered {
                                                button {
                                                    class: "btn",
                                                    r#type: "button",
                                                    "data-action": "apply",
                                                    disabled: native && !ctx.graph_session.read().is_server_backed(),
                                                    title: if native {
                                                        "native-only connector: apply switches the server-hosted source; it cannot run in the browser"
                                                    } else {
                                                        "switch this browser session's graph view"
                                                    },
                                                    onclick: move |_| {
                                                        let target = (!is_default)
                                                            .then(|| apply_id.clone());
                                                        apply_source(
                                                            ctx,
                                                            apply_anchor.clone(),
                                                            target,
                                                        );
                                                    },
                                                    "{apply_label}"
                                                }
                                            }
                                            if has_package_file
                                                && matches!(server_package, ServerPackage::Idle | ServerPackage::Failed(_))
                                            {
                                                button {
                                                    class: "btn",
                                                    r#type: "button",
                                                    "data-action": "load-definition",
                                                    onclick: move |_| load_server_package(load_id.clone()),
                                                    "Edit server package"
                                                }
                                            }
                                        }
                                        if let Some(status) = &apply_here {
                                            {apply_status_view(ctx, status)}
                                        }
                                    }
                                    match &server_package {
                                        ServerPackage::Idle => rsx! {},
                                        ServerPackage::Loading => rsx! {
                                            div { class: "imp-note", role: "status", "loading package definition…" }
                                        },
                                        ServerPackage::Failed(message) => rsx! {
                                            div { class: "imp-error", role: "alert",
                                                "data-field": "definition-error", "{message}"
                                            }
                                        },
                                        ServerPackage::Ready(definition) => {
                                            // The server probes its own packages directory:
                                            // a ConfigMap projection is immutable however
                                            // authorized the viewer is.
                                            let editable = switch_allowed && definition.writable;
                                            let original = definition.source.clone();
                                            let dirty = *MANIFEST.read() != definition.source;
                                            rsx! {
                                                {manifest_editor(editor_key.clone(), editor_view, spliceable, !editable)}
                                                if !editable {
                                                    p { class: "imp-readonly", "data-field": "definition-readonly",
                                                        if !switch_allowed {
                                                            "read-only: writing a deployment package requires runtime switching to be enabled for your group"
                                                        } else {
                                                            "read-only: the server's packages directory is not writable"
                                                        }
                                                    }
                                                }
                                                div { class: "imp-actions",
                                                    button {
                                                        class: "btn",
                                                        r#type: "button",
                                                        "data-action": "save-definition",
                                                        disabled: !editable || !dirty || busy,
                                                        onclick: move |_| save_server_package(save_id.clone()),
                                                        "Save to server"
                                                    }
                                                    button {
                                                        class: "btn",
                                                        r#type: "button",
                                                        "data-action": "revert-definition",
                                                        disabled: !dirty || busy,
                                                        onclick: move |_| {
                                                            set_manifest(original.clone());
                                                            *ERROR.write() = None;
                                                            *STATUS.write() = None;
                                                        },
                                                        "Revert"
                                                    }
                                                    button {
                                                        class: "btn",
                                                        r#type: "button",
                                                        "data-action": "copy-toml",
                                                        onclick: move |_| {
                                                            copy_to_clipboard(&MANIFEST.peek());
                                                            *STATUS.write() = Some("package copied to clipboard".into());
                                                        },
                                                        "Copy TOML"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            None => rsx! {
                                div { class: "imp-note", "catalog entry not loaded — refresh the catalog" }
                            },
                        }
                    }
                    Some(Selection::Local(pid)) => {
                        let pid = pid.clone();
                        let pid_dup = pid.clone();
                        let pid_export = pid.clone();
                        let pid_delete = pid.clone();
                        let editor_key = pid.clone();
                        rsx! {
                            {manifest_editor(editor_key, editor_view, spliceable, false)}
                            div { class: "imp-actions",
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "duplicate",
                                    onclick: move |_| duplicate_local(&pid_dup),
                                    "Duplicate"
                                }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "copy-toml",
                                    onclick: move |_| {
                                        copy_to_clipboard(&MANIFEST.peek());
                                        *STATUS.write() = Some("manifest copied to clipboard".into());
                                    },
                                    "Copy TOML"
                                }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "export",
                                    onclick: move |_| {
                                        if let Err(e) = super::instances::download_text(
                                            &format!("{pid_export}.toml"),
                                            "application/toml",
                                            &MANIFEST.peek(),
                                        ) {
                                            *STATUS.write() = Some(format!("export: {e}"));
                                        }
                                    },
                                    "Export"
                                }
                                button {
                                    class: "btn imp-danger",
                                    r#type: "button",
                                    "data-action": "delete",
                                    onclick: move |_| delete_local(&pid_delete),
                                    "Delete"
                                }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "apply",
                                    title: "preview only in browser mode",
                                    onclick: run_preview,
                                    "Apply"
                                }
                            }
                        }
                    }
                    Some(Selection::NewCatalog) => {
                        let draft = NEW_SOURCE.read().clone();
                        let rows = draft.variables.len();
                        let ready = !draft.id.trim().is_empty()
                            && !draft.name.trim().is_empty()
                            && !draft.endpoint.trim().is_empty()
                            && !draft.package.trim().is_empty();
                        rsx! {
                            div { class: "imp-form", "data-form": "new-source",
                                div { class: "imp-summary-title", "New server source" }
                                div { class: "imp-note",
                                    "declares a catalog entry plus its package file; both land in the server's packages directory and are re-merged at boot"
                                }
                                if !switch_allowed {
                                    p { class: "imp-readonly", "data-field": "create-readonly",
                                        "read-only: adding a source requires runtime switching to be enabled for your group"
                                    }
                                }
                                label { class: "imp-field",
                                    span { "id" }
                                    input {
                                        r#type: "text",
                                        placeholder: "my-bank",
                                        value: "{draft.id}",
                                        oninput: move |e| NEW_SOURCE.write().id = e.value(),
                                    }
                                }
                                label { class: "imp-field",
                                    span { "name" }
                                    input {
                                        r#type: "text",
                                        placeholder: "My bank",
                                        value: "{draft.name}",
                                        oninput: move |e| NEW_SOURCE.write().name = e.value(),
                                    }
                                }
                                label { class: "imp-field",
                                    span { "endpoint" }
                                    input {
                                        r#type: "text",
                                        placeholder: "http://api.example.svc:8080",
                                        value: "{draft.endpoint}",
                                        oninput: move |e| NEW_SOURCE.write().endpoint = e.value(),
                                    }
                                }
                                label { class: "imp-field",
                                    span { "package file" }
                                    input {
                                        r#type: "text",
                                        placeholder: "my-importer.toml",
                                        value: "{draft.package}",
                                        oninput: move |e| NEW_SOURCE.write().package = e.value(),
                                    }
                                }
                                div { class: "imp-vars",
                                    span { class: "imp-note", "variables" }
                                    for row in 0..rows {
                                        div { key: "{row}", class: "imp-var",
                                            input {
                                                r#type: "text",
                                                placeholder: "key",
                                                value: "{draft.variables[row].0}",
                                                oninput: move |e| NEW_SOURCE.write().variables[row].0 = e.value(),
                                            }
                                            span { "=" }
                                            input {
                                                r#type: "text",
                                                placeholder: "value",
                                                value: "{draft.variables[row].1}",
                                                oninput: move |e| NEW_SOURCE.write().variables[row].1 = e.value(),
                                            }
                                            button {
                                                class: "btn imp-mini",
                                                r#type: "button",
                                                title: "Remove variable",
                                                disabled: rows == 1,
                                                onclick: move |_| { NEW_SOURCE.write().variables.remove(row); },
                                                "×"
                                            }
                                        }
                                    }
                                    button {
                                        class: "btn imp-mini",
                                        r#type: "button",
                                        onclick: move |_| {
                                            NEW_SOURCE.write().variables.push((String::new(), String::new()));
                                        },
                                        "+ variable"
                                    }
                                }
                            }
                            {manifest_editor("new-source".to_string(), editor_view, spliceable, false)}
                            div { class: "imp-actions",
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "create-source",
                                    disabled: !switch_allowed || !ready || busy || manifest_empty,
                                    onclick: move |_| create_catalog_source(),
                                    "Create"
                                }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    "data-action": "cancel-source",
                                    disabled: busy,
                                    onclick: move |_| {
                                        *SELECTION.write() = None;
                                        set_manifest(String::new());
                                        *ERROR.write() = None;
                                        *STATUS.write() = None;
                                    },
                                    "Cancel"
                                }
                            }
                        }
                    }
                }}
                if let Some(message) = &error {
                    div { class: "imp-error", role: "alert", "data-field": "write-error", "{message}" }
                }
            }

            // ── bottom: parse preview ──────────────────────────────────
            div { class: "imp-preview",
                {if selection.is_none() {
                    rsx! {
                        div { class: "imp-note", "preview runs against the selected package" }
                    }
                } else if manifest_empty {
                    rsx! {
                        div { class: "imp-note",
                            "load the server package, or duplicate the entry to a local package, to run parse previews"
                        }
                    }
                } else {
                    rsx! {
                        div { class: "imp-preview-input",
                            textarea {
                                class: "imp-sample",
                                "data-field": "sample-input",
                                placeholder: "sample input for the selected package…",
                                value: "{SAMPLE}",
                                oninput: move |e| {
                                    let v = e.value();
                                    if let Some(Selection::Local(id)) = SELECTION.peek().clone() {
                                        let _ = LocalStorage::set(sample_key(&id), &v);
                                    }
                                    *SAMPLE.write() = v;
                                },
                            }
                        }
                        div { class: "imp-preview-run",
                            button {
                                class: "btn",
                                r#type: "button",
                                "data-action": "parse-preview",
                                disabled: preview_running || manifest_empty,
                                onclick: run_preview,
                                if preview_running { "Parsing…" } else { "Parse preview" }
                            }
                            if let Some(status) = &status {
                                div { class: "imp-status", "data-field": "preview-status", "{status}" }
                            }
                            match &preview {
                                PreviewState::Idle => rsx! {},
                                PreviewState::Done(p) => {
                                    let preview_graph = p.clone();
                                    rsx! {
                                    div { class: "imp-preview-result", "data-outcome": "ok",
                                        span { class: "imp-counts",
                                            strong { "data-field": "preview-nodes", "{p.nodes}" }
                                            " nodes · "
                                            strong { "data-field": "preview-edges", "{p.edges}" }
                                            " edges"
                                        }
                                        if !p.unresolved.is_empty() {
                                            div { class: "imp-unresolved",
                                                span { class: "imp-counts", "unresolved:" }
                                                for id in &p.unresolved {
                                                    code { key: "{id}", class: "imp-unresolved-id", "{id}" }
                                                }
                                            }
                                        }
                                        if !p.sample_ids.is_empty() {
                                            div { class: "imp-samples",
                                                span { class: "imp-counts", "sample ids:" }
                                                for id in &p.sample_ids {
                                                    code { key: "{id}", class: "imp-sample-id", "{id}" }
                                                }
                                            }
                                        }
                                        if !p.node_ids.is_empty() {
                                            button {
                                                class: "btn imp-mini",
                                                r#type: "button",
                                                "data-action": "view-preview-graph",
                                                title: "mount the parsed sample as a client-side graph",
                                                onclick: move |_| {
                                                    let gd = graph_data_from_preview(&preview_graph);
                                                    *STATUS.write() = Some(format!(
                                                        "preview graph: {} nodes, {} edges — client-only (server tools disabled)",
                                                        gd.n_nodes, gd.n_edges
                                                    ));
                                                    crate::replace_with_client_graph(
                                                        ctx,
                                                        gd,
                                                        "pest preview",
                                                    );
                                                },
                                                "View as graph"
                                            }
                                        }
                                    }
                                }},
                                PreviewState::Failed { message, timeout } => rsx! {
                                    div {
                                        class: if *timeout { "imp-preview-error timeout" } else { "imp-preview-error" },
                                        role: "alert",
                                        "data-outcome": if *timeout { "timeout" } else { "error" },
                                        "data-field": "preview-error",
                                        "{message}"
                                        if *timeout {
                                            div { class: "imp-note",
                                                "the grammar hit the worker CPU limit and the worker was killed — the UI thread never blocked"
                                            }
                                        }
                                    }
                                },
                            }
                        }
                    }
                }}
            }
        }
    }
}

/// The manifest/grammar editor pair, shared by local packages, deployment
/// packages fetched from the server, and a `POST /importers` draft.
/// `read_only` is the server's posture: a package the viewer may read but not
/// write (unauthorized, or an immutable packages directory).
fn manifest_editor(
    editor_key: String,
    view: EditorView,
    spliceable: bool,
    read_only: bool,
) -> Element {
    rsx! {
        div { class: "imp-editor-tabs", role: "tablist",
            button {
                class: if view == EditorView::Manifest { "imp-tab active" } else { "imp-tab" },
                r#type: "button",
                role: "tab",
                "data-view": "manifest",
                aria_selected: if view == EditorView::Manifest { "true" } else { "false" },
                onclick: move |_| *EDITOR_VIEW.write() = EditorView::Manifest,
                "Manifest (TOML)"
            }
            button {
                class: if view == EditorView::Grammar { "imp-tab active" } else { "imp-tab" },
                r#type: "button",
                role: "tab",
                "data-view": "grammar",
                aria_selected: if view == EditorView::Grammar { "true" } else { "false" },
                onclick: move |_| *EDITOR_VIEW.write() = EditorView::Grammar,
                "Grammar (pest)"
            }
        }
        div { class: "imp-editor-host",
            match view {
                EditorView::Manifest => rsx! {
                    // One-way binding: the editor is the only writer while
                    // mounted; cross-view sync happens via remount (key) so
                    // no signal write can reenter the editor's on_change.
                    MonacoEditor {
                        key: "{editor_key}",
                        initial: MANIFEST.peek().clone(),
                        language: "toml".to_string(),
                        theme: Some(PANEL_KIT_DARK_THEME.to_string()),
                        read_only: read_only,
                        on_change: move |text: String| set_manifest(text),
                    }
                },
                EditorView::Grammar => rsx! {
                    if spliceable {
                        MonacoEditor {
                            key: "{editor_key}",
                            initial: GRAMMAR.peek().clone(),
                            language: "pest".to_string(),
                            theme: Some(PANEL_KIT_DARK_THEME.to_string()),
                            read_only: read_only,
                            on_change: move |text: String| {
                                // The peek guard must drop before the arms
                                // write MANIFEST — a scrutinee temporary
                                // lives for the whole match.
                                let current = MANIFEST.peek().clone();
                                match splice_grammar(&current, &text) {
                                    Ok(manifest) => set_manifest_from_grammar(manifest),
                                    Err(e) => *STATUS.write() = Some(e),
                                }
                            },
                        }
                    } else {
                        div { class: "imp-note", "data-field": "grammar-readonly",
                            "no spliceable grammar = ''' literal under [parser] — edit the manifest TOML directly (json-engine packages have no pest grammar)"
                        }
                    }
                },
            }
        }
    }
}

/// Whether the catalog's runtime switch posture allows session-scoped source
/// selection (mirrors the retired Settings → Importers gating).
fn catalog_runtime_switch(catalog: &CatalogState) -> bool {
    match catalog {
        CatalogState::Ready(c) => c.runtime_switch.enabled && c.runtime_switch.allowed,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_extract_splice_roundtrip() {
        let manifest = template_manifest("custom.demo", "Demo");
        let grammar = extract_grammar(&manifest).expect("template grammar is spliceable");
        assert!(grammar.contains("document = { SOI"));
        let edited = grammar.replacen("document", "top", 1);
        let spliced = splice_grammar(&manifest, &edited).expect("splice succeeds");
        assert_eq!(extract_grammar(&spliced).as_deref(), Some(edited.as_str()));
        assert!(spliced.contains("root_rule = \"document\""));
        assert!(spliced.contains("[parser.captures]"));
        assert_eq!(spliced.ends_with('\n'), manifest.ends_with('\n'));
    }

    #[test]
    fn splice_rejects_terminator_line_and_missing_grammar() {
        let manifest = template_manifest("custom.demo", "Demo");
        assert!(splice_grammar(&manifest, "a = { '''\n").is_err());
        assert!(splice_grammar("[parser]\nengine = \"json\"\n", "x").is_err());
        assert!(extract_grammar("[parser]\nengine = \"json\"\n").is_none());
    }

    #[test]
    fn rewrite_metadata_id_scoped_to_metadata_table() {
        let manifest = template_manifest("custom.demo", "Demo");
        let rewritten = rewrite_metadata_id(&manifest, "custom.copy");
        assert!(rewritten.contains("[metadata]\nid = \"custom.copy\""));
        // [parser.captures] id binding stays untouched.
        assert!(rewritten.contains("id = \"node_id\""));
    }

    #[test]
    fn building_errors_are_detected_from_the_error_string() {
        assert!(api::is_building_error(
            "/graph/init -> HTTP 503: building importer source \"hindsight-memory-bank\": the import is running"
        ));
        // retryable-by-polling, must surface as an error.
        assert!(!api::is_building_error(
            "/graph/init -> HTTP 503: import alternate source \"x\": connector unreachable"
        ));
        assert!(!api::is_building_error("/graph/init -> HTTP 404"));
    }
}
