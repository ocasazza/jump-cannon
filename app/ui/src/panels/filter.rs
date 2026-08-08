//! Filter panel — a nested, directly manipulable Boolean outline backed by
//! `jump-cannon-filter-model`, plus the inverted index used for field facets.
//!
//! Three layers, top to bottom:
//!   - an accessible logic outline with repeatable search/field leaves,
//!     nested ALL/ANY groups, keyboard reorder controls, and pointer drag/drop;
//!   - a contextual field/value palette decoded from
//!     `GET /graph/meta_summary`, with live bucket counts;
//!   - a last-valid evaluator that reports node-keyed diagnostics and subtree
//!     counts before dispatching Filter/Dim masks to the GPU.
//!
//! The public compatibility projection remains available to Inspector and the
//! command palette. User-facing settings continue to persist under
//! `jc_filter_v1` so v1 state can be upgraded in place.

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use futures::future::join_all;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::api::get_proto;
use crate::{proto, render, Ctx};

use jump_cannon_filter_model::{
    self as filter_model, Clause, Diagnostic, EvalOutput, GroupMode, NodeId, Rule, RuleGroup,
    RuleKind, SearchMatches,
};
pub(crate) use jump_cannon_filter_model::{
    Card, ConnectorOp, Op, QueryModel,
};

// --- filter behavior (port of ui/state.rs::FilterBehavior) ----------------------

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum FilterBehavior {
    #[default]
    Filter,
    Focus,
}

impl FilterBehavior {
    /// The Focus variant is presented as "Dim" throughout the UI so "Focus" is
    /// left to mean only the Camera panel's focus mode; the variant name stays
    /// Focus for the persisted serde tag.
    pub(crate) fn tooltip(self) -> &'static str {
        match self {
            FilterBehavior::Filter => "Hide non-matching nodes and the edges that touch them.",
            FilterBehavior::Focus => "Keep non-matches on screen but dim them to ~25% alpha.",
        }
    }
    pub(crate) fn toggled(self) -> Self {
        match self {
            FilterBehavior::Filter => FilterBehavior::Focus,
            FilterBehavior::Focus => FilterBehavior::Filter,
        }
    }
}

// --- inverted index (port of ui/field_index.rs) ----------------------------------

#[derive(Debug, Default, Clone)]
pub(crate) struct FieldIndex {
    /// field -> value -> sorted `Vec<u32>` of node indices.
    pub by_field: HashMap<String, HashMap<String, Vec<u32>>>,
}

impl FieldIndex {
    /// Decode a [`proto::MetaSummary`] into a FieldIndex. The server already
    /// produces dense node indices, so no id remapping is needed.
    pub(crate) fn from_proto(p: &proto::MetaSummary) -> Self {
        let mut by_field: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
        for b in &p.buckets {
            let field = match p.fields.get(b.field_idx as usize) {
                Some(s) => s.clone(),
                None => continue,
            };
            let mut v = b.node_idx.clone();
            v.sort_unstable();
            v.dedup();
            by_field
                .entry(field)
                .or_default()
                .insert(b.value.clone(), v);
        }
        Self { by_field }
    }

    /// Per-node categorical f32 metric: each node's value is the bucket id
    /// of its primary tag (first tag in case-sensitive sorted order — the
    /// deterministic tiebreaker for multi-tagged nodes). Bucket ids are
    /// `hash(tag) as u32`; untagged nodes get `0`. `None` when no node
    /// carries any tag. Ported for the Style panel's tag-color path.
    #[allow(dead_code)] // consumer lands with the Style panel port
    pub(crate) fn tag_primary_metric(&self, n_nodes: usize) -> Option<Vec<f32>> {
        let tags = self.by_field.get("tags")?;
        // Walk the (value -> [node_idx]) buckets in sorted value order so
        // the first bucket that claims a node defines its primary tag.
        let mut sorted: Vec<(&String, &Vec<u32>)> = tags.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = vec![0.0_f32; n_nodes];
        let mut assigned = vec![false; n_nodes];
        for (value, idxs) in sorted {
            // Default Hasher is non-portable across rustc versions but
            // stable within a process run — fine, never persisted.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            value.hash(&mut h);
            let bucket = (h.finish() as u32) as f32;
            for &i in idxs {
                let i = i as usize;
                if i < n_nodes && !assigned[i] {
                    out[i] = bucket;
                    assigned[i] = true;
                }
            }
        }
        Some(out)
    }
}

// --- shared panel state -----------------------------------------------------------

const STORE_KEY: &str = "jc_filter_v1";

/// localStorage shape — the egui app gets this for free via eframe Storage.
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    query: QueryModel,
    #[serde(default)]
    behavior: FilterBehavior,
}

fn load() -> Persisted {
    let mut persisted: Persisted = LocalStorage::get(STORE_KEY).unwrap_or_default();
    persisted.query.normalize_imported();
    persisted
}

pub(crate) static QUERY: GlobalSignal<QueryModel> = Signal::global(|| load().query);
pub(crate) static BEHAVIOR: GlobalSignal<FilterBehavior> = Signal::global(|| load().behavior);
/// `None` = fetch in flight (or not started); `Some(Err)` = fetch failed.
pub(crate) static FIELD_INDEX: GlobalSignal<Option<Result<FieldIndex, String>>> =
    Signal::global(|| None);
static FIELD_INDEX_STARTED: GlobalSignal<bool> = Signal::global(|| false);
static FIELD_INDEX_SESSION: GlobalSignal<u64> = Signal::global(|| 0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EvalPhase {
    #[default]
    Idle,
    Checking,
    Applied,
    Invalid,
}

#[derive(Debug, Clone, Default)]
struct EvaluationState {
    generation: u64,
    applied_version: u64,
    phase: EvalPhase,
    matches: Option<HashSet<u32>>,
    counts: HashMap<NodeId, usize>,
    diagnostics: Vec<Diagnostic>,
}

static EVALUATION: GlobalSignal<EvaluationState> = Signal::global(EvaluationState::default);

fn persist() {
    let p = Persisted {
        query: QUERY.peek().clone(),
        behavior: *BEHAVIOR.peek(),
    };
    let _ = LocalStorage::set(STORE_KEY, &p);
}

/// AppState round-trip seam (`crate::appstate`): the live query model +
/// behavior (egui's `query` / `filter_behavior` AppState fields).
pub(crate) fn state_snapshot() -> (QueryModel, FilterBehavior) {
    (QUERY.read().clone(), *BEHAVIOR.read())
}

/// AppState round-trip seam: write the imported filter state straight to
/// localStorage; the apply path's reload re-seeds the signals.
pub(crate) fn state_restore(query: &QueryModel, behavior: FilterBehavior) {
    let mut query = query.clone();
    query.normalize_imported();
    let _ = LocalStorage::set(STORE_KEY, &Persisted { query, behavior });
}

/// One-shot `/graph/meta_summary` fetch — called from both panels' render
/// paths so whichever opens first arms it (the egui app fetches at boot).
pub(crate) fn ensure_field_index(ctx: Ctx) {
    if !ctx.graph_session.peek().is_server_backed() {
        return;
    }
    if *FIELD_INDEX_STARTED.peek() {
        return;
    }
    *FIELD_INDEX_STARTED.write() = true;
    let session = *FIELD_INDEX_SESSION.peek();
    spawn(async move {
        let fetched = get_proto::<proto::MetaSummary>("/graph/meta_summary").await;
        if *FIELD_INDEX_SESSION.peek() != session {
            return;
        }
        match fetched {
            Ok(m) => {
                tracing::info!(
                    "[filter] meta_summary: {} fields, {} buckets",
                    m.fields.len(),
                    m.buckets.len()
                );
                *FIELD_INDEX.write() = Some(Ok(FieldIndex::from_proto(&m)));
                // Persisted filters can resolve now — re-push the mask.
                sync_gpu();
            }
            Err(e) => {
                tracing::warn!("[filter] meta_summary fetch failed: {e}");
                *FIELD_INDEX.write() = Some(Err(e));
            }
        }
    });
}

/// Drop index-derived state whenever dense node indices may have changed.
/// The user's persisted query remains available when a new server graph is
/// loaded, but no stale mask is allowed to survive the transition.
pub(crate) fn reset_for_graph_session() {
    let next = FIELD_INDEX_SESSION.peek().wrapping_add(1);
    *FIELD_INDEX_SESSION.write() = next;
    *FIELD_INDEX_STARTED.write() = false;
    *FIELD_INDEX.write() = None;
    let next_generation = EVALUATION.peek().generation.wrapping_add(1);
    *EVALUATION.write() = EvaluationState {
        generation: next_generation,
        ..EvaluationState::default()
    };
    render::with_host(|h| {
        let (pipes, queue) = h.pipes_and_queue();
        pipes.set_filter_mask(queue, None);
        pipes.set_focus_set(queue, None, &HashSet::new());
    });
}

/// Evaluate every query edit from the app root, including mutations made by
/// Inspector badges or command-palette actions while this panel is closed.
/// Invalid/pending drafts deliberately retain the last applied match set.
pub(crate) fn use_query_evaluator(ctx: Ctx) {
    use_effect(move || {
        let query = QUERY.read().clone();
        let graph_session = ctx.graph_session.read().clone();
        let graph = ctx.graph.read();
        let Some(graph) = graph.as_ref() else {
            return;
        };
        let node_count = graph.ids.len();
        let graph_revision = graph.graph_revision;
        let server_backed = graph_session.is_server_backed();

        if server_backed && FIELD_INDEX.read().is_none() {
            ensure_field_index(ctx);
            let mut state = EVALUATION.write();
            state.phase = EvalPhase::Checking;
            return;
        }

        let field_index = FIELD_INDEX
            .read()
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .cloned()
            .unwrap_or_default();
        let diagnostics =
            filter_model::validate(query.root(), &field_index.by_field, server_backed);
        let generation = EVALUATION.peek().generation.wrapping_add(1);
        {
            let mut state = EVALUATION.write();
            state.generation = generation;
            state.diagnostics = diagnostics.clone();
            if !diagnostics.is_empty() {
                state.phase = EvalPhase::Invalid;
            }
        }
        if !diagnostics.is_empty() {
            return;
        }

        if query.is_empty() {
            apply_evaluation(generation, EvalOutput::default());
            return;
        }

        let searches = filter_model::search_rules(query.root());
        if searches.is_empty() {
            let output = filter_model::evaluate(
                query.root(),
                &field_index.by_field,
                &SearchMatches::new(),
                node_count,
            );
            apply_evaluation(generation, output);
            return;
        }

        EVALUATION.write().phase = EvalPhase::Checking;
        let query_root = query.root().clone();
        let facets = field_index.by_field;
        let session_epoch = graph_session.epoch;
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(220).await;
            if EVALUATION.peek().generation != generation {
                return;
            }
            // `/search/matches` is the set-valued primitive: no client limit,
            // so a rule matching more nodes than the ranked endpoint's cap
            // still hides exactly the non-matching nodes.
            let requests = searches.into_iter().map(|(rule_id, query)| async move {
                let response = crate::api::search_matches(&query).await;
                (rule_id, response)
            });
            let mut matches = SearchMatches::new();
            let mut errors = Vec::new();
            // The indices are dense positions inside the snapshot that served
            // them, so they are only comparable when every rule — and the graph
            // on screen — came from that same revision.
            let mut served_revision = None;
            for (rule_id, response) in join_all(requests).await {
                match response {
                    Ok(response) => {
                        if *served_revision.get_or_insert(response.revision) != response.revision {
                            return;
                        }
                        matches.insert(rule_id, response.value.into_iter().collect());
                    }
                    Err(error) => errors.push(Diagnostic {
                        node_id: rule_id,
                        message: error,
                    }),
                }
            }
            if EVALUATION.peek().generation != generation
                || ctx.graph_session.peek().epoch != session_epoch
                || ctx
                    .graph
                    .peek()
                    .as_ref()
                    .and_then(|graph| graph.graph_revision)
                    != graph_revision
                || served_revision.is_some_and(|served| graph_revision != Some(served))
            {
                return;
            }
            if !errors.is_empty() {
                let mut state = EVALUATION.write();
                state.phase = EvalPhase::Invalid;
                state.diagnostics = errors;
                return;
            }
            let output = filter_model::evaluate(&query_root, &facets, &matches, node_count);
            apply_evaluation(generation, output);
        });
    });
}

fn apply_evaluation(generation: u64, output: EvalOutput) {
    if EVALUATION.peek().generation != generation {
        return;
    }
    {
        let mut state = EVALUATION.write();
        state.phase = if output.matches.is_some() {
            EvalPhase::Applied
        } else {
            EvalPhase::Idle
        };
        state.matches = output.matches;
        state.counts = output.counts;
        state.diagnostics.clear();
        state.applied_version = state.applied_version.wrapping_add(1);
    }
    sync_gpu();
}

pub(crate) fn current_matches() -> Option<HashSet<u32>> {
    EVALUATION.peek().matches.clone()
}

pub(crate) fn applied_version() -> u64 {
    EVALUATION.peek().applied_version
}

/// Push the last valid expression result into the renderer. `Some(empty)` is
/// intentionally distinct from no filter: a valid zero-result query hides or
/// dims every node and the panel explains why.
pub(crate) fn sync_gpu() {
    let matching = current_matches();
    let behavior = *BEHAVIOR.peek();
    render::with_host(|h| {
        let (pipes, queue) = h.pipes_and_queue();
        match behavior {
            FilterBehavior::Filter => {
                // Reset the focus dim mask, then push the hard filter mask.
                pipes.set_focus_set(queue, None, &HashSet::new());
                pipes.set_filter_mask(queue, matching.as_ref());
            }
            FilterBehavior::Focus => {
                pipes.set_filter_mask(queue, None);
                pipes.set_filter_focus_set(queue, matching.as_ref());
            }
        }
    });
}

/// Canonical expression mutation. Compatibility callers may still append a
/// v1 `Card`; absorb it as an active leaf before persisting.
pub(crate) fn edit_filters(f: impl FnOnce(&mut QueryModel)) {
    crate::appstate::note_source("Filter");
    {
        let mut query = QUERY.write();
        f(&mut query);
        query.absorb_appended_cards();
    }
    persist();
}

pub(crate) fn toggle_behavior() {
    crate::appstate::note_source("Filter");
    let next = BEHAVIOR.peek().toggled();
    *BEHAVIOR.write() = next;
    persist();
    sync_gpu();
}

// --- panel -------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FieldChoice {
    name: String,
    node_count: usize,
    values: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropIntent {
    Before(NodeId),
    Inside(NodeId),
}

#[derive(Clone, Copy)]
struct EditorSignals {
    active_palette: Signal<Option<NodeId>>,
    drag_source: Signal<Option<NodeId>>,
    drag_target: Signal<Option<DropIntent>>,
}

fn field_catalog(index: &FieldIndex) -> Vec<FieldChoice> {
    let mut fields: Vec<FieldChoice> = index
        .by_field
        .iter()
        .map(|(name, buckets)| {
            let mut nodes = HashSet::new();
            let mut values: Vec<(String, usize)> = buckets
                .iter()
                .map(|(value, indices)| {
                    nodes.extend(indices.iter().copied());
                    (value.clone(), indices.len())
                })
                .collect();
            values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            FieldChoice {
                name: name.clone(),
                node_count: nodes.len(),
                values,
            }
        })
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

fn phase_name(phase: EvalPhase) -> &'static str {
    match phase {
        EvalPhase::Idle => "idle",
        EvalPhase::Checking => "checking",
        EvalPhase::Applied => "applied",
        EvalPhase::Invalid => "invalid",
    }
}

fn group_mode_name(mode: GroupMode) -> &'static str {
    match mode {
        GroupMode::All => "all",
        GroupMode::Any => "any",
    }
}

fn group_connector(mode: GroupMode) -> &'static str {
    match mode {
        GroupMode::All => "AND",
        GroupMode::Any => "OR",
    }
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Eq => "is",
        Op::Neq => "is not",
        Op::Contains => "contains",
        Op::Matches => "matches regex",
    }
}

fn op_token(op: Op) -> &'static str {
    match op {
        Op::Eq => "eq",
        Op::Neq => "neq",
        Op::Contains => "contains",
        Op::Matches => "matches",
    }
}

fn parse_op(value: &str) -> Op {
    match value {
        "neq" => Op::Neq,
        "contains" => Op::Contains,
        "matches" => Op::Matches,
        _ => Op::Eq,
    }
}

pub fn panel(ctx: Ctx) -> Element {
    let server_backed = ctx.graph_session.read().is_server_backed();
    ensure_field_index(ctx);
    crate::appstate::ensure_init();

    let mut active_palette = use_signal(|| None::<NodeId>);
    let drag_source = use_signal(|| None::<NodeId>);
    let drag_target = use_signal(|| None::<DropIntent>);
    let signals = EditorSignals {
        active_palette,
        drag_source,
        drag_target,
    };
    let drag_source_now = *drag_source.read();
    let drag_target_now = *drag_target.read();

    let q = QUERY.read().clone();
    let evaluation = EVALUATION.read().clone();
    let node_count = ctx
        .graph
        .read()
        .as_ref()
        .map(|graph| graph.ids.len())
        .unwrap_or_default();
    let index_snapshot = FIELD_INDEX.read().clone();
    let catalog = index_snapshot
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(field_catalog)
        .unwrap_or_default();
    let index_note = match index_snapshot {
        None if server_backed => Some("Loading filterable fields…".to_string()),
        Some(Err(error)) => Some(format!("Field index unavailable: {error}")),
        _ => None,
    };

    let mut diagnostics: HashMap<NodeId, Vec<String>> = HashMap::new();
    for diagnostic in &evaluation.diagnostics {
        diagnostics
            .entry(diagnostic.node_id)
            .or_default()
            .push(diagnostic.message.clone());
    }

    let last_valid = evaluation.matches.as_ref().map(HashSet::len);
    // "1 node" / "2 nodes" — the count is user-facing copy, not a raw number.
    let nodes_phrase = |count: usize| {
        format!("{count} {}", if count == 1 { "node" } else { "nodes" })
    };
    let (status_title, status_detail) = match evaluation.phase {
        EvalPhase::Idle => (
            format!("All {node_count} nodes"),
            "No active expression".to_string(),
        ),
        EvalPhase::Checking => (
            "Checking changes…".to_string(),
            last_valid
                .map(|count| format!("Still showing {}", nodes_phrase(count)))
                .unwrap_or_else(|| "Nothing applied yet".to_string()),
        ),
        EvalPhase::Applied => (
            format!("{} matching", nodes_phrase(last_valid.unwrap_or_default())),
            "Live".to_string(),
        ),
        EvalPhase::Invalid => {
            let issue_count = evaluation.diagnostics.len();
            (
                format!(
                    "{issue_count} {}",
                    if issue_count == 1 { "issue" } else { "issues" }
                ),
                last_valid
                    .map(|count| format!("Still showing {}", nodes_phrase(count)))
                    .unwrap_or_else(|| "Fix the outline to apply it".to_string()),
            )
        }
    };
    let status_class = format!("fil-status {}", phase_name(evaluation.phase));
    let status_phase = phase_name(evaluation.phase);
    let query_state = match evaluation.phase {
        EvalPhase::Idle => "idle",
        EvalPhase::Checking => "loading",
        EvalPhase::Applied => "valid",
        EvalPhase::Invalid => "invalid",
    };
    let last_valid_attr = last_valid
        .map(|count| count.to_string())
        .unwrap_or_default();
    let behavior = *BEHAVIOR.read();
    let filter_checked = if behavior == FilterBehavior::Filter {
        "true"
    } else {
        "false"
    };
    let dim_checked = if behavior == FilterBehavior::Focus {
        "true"
    } else {
        "false"
    };

    rsx! {
        div {
            class: "fil",
            "data-testid": "filter-builder",
            "data-filter-phase": "{status_phase}",
            "data-query-state": "{query_state}",
            "data-match-count": "{last_valid_attr}",
            if !server_backed {
                div { class: "fil-note warning", role: "note",
                    "Search and metadata fields require a server-hosted graph. Existing rules stay editable."
                }
            }
            if let Some(note) = index_note {
                div { class: "fil-note", role: "status", "{note}" }
            }

            header { class: "fil-toolbar",
                div {
                    class: "{status_class}",
                    role: "status",
                    "aria-live": "polite",
                    "data-testid": "filter-evaluation",
                    "data-phase": "{status_phase}",
                    "data-applied-count": "{last_valid_attr}",
                    "data-query-state": "{query_state}",
                    "data-match-count": "{last_valid_attr}",
                    span { class: "fil-status-dot", "aria-hidden": "true" }
                    span { class: "fil-status-copy",
                        strong { "{status_title}" }
                        small { "{status_detail}" }
                    }
                }
                div {
                    class: "fil-behavior",
                    role: "radiogroup",
                    "aria-label": "How non-matching nodes are displayed",
                    "data-testid": "filter-behavior",
                    button {
                        class: if behavior == FilterBehavior::Filter { "active" } else { "" },
                        role: "radio",
                        "aria-checked": "{filter_checked}",
                        title: FilterBehavior::Filter.tooltip(),
                        onclick: move |_| {
                            if *BEHAVIOR.peek() != FilterBehavior::Filter {
                                toggle_behavior();
                            }
                        },
                        "Filter"
                    }
                    button {
                        class: if behavior == FilterBehavior::Focus { "active" } else { "" },
                        role: "radio",
                        "aria-checked": "{dim_checked}",
                        title: FilterBehavior::Focus.tooltip(),
                        onclick: move |_| {
                            if *BEHAVIOR.peek() != FilterBehavior::Focus {
                                toggle_behavior();
                            }
                        },
                        "Dim"
                    }
                }
                button {
                    class: "fil-reset",
                    "data-testid": "filter-reset",
                    "aria-label": "Clear the filter expression",
                    onclick: move |_| {
                        *active_palette.write() = None;
                        edit_filters(QueryModel::clear);
                    },
                    "Clear all"
                }
            }

            div { class: "fil-intro",
                strong { "Build the logic as a sentence." }
                span { "ALL means every line must match; ANY means at least one. Nest a group when a phrase needs its own logic." }
            }

            section {
                class: "fil-expression",
                role: "tree",
                "aria-label": "Filter expression",
                "data-testid": "filter-expression",
                {group_el(
                    q.root(),
                    true,
                    0,
                    None,
                    &catalog,
                    &evaluation,
                    &diagnostics,
                    signals,
                    drag_source_now,
                    drag_target_now,
                )}
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn group_el(
    group: &RuleGroup,
    root: bool,
    depth: usize,
    parent_id: Option<NodeId>,
    catalog: &[FieldChoice],
    evaluation: &EvaluationState,
    diagnostics: &HashMap<NodeId, Vec<String>>,
    signals: EditorSignals,
    drag_source_now: Option<NodeId>,
    drag_target_now: Option<DropIntent>,
) -> Element {
    let id = group.id;
    let mode = group.mode;
    let enabled = group.enabled;
    let negated = group.negated;
    let parent_attr = parent_id.map(|id| id.to_string()).unwrap_or_default();
    let level = depth + 1;
    let enabled_attr = if enabled { "true" } else { "false" };
    let negated_attr = if negated { "true" } else { "false" };
    let mode_attr = group_mode_name(mode);
    let group_copy = match (mode, negated) {
        (GroupMode::All, false) => "Every line below must match",
        (GroupMode::Any, false) => "At least one line below must match",
        (GroupMode::All, true) => "Exclude nodes matching every line below",
        (GroupMode::Any, true) => "Exclude nodes matching any line below",
    };
    let mut class = if root {
        "fil-group root".to_string()
    } else {
        "fil-group nested".to_string()
    };
    if !enabled {
        class.push_str(" disabled");
    }
    if negated {
        class.push_str(" negated");
    }
    if drag_target_now == Some(DropIntent::Inside(id)) {
        class.push_str(" drop-inside");
    }
    let node_diagnostics = diagnostics.get(&id).cloned().unwrap_or_default();
    let has_diagnostics = !node_diagnostics.is_empty();
    let aria_invalid = if has_diagnostics { "true" } else { "false" };

    let mut drag_start = signals.drag_source;
    let mut drag_end_source = signals.drag_source;
    let mut drag_end_target = signals.drag_target;
    let drag_for_inside = signals.drag_source;
    let mut target_for_inside = signals.drag_target;
    let drop_source = signals.drag_source;
    let mut drop_target = signals.drag_target;
    let mut add_field_palette = signals.active_palette;
    let field_add_disabled = catalog.is_empty();
    let field_add_disabled_attr = if field_add_disabled { "true" } else { "false" };

    rsx! {
        section {
            class: "{class}",
            role: "treeitem",
            "aria-level": "{level}",
            "aria-expanded": "true",
            "aria-disabled": if enabled { "false" } else { "true" },
            "aria-invalid": "{aria_invalid}",
            "data-testid": "filter-group",
            "data-group-id": "{id}",
            "data-expression": "group",
            "data-expression-id": "{id}",
            "data-expression-kind": "group",
            "data-expression-depth": "{depth}",
            "data-expression-parent": "{parent_attr}",
            "data-expression-enabled": "{enabled_attr}",
            "data-expression-negated": "{negated_attr}",
            "data-expression-mode": "{mode_attr}",
            "data-mode": "{mode_attr}",
            header { class: "fil-group-head",
                if !root {
                    button {
                        class: "fil-grab",
                        draggable: "true",
                        title: "Drag this group to reorder or nest it",
                        "aria-label": "Drag group {id}",
                        "aria-grabbed": if drag_source_now == Some(id) { "true" } else { "false" },
                        ondragstart: move |event| {
                            event.stop_propagation();
                            *drag_start.write() = Some(id);
                        },
                        ondragend: move |_| {
                            *drag_end_source.write() = None;
                            *drag_end_target.write() = None;
                        },
                        "⠿"
                    }
                }
                div { class: "fil-group-title",
                    span { class: "fil-eyebrow", if root { "Root logic" } else { "Nested logic" } }
                    div {
                        class: "fil-mode",
                        role: "group",
                        "aria-label": "Choose whether all or any rules in this group must match",
                        span { "Match" }
                        button {
                            class: if mode == GroupMode::All { "active" } else { "" },
                            "aria-pressed": if mode == GroupMode::All { "true" } else { "false" },
                            "data-testid": "filter-group-mode",
                            "data-mode-target": "all",
                            onclick: move |_| edit_filters(|query| {
                                if let Some(group) = query.group_mut(id) {
                                    group.mode = GroupMode::All;
                                }
                            }),
                            "ALL"
                        }
                        button {
                            class: if mode == GroupMode::Any { "active" } else { "" },
                            "aria-pressed": if mode == GroupMode::Any { "true" } else { "false" },
                            "data-testid": "filter-group-mode",
                            "data-mode-target": "any",
                            onclick: move |_| edit_filters(|query| {
                                if let Some(group) = query.group_mut(id) {
                                    group.mode = GroupMode::Any;
                                }
                            }),
                            "ANY"
                        }
                    }
                    small { "{group_copy}" }
                }
                {count_el(id, enabled, evaluation)}
                div { class: "fil-actions", role: "group", "aria-label": "Group actions",
                    button {
                        class: if enabled { "fil-state active" } else { "fil-state" },
                        title: if enabled { "Turn this group off without deleting it" } else { "Turn this group back on" },
                        "aria-label": if enabled { "Disable group" } else { "Enable group" },
                        "aria-pressed": "{enabled_attr}",
                        onclick: move |_| toggle_enabled(id),
                        if enabled { "On" } else { "Off" }
                    }
                    button {
                        class: if negated { "fil-not active" } else { "fil-not" },
                        title: "Invert the result of this whole group",
                        "aria-label": "Negate group",
                        "aria-pressed": "{negated_attr}",
                        onclick: move |_| toggle_negated(id),
                        "NOT"
                    }
                    if !root {
                        button {
                            class: "fil-icon",
                            title: "Move group up",
                            "aria-label": "Move group {id} up",
                            onclick: move |_| edit_filters(|query| { query.move_by(id, -1); }),
                            "↑"
                        }
                        button {
                            class: "fil-icon",
                            title: "Move group down",
                            "aria-label": "Move group {id} down",
                            onclick: move |_| edit_filters(|query| { query.move_by(id, 1); }),
                            "↓"
                        }
                        button {
                            class: "fil-icon danger",
                            title: "Delete group and its rules",
                            "aria-label": "Delete group {id}",
                            onclick: move |_| edit_filters(|query| { query.remove(id); }),
                            "×"
                        }
                    }
                }
            }
            for message in node_diagnostics {
                p {
                    class: "fil-diagnostic",
                    role: "alert",
                    "data-testid": "filter-diagnostic",
                    "data-diagnostic-node-id": "{id}",
                    "{message}"
                }
            }
            div {
                class: "fil-group-children",
                role: "group",
                "data-testid": "filter-group-children",
                "data-expression-group-id": "{id}",
                "data-drop-target": "group",
                ondragover: move |event| {
                    if drag_for_inside.peek().is_some() {
                        event.prevent_default();
                        event.stop_propagation();
                        if *target_for_inside.peek() != Some(DropIntent::Inside(id)) {
                            *target_for_inside.write() = Some(DropIntent::Inside(id));
                        }
                    }
                },
                ondrop: move |event| {
                    event.prevent_default();
                    event.stop_propagation();
                    if let Some(source_id) = *drop_source.peek() {
                        edit_filters(|query| { query.move_to_group(source_id, id); });
                    }
                    *drop_target.write() = None;
                },
                if group.children.is_empty() {
                    div { class: "fil-empty-drop",
                        strong { "No rules yet" }
                        span { "Add a line below, or drop one here." }
                    }
                }
                for (index, child) in group.children.iter().enumerate() {
                    {clause_el(
                        child,
                        index,
                        group.children.len(),
                        mode,
                        depth + 1,
                        id,
                        catalog,
                        evaluation,
                        diagnostics,
                        signals,
                        drag_source_now,
                        drag_target_now,
                    )}
                }
                div { class: "fil-add", role: "group", "aria-label": "Add to this group",
                    button {
                        "data-testid": "filter-add-search",
                        "data-expression-target": "{id}",
                        onclick: move |_| edit_filters(|query| { query.add_search(id); }),
                        span { "＋" }
                        "Search"
                    }
                    button {
                        "data-testid": "filter-add-field",
                        "data-expression-target": "{id}",
                        disabled: field_add_disabled,
                        "aria-disabled": "{field_add_disabled_attr}",
                        title: if field_add_disabled { "Filterable fields are still loading or unavailable" } else { "Add a field rule" },
                        onclick: move |_| {
                            let mut added = None;
                            edit_filters(|query| {
                                added = query.add_field(id, "");
                            });
                            if let Some(rule_id) = added {
                                *add_field_palette.write() = Some(rule_id);
                            }
                        },
                        span { "＋" }
                        "Field"
                    }
                    button {
                        "data-testid": "filter-add-group",
                        "data-expression-target": "{id}",
                        onclick: move |_| edit_filters(|query| { query.add_group(id); }),
                        span { "＋" }
                        "Group"
                    }
                    span { class: "fil-drop-hint", "Drop here to move into this group" }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn clause_el(
    clause: &Clause,
    index: usize,
    sibling_count: usize,
    parent_mode: GroupMode,
    depth: usize,
    parent_id: NodeId,
    catalog: &[FieldChoice],
    evaluation: &EvaluationState,
    diagnostics: &HashMap<NodeId, Vec<String>>,
    signals: EditorSignals,
    drag_source_now: Option<NodeId>,
    drag_target_now: Option<DropIntent>,
) -> Element {
    let id = clause.id();
    let connector = group_connector(parent_mode);
    let mut class = "fil-clause".to_string();
    if drag_target_now == Some(DropIntent::Before(id)) {
        class.push_str(" drop-before");
    }
    let drag_for_before = signals.drag_source;
    let mut target_for_before = signals.drag_target;
    let drop_source = signals.drag_source;
    let mut drop_target = signals.drag_target;
    rsx! {
        div {
            key: "{id}",
            class: "{class}",
            "data-drop-before-id": "{id}",
            ondragover: move |event| {
                if drag_for_before.peek().is_some() {
                    event.prevent_default();
                    event.stop_propagation();
                    if *target_for_before.peek() != Some(DropIntent::Before(id)) {
                        *target_for_before.write() = Some(DropIntent::Before(id));
                    }
                }
            },
            ondrop: move |event| {
                event.prevent_default();
                event.stop_propagation();
                if let Some(source_id) = *drop_source.peek() {
                    edit_filters(|query| { query.move_before(source_id, id); });
                }
                *drop_target.write() = None;
            },
            div { class: if index == 0 { "fil-junction first" } else { "fil-junction" },
                if index > 0 {
                    span {
                        title: if parent_mode == GroupMode::All {
                            "Both this line and its siblings must match"
                        } else {
                            "This line or one of its siblings must match"
                        },
                        "{connector}"
                    }
                } else if sibling_count > 1 {
                    span { class: "sr-only", "First of {sibling_count} lines" }
                }
            }
            match clause {
                Clause::Rule(rule) => rule_el(
                    rule,
                    depth,
                    parent_id,
                    catalog,
                    evaluation,
                    diagnostics,
                    signals,
                    drag_source_now,
                ),
                Clause::Group(group) => group_el(
                    group,
                    false,
                    depth,
                    Some(parent_id),
                    catalog,
                    evaluation,
                    diagnostics,
                    signals,
                    drag_source_now,
                    drag_target_now,
                ),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rule_el(
    rule: &Rule,
    depth: usize,
    parent_id: NodeId,
    catalog: &[FieldChoice],
    evaluation: &EvaluationState,
    diagnostics: &HashMap<NodeId, Vec<String>>,
    signals: EditorSignals,
    drag_source_now: Option<NodeId>,
) -> Element {
    let id = rule.id;
    let enabled = rule.enabled;
    let negated = rule.negated;
    let level = depth + 1;
    let enabled_attr = if enabled { "true" } else { "false" };
    let negated_attr = if negated { "true" } else { "false" };
    let kind_attr = match rule.kind {
        RuleKind::Search { .. } => "search",
        RuleKind::Field { .. } => "field",
    };
    let node_diagnostics = diagnostics.get(&id).cloned().unwrap_or_default();
    let has_diagnostics = !node_diagnostics.is_empty();
    let aria_invalid = if has_diagnostics { "true" } else { "false" };
    let mut class = "fil-rule".to_string();
    if !enabled {
        class.push_str(" disabled");
    }
    if negated {
        class.push_str(" negated");
    }
    if has_diagnostics {
        class.push_str(" invalid");
    }
    let mut drag_start = signals.drag_source;
    let mut drag_end_source = signals.drag_source;
    let mut drag_end_target = signals.drag_target;
    let mut palette_on_delete = signals.active_palette;

    rsx! {
        article {
            class: "{class}",
            role: "treeitem",
            "aria-level": "{level}",
            "aria-disabled": if enabled { "false" } else { "true" },
            "aria-invalid": "{aria_invalid}",
            "data-testid": "filter-rule",
            "data-rule-id": "{id}",
            "data-rule-kind": "{kind_attr}",
            "data-expression": "{kind_attr}",
            "data-expression-id": "{id}",
            "data-expression-kind": "{kind_attr}",
            "data-expression-depth": "{depth}",
            "data-expression-parent": "{parent_id}",
            "data-expression-enabled": "{enabled_attr}",
            "data-expression-negated": "{negated_attr}",
            div { class: "fil-rule-main",
                button {
                    class: "fil-grab",
                    draggable: "true",
                    title: "Drag this rule to reorder or move it into another group",
                    "aria-label": "Drag {kind_attr} rule {id}",
                    "aria-grabbed": if drag_source_now == Some(id) { "true" } else { "false" },
                    ondragstart: move |event| {
                        event.stop_propagation();
                        *drag_start.write() = Some(id);
                    },
                    ondragend: move |_| {
                        *drag_end_source.write() = None;
                        *drag_end_target.write() = None;
                    },
                    "⠿"
                }
                button {
                    class: if enabled { "fil-state active" } else { "fil-state" },
                    title: if enabled { "Turn this rule off without deleting it" } else { "Turn this rule back on" },
                    "aria-label": if enabled { "Disable rule" } else { "Enable rule" },
                    "aria-pressed": "{enabled_attr}",
                    onclick: move |_| toggle_enabled(id),
                    if enabled { "On" } else { "Off" }
                }
                span { class: "fil-rule-kind", "{kind_attr}" }
                div { class: "fil-rule-editor",
                    {rule_editor(rule, catalog, has_diagnostics, signals)}
                }
                {count_el(id, enabled, evaluation)}
                div { class: "fil-actions", role: "group", "aria-label": "Rule actions",
                    button {
                        class: if negated { "fil-not active" } else { "fil-not" },
                        title: "Invert this rule",
                        "aria-label": "Negate rule",
                        "aria-pressed": "{negated_attr}",
                        onclick: move |_| toggle_negated(id),
                        "NOT"
                    }
                    button {
                        class: "fil-icon",
                        title: "Move rule up",
                        "aria-label": "Move rule {id} up",
                        onclick: move |_| edit_filters(|query| { query.move_by(id, -1); }),
                        "↑"
                    }
                    button {
                        class: "fil-icon",
                        title: "Move rule down",
                        "aria-label": "Move rule {id} down",
                        onclick: move |_| edit_filters(|query| { query.move_by(id, 1); }),
                        "↓"
                    }
                    button {
                        class: "fil-icon danger",
                        title: "Delete rule",
                        "aria-label": "Delete rule {id}",
                        onclick: move |_| {
                            if *palette_on_delete.peek() == Some(id) {
                                *palette_on_delete.write() = None;
                            }
                            edit_filters(|query| { query.remove(id); });
                        },
                        "×"
                    }
                }
            }
            for message in node_diagnostics {
                p {
                    class: "fil-diagnostic",
                    role: "alert",
                    "data-testid": "filter-diagnostic",
                    "data-diagnostic-node-id": "{id}",
                    "{message}"
                }
            }
            if let RuleKind::Field { field, op, value } = &rule.kind {
                if *signals.active_palette.read() == Some(id) {
                    {field_palette(id, field, *op, value, catalog, signals.active_palette)}
                }
            }
        }
    }
}

fn rule_editor(
    rule: &Rule,
    catalog: &[FieldChoice],
    invalid: bool,
    signals: EditorSignals,
) -> Element {
    let id = rule.id;
    let aria_invalid = if invalid { "true" } else { "false" };
    match &rule.kind {
        RuleKind::Search { query } => {
            let mut active_palette = signals.active_palette;
            rsx! {
                label { class: "sr-only", r#for: "fil-search-{id}", "Search query" }
                input {
                    id: "fil-search-{id}",
                    class: "fil-input search",
                    r#type: "search",
                    value: "{query}",
                    placeholder: "words, phrase, or search syntax…",
                    "aria-invalid": "{aria_invalid}",
                    "data-testid": "filter-search-query",
                    "data-expression-input": "query",
                    onfocus: move |_| *active_palette.write() = None,
                    oninput: move |event| {
                        let value = event.value();
                        edit_filters(|model| {
                            if let Some(rule) = model.rule_mut(id) {
                                rule.kind = RuleKind::Search { query: value };
                            }
                        });
                    },
                }
            }
        }
        RuleKind::Field { field, op, value } => {
            let selected_known = catalog.iter().any(|choice| choice.name == *field);
            let datalist_id = format!("fil-values-{id}");
            let palette_open = *signals.active_palette.peek() == Some(id);
            let mut field_palette_signal = signals.active_palette;
            let mut value_palette_signal = signals.active_palette;
            let mut palette_button_signal = signals.active_palette;
            rsx! {
                label { class: "sr-only", r#for: "fil-field-{id}", "Field" }
                select {
                    id: "fil-field-{id}",
                    class: "fil-select field",
                    "aria-label": "Field",
                    "aria-invalid": "{aria_invalid}",
                    "data-testid": "filter-field-name",
                    "data-expression-input": "field",
                    onfocus: move |_| *field_palette_signal.write() = Some(id),
                    onchange: move |event| {
                        let next = event.value();
                        edit_filters(|model| {
                            if let Some(Rule { kind: RuleKind::Field { field, value, .. }, .. }) = model.rule_mut(id) {
                                if *field != next {
                                    *field = next;
                                    value.clear();
                                }
                            }
                        });
                    },
                    option { value: "", selected: field.is_empty(), "Choose field…" }
                    if !field.is_empty() && !selected_known {
                        option { value: "{field}", selected: true, "{field} · unavailable" }
                    }
                    for choice in catalog {
                        option {
                            key: "{choice.name}",
                            value: "{choice.name}",
                            selected: choice.name == *field,
                            {format!("{} · {} values", choice.name, choice.values.len())}
                        }
                    }
                }
                label { class: "sr-only", r#for: "fil-op-{id}", "Operator" }
                select {
                    id: "fil-op-{id}",
                    class: "fil-select op",
                    "aria-label": "Operator",
                    "data-testid": "filter-field-operator",
                    "data-expression-input": "operator",
                    onchange: move |event| {
                        let next = parse_op(&event.value());
                        edit_filters(|model| {
                            if let Some(Rule { kind: RuleKind::Field { op, .. }, .. }) = model.rule_mut(id) {
                                *op = next;
                            }
                        });
                    },
                    for candidate in [Op::Eq, Op::Neq, Op::Contains, Op::Matches] {
                        option {
                            value: "{op_token(candidate)}",
                            selected: candidate == *op,
                            "{op_name(candidate)}"
                        }
                    }
                }
                label { class: "sr-only", r#for: "fil-value-{id}", "Value" }
                input {
                    id: "fil-value-{id}",
                    class: "fil-input value",
                    r#type: "text",
                    value: "{value}",
                    list: "{datalist_id}",
                    placeholder: if *op == Op::Matches { "regular expression…" } else { "value…" },
                    "aria-label": "Value",
                    "aria-invalid": "{aria_invalid}",
                    "aria-autocomplete": "list",
                    "data-testid": "filter-field-value",
                    "data-expression-input": "value",
                    onfocus: move |_| *value_palette_signal.write() = Some(id),
                    oninput: move |event| {
                        let next = event.value();
                        edit_filters(|model| {
                            if let Some(Rule { kind: RuleKind::Field { value, .. }, .. }) = model.rule_mut(id) {
                                *value = next;
                            }
                        });
                    },
                }
                datalist { id: "{datalist_id}",
                    if let Some(choice) = catalog.iter().find(|choice| choice.name == *field) {
                        for (candidate, count) in choice.values.iter().take(200) {
                            option { value: "{candidate}", label: "{count} nodes" }
                        }
                    }
                }
                button {
                    class: if palette_open { "fil-palette-toggle active" } else { "fil-palette-toggle" },
                    title: "Browse fields and known values with node counts",
                    "aria-label": "Browse field values",
                    "aria-expanded": if palette_open { "true" } else { "false" },
                    "aria-controls": "fil-palette-{id}",
                    "data-testid": "filter-value-palette-toggle",
                    onclick: move |_| {
                        let next = if *palette_button_signal.peek() == Some(id) { None } else { Some(id) };
                        *palette_button_signal.write() = next;
                    },
                    "▾"
                }
            }
        }
    }
}

fn field_palette(
    id: NodeId,
    field: &str,
    op: Op,
    value: &str,
    catalog: &[FieldChoice],
    mut active_palette: Signal<Option<NodeId>>,
) -> Element {
    let current = catalog.iter().find(|choice| choice.name == field);
    let needle = value.to_lowercase();
    let suggestions: Vec<(String, usize)> = current
        .map(|choice| {
            choice
                .values
                .iter()
                .filter(|(candidate, _)| {
                    needle.trim().is_empty()
                        || op == Op::Matches
                        || candidate.to_lowercase().contains(&needle)
                })
                .take(40)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let palette_title = current
        .map(|choice| {
            format!(
                "{} known values across {} nodes",
                choice.values.len(),
                choice.node_count
            )
        })
        .unwrap_or_else(|| "Choose a filterable field".to_string());

    rsx! {
        div {
            id: "fil-palette-{id}",
            class: "fil-context",
            role: "region",
            "aria-label": "Field and value palette",
            "data-testid": "filter-value-palette",
            "data-expression-id": "{id}",
            div { class: "fil-context-head",
                div {
                    strong { "Data palette" }
                    small { "{palette_title}" }
                }
                button {
                    class: "fil-icon",
                    "aria-label": "Close field and value palette",
                    onclick: move |_| *active_palette.write() = None,
                    "×"
                }
            }
            div {
                class: "fil-field-palette",
                role: "listbox",
                "aria-label": "Filterable fields",
                for choice in catalog {
                    {
                        let field_name = choice.name.clone();
                        let selected = choice.name == field;
                        rsx! {
                            button {
                                key: "{choice.name}",
                                class: if selected { "active" } else { "" },
                                role: "option",
                                "aria-selected": if selected { "true" } else { "false" },
                                "data-field": "{choice.name}",
                                "data-field-value-count": "{choice.values.len()}",
                                "data-field-node-count": "{choice.node_count}",
                                onclick: move |_| edit_filters(|model| {
                                    if let Some(Rule { kind: RuleKind::Field { field, value, .. }, .. }) = model.rule_mut(id) {
                                        if *field != field_name {
                                            field.clone_from(&field_name);
                                            value.clear();
                                        }
                                    }
                                }),
                                span { "{choice.name}" }
                                small { {format!("{}v · {}n", choice.values.len(), choice.node_count)} }
                            }
                        }
                    }
                }
            }
            if let Some(choice) = current {
                div { class: "fil-value-head",
                    span { "Known {choice.name} values" }
                    if !needle.trim().is_empty() && op != Op::Matches {
                        small { "containing “{value}”" }
                    }
                }
                div {
                    class: "fil-value-palette",
                    role: "listbox",
                    "aria-label": "Known values for {choice.name}",
                    if suggestions.is_empty() {
                        span { class: "fil-palette-empty", "No known values match this draft. You can still use the typed value." }
                    }
                    for (candidate, count) in suggestions {
                        {
                            let next_value = candidate.clone();
                            let selected = candidate == value;
                            rsx! {
                                button {
                                    key: "{candidate}",
                                    class: if selected { "active" } else { "" },
                                    role: "option",
                                    "aria-selected": if selected { "true" } else { "false" },
                                    "data-value": "{candidate}",
                                    "data-value-node-count": "{count}",
                                    onclick: move |_| edit_filters(|model| {
                                        if let Some(Rule { kind: RuleKind::Field { value, .. }, .. }) = model.rule_mut(id) {
                                            value.clone_from(&next_value);
                                        }
                                    }),
                                    span { "{candidate}" }
                                    small { "{count}" }
                                }
                            }
                        }
                    }
                }
                if choice.values.len() > 40 && needle.trim().is_empty() {
                    div { class: "fil-palette-more", "Showing the 40 most common values. Type to narrow the palette." }
                }
            }
        }
    }
}

fn count_el(id: NodeId, enabled: bool, evaluation: &EvaluationState) -> Element {
    if !enabled {
        return rsx! {
            output {
                class: "fil-count off",
                "data-testid": "filter-match-count",
                "data-expression-count": "",
                "data-match-count": "",
                "data-count": "",
                "off"
            }
        };
    }
    let Some(count) = evaluation.counts.get(&id).copied() else {
        return if evaluation.phase == EvalPhase::Checking {
            rsx! {
                output {
                    class: "fil-count pending",
                    "data-testid": "filter-match-count",
                    "data-expression-count": "",
                    "data-match-count": "",
                    "data-count": "",
                    "aria-label": "Result count pending",
                    "…"
                }
            }
        } else {
            rsx! {}
        };
    };
    let stale = matches!(evaluation.phase, EvalPhase::Checking | EvalPhase::Invalid);
    let noun = if count == 1 { "node" } else { "nodes" };
    rsx! {
        output {
            class: if stale { "fil-count stale" } else { "fil-count" },
            title: if stale { "Count from the last valid expression" } else { "Live subtree match count" },
            "aria-label": if stale {
                format!("{count} {noun} in the last valid expression")
            } else {
                format!("{count} matching {noun}")
            },
            "data-testid": "filter-match-count",
            "data-expression-count": "{count}",
            "data-match-count": "{count}",
            "data-count": "{count}",
            "data-expression-count-stale": if stale { "true" } else { "false" },
            "data-evaluation-version": "{evaluation.applied_version}",
            "{count} {noun}"
        }
    }
}

fn toggle_enabled(id: NodeId) {
    edit_filters(|query| {
        if id == query.root().id {
            let enabled = query.root().enabled;
            query.root_mut().enabled = !enabled;
        } else {
            query.toggle_enabled(id);
        }
    });
}

fn toggle_negated(id: NodeId) {
    edit_filters(|query| {
        if id == query.root().id {
            let negated = query.root().negated;
            query.root_mut().negated = !negated;
        } else {
            query.toggle_negated(id);
        }
    });
}

#[cfg(test)]
mod tests {
    // The v1 facet-combinator semantics these used to assert against the
    // panel-local index now run through migration + `evaluate` in
    // `jump-cannon-filter-model` (the `v1_*` tests there), which is the code
    // path the panel actually uses.
    use super::{Card, Op, QueryModel};

    #[test]
    fn query_reset_removes_cards_and_active_filters() {
        let mut query = QueryModel::default();
        query.cards.push(Card::Filter {
            field: "tags".into(),
            op: Op::Eq,
            value: "rust".into(),
        });
        query.toggle_field_filter("tags", "rust");
        query.clear();
        assert!(query.cards.is_empty());
        assert!(query.root().children.is_empty());
        assert!(query.active_filters.by_field.is_empty());
        assert!(query.active_filters.insertion_order.is_empty());
    }
}
