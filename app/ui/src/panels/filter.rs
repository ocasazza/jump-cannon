//! Filter panel — Dioxus port of crates/graph-renderer/src/ui/sections/filter.rs
//! plus its data model (ui/query.rs) and the inverted index (ui/field_index.rs).
//!
//! Three layers, top to bottom:
//!   - the card-stream query builder (search card, filter cards, AND/OR
//!     connectors, paren pairs, NOT) — same mutation rules as the egui
//!     section, persisted across sessions;
//!   - a field/value bucket browser decoded from `GET /graph/meta_summary`
//!     (protobuf `MetaSummary`); clicking a bucket toggles it into the
//!     active (field, value) set the chip strip renders;
//!   - the GPU push: active filters resolve through [`FieldIndex::matches`]
//!     (per-field any/all, cross-field any/all) and land on the renderer via
//!     `set_filter_mask` (Filter) or `set_focus_set` (Focus) — the same
//!     dispatch as app.rs::apply_focus_set_to_gpu's no-node-focus arm.
//!
//! The active (field, value) chip strip — its per-field / cross-field
//! combinators and the Filter/Dim behavior toggle — renders at the top of this
//! panel (it was the separate "Filters" strip panel before the two merged).
//! User-facing settings persist under `jc_filter_v1`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use panel_kit::badge::{Badge, BadgeKind};
use serde::{Deserialize, Serialize};

use crate::api::get_proto;
use crate::badges::badge_kind_for;
use crate::{proto, render, Ctx};

// --- query model (port of ui/query.rs) -----------------------------------------

/// How multiple selections combine. `Any` = union (OR), `All` = intersect
/// (AND). Used both per-field (value buckets within a field) and cross-field
/// (each field's resulting set). Per-field `All` is meaningful for
/// multi-valued fields like `tags`; for single-valued fields it degenerates
/// to `∅` — surfaced anyway rather than lying about the data shape.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) enum Combinator {
    /// Union — match any of the selected values.
    #[default]
    Any,
    /// Intersect — match all selected values.
    All,
}

impl Combinator {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Combinator::Any => "any",
            Combinator::All => "all",
        }
    }
    pub(crate) fn toggled(self) -> Self {
        match self {
            Combinator::Any => Combinator::All,
            Combinator::All => Combinator::Any,
        }
    }
}

/// Active per-field filter selections driven by bucket/chip clicks.
///
/// Defaults: per-field `Any` (OR within a field), cross-field `All` (AND
/// across fields). `insertion_order` keeps the chip strip stable in the
/// order the user added fields (BTreeMap iteration would shuffle it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ActiveFieldFilters {
    pub by_field: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub insertion_order: Vec<String>,
    /// Per-field combinator override. Missing key = `Any` (OR).
    #[serde(default)]
    pub field_combinator: BTreeMap<String, Combinator>,
    /// How field-level results combine. Default `All` (AND) so existing
    /// persisted state behaves as before.
    #[serde(default = "default_cross_field_combinator")]
    pub cross_field_combinator: Combinator,
}

fn default_cross_field_combinator() -> Combinator {
    Combinator::All
}

impl Default for ActiveFieldFilters {
    // NB: cross-field default is `All` (AND), not the `Combinator` type
    // default — that's `Any`, which fits the per-field level instead.
    fn default() -> Self {
        Self {
            by_field: BTreeMap::new(),
            insertion_order: Vec::new(),
            field_combinator: BTreeMap::new(),
            cross_field_combinator: default_cross_field_combinator(),
        }
    }
}

impl ActiveFieldFilters {
    pub(crate) fn combinator_for(&self, field: &str) -> Combinator {
        self.field_combinator
            .get(field)
            .copied()
            .unwrap_or(Combinator::Any)
    }
    pub(crate) fn set_combinator_for(&mut self, field: &str, c: Combinator) {
        // Keep the map sparse: drop the default to avoid serde bloat.
        if matches!(c, Combinator::Any) {
            self.field_combinator.remove(field);
        } else {
            self.field_combinator.insert(field.to_string(), c);
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Op {
    /// `=`
    Eq,
    /// `≠`
    Neq,
    /// `~`
    Contains,
    /// `~/regex/`
    Matches,
}

impl Op {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Neq => "≠",
            Op::Contains => "~",
            Op::Matches => "~/r/",
        }
    }

    pub(crate) fn cycle(self) -> Op {
        match self {
            Op::Eq => Op::Neq,
            Op::Neq => Op::Contains,
            Op::Contains => Op::Matches,
            Op::Matches => Op::Eq,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ConnectorOp {
    And,
    Or,
}

impl ConnectorOp {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ConnectorOp::And => "and",
            ConnectorOp::Or => "or",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Card {
    /// System search card; no delete button.
    Search { value: String, regex: bool },
    Filter { field: String, op: Op, value: String },
    Connector { op: ConnectorOp },
    ParenOpen,
    ParenClose,
    Not,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QueryModel {
    pub cards: Vec<Card>,
    /// Bucket/chip-driven (field, value) toggles. Resolved through
    /// [`FieldIndex::matches`] and pushed to the GPU by [`sync_gpu`].
    #[serde(default)]
    pub active_filters: ActiveFieldFilters,
}

impl Default for QueryModel {
    fn default() -> Self {
        Self {
            // Always start with the system search card.
            cards: vec![Card::Search { value: String::new(), regex: false }],
            active_filters: ActiveFieldFilters::default(),
        }
    }
}

impl QueryModel {
    /// Toggle inclusion of `(field, value)` in the active filter set.
    pub(crate) fn toggle_field_filter(&mut self, field: &str, value: &str) {
        let entry = self.active_filters.by_field.entry(field.to_string()).or_default();
        if entry.contains(value) {
            entry.remove(value);
            if entry.is_empty() {
                self.active_filters.by_field.remove(field);
                self.active_filters.insertion_order.retain(|f| f != field);
                self.active_filters.field_combinator.remove(field);
            }
        } else {
            entry.insert(value.to_string());
            if !self.active_filters.insertion_order.iter().any(|f| f == field) {
                self.active_filters.insertion_order.push(field.to_string());
            }
        }
    }

    pub(crate) fn clear_field(&mut self, field: &str) {
        self.active_filters.by_field.remove(field);
        self.active_filters.insertion_order.retain(|f| f != field);
        self.active_filters.field_combinator.remove(field);
    }

    pub(crate) fn clear_all_filters(&mut self) {
        self.active_filters = ActiveFieldFilters::default();
    }

    /// Returns true if `(field, value)` is currently selected.
    pub(crate) fn is_filter_active(&self, field: &str, value: &str) -> bool {
        self.active_filters
            .by_field
            .get(field)
            .map(|set| set.contains(value))
            .unwrap_or(false)
    }

    /// Reset to the default model: just the system search card.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// If `idx` pointed at a removed `ParenOpen`, remove the matching
/// `ParenClose` (the next unmatched `)` to the right of `idx`).
fn remove_matching_paren_close(cards: &mut Vec<Card>, idx: usize) {
    let mut depth: i32 = 0;
    let mut found: Option<usize> = None;
    for (i, c) in cards.iter().enumerate().skip(idx) {
        match c {
            Card::ParenOpen => depth += 1,
            Card::ParenClose => {
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if let Some(i) = found {
        cards.remove(i);
    }
}

/// Mirror of `remove_matching_paren_close` for a removed `ParenClose`:
/// walk LEFT from `idx-1` to find the matching unmatched `(`.
fn remove_matching_paren_open(cards: &mut Vec<Card>, idx: usize) {
    if idx == 0 {
        return;
    }
    let mut depth: i32 = 0;
    let mut found: Option<usize> = None;
    for i in (0..idx).rev() {
        match cards[i] {
            Card::ParenClose => depth += 1,
            Card::ParenOpen => {
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if let Some(i) = found {
        cards.remove(i);
    }
}

// --- filter behavior (port of ui/state.rs::FilterBehavior) ----------------------

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum FilterBehavior {
    #[default]
    Filter,
    Focus,
}

impl FilterBehavior {
    pub(crate) fn label(self) -> &'static str {
        match self {
            FilterBehavior::Filter => "Filter",
            // Displayed as "Dim" so "Focus" is left to mean only the Camera
            // panel's focus mode (the variant name stays Focus for the
            // persisted serde tag).
            FilterBehavior::Focus => "Dim",
        }
    }
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
            by_field.entry(field).or_default().insert(b.value.clone(), v);
        }
        Self { by_field }
    }

    /// `Some(set)` when at least one filter is set AND at least one
    /// (field, value) pair resolves to a known bucket; `None` otherwise
    /// (= no filter active).
    pub(crate) fn matches(&self, filters: &ActiveFieldFilters) -> Option<HashSet<u32>> {
        if filters.by_field.is_empty() {
            return None;
        }
        let mut per_field: Vec<HashSet<u32>> = Vec::new();
        for (field, values) in &filters.by_field {
            if values.is_empty() {
                continue;
            }
            let Some(buckets) = self.by_field.get(field) else {
                continue;
            };
            let combinator = filters.combinator_for(field);
            // Resolve each selected value to its bucket. Skip unknown
            // values rather than treating them as the empty set —
            // otherwise stale persisted state would tank intersections.
            let mut buckets_for_field: Vec<&Vec<u32>> = Vec::new();
            for v in values {
                if let Some(idxs) = buckets.get(v) {
                    buckets_for_field.push(idxs);
                }
            }
            if buckets_for_field.is_empty() {
                continue;
            }
            let combined: HashSet<u32> = match combinator {
                Combinator::Any => {
                    let mut union: HashSet<u32> = HashSet::new();
                    for b in &buckets_for_field {
                        union.extend(b.iter().copied());
                    }
                    union
                }
                Combinator::All => {
                    // Intersect smallest first.
                    let mut sorted = buckets_for_field.clone();
                    sorted.sort_by_key(|b| b.len());
                    let mut acc: HashSet<u32> = sorted[0].iter().copied().collect();
                    for b in sorted.iter().skip(1) {
                        let bset: HashSet<u32> = b.iter().copied().collect();
                        acc.retain(|x| bset.contains(x));
                    }
                    acc
                }
            };
            per_field.push(combined);
        }
        if per_field.is_empty() {
            return None;
        }
        let acc: HashSet<u32> = match filters.cross_field_combinator {
            Combinator::All => {
                // Intersect smallest first.
                per_field.sort_by_key(|s| s.len());
                let mut iter = per_field.into_iter();
                let mut acc = iter.next().unwrap();
                for next in iter {
                    acc.retain(|x| next.contains(x));
                }
                acc
            }
            Combinator::Any => {
                let mut acc: HashSet<u32> = HashSet::new();
                for s in per_field {
                    acc.extend(s.into_iter());
                }
                acc
            }
        };
        Some(acc)
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
    LocalStorage::get(STORE_KEY).unwrap_or_default()
}

pub(crate) static QUERY: GlobalSignal<QueryModel> = Signal::global(|| load().query);
pub(crate) static BEHAVIOR: GlobalSignal<FilterBehavior> = Signal::global(|| load().behavior);
/// `None` = fetch in flight (or not started); `Some(Err)` = fetch failed.
pub(crate) static FIELD_INDEX: GlobalSignal<Option<Result<FieldIndex, String>>> =
    Signal::global(|| None);
static FIELD_INDEX_STARTED: GlobalSignal<bool> = Signal::global(|| false);

fn persist() {
    let p = Persisted { query: QUERY.peek().clone(), behavior: *BEHAVIOR.peek() };
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
    let _ = LocalStorage::set(STORE_KEY, &Persisted { query: query.clone(), behavior });
}

/// One-shot `/graph/meta_summary` fetch — called from both panels' render
/// paths so whichever opens first arms it (the egui app fetches at boot).
pub(crate) fn ensure_field_index() {
    if *FIELD_INDEX_STARTED.peek() {
        return;
    }
    *FIELD_INDEX_STARTED.write() = true;
    spawn(async move {
        match get_proto::<proto::MetaSummary>("/graph/meta_summary").await {
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

/// Resolve the active filters through the field index and dispatch on
/// [`FilterBehavior`] — the no-node-focus arm of the egui app's
/// `apply_focus_set_to_gpu`. Always clears the *other* GPU path so toggling
/// between modes doesn't leave stale state behind.
///
/// PARITY GAP: the egui app re-pushes this change-detected per frame, so a
/// renderer rebuild repaints the mask automatically; here it is mutation-
/// driven, and a canvas remount drops the mask until the next filter edit
/// (render/mod.rs::reapply_ctl_state is outside this file's ownership).
pub(crate) fn sync_gpu() {
    let matching: Option<HashSet<u32>> = FIELD_INDEX
        .peek()
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|fi| fi.matches(&QUERY.peek().active_filters));
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
                // Clear the hard filter mask, then dim via focus_set.
                // Empty matching → no dim (all visible).
                pipes.set_filter_mask(queue, None);
                pipes.set_focus_set(queue, None, &matching.unwrap_or_default());
            }
        }
    });
}

/// Card-stream mutation: persist only — cards don't drive the GPU mask
/// (Filter-card leaves are "unsupported" in the egui evaluator too).
///
/// PARITY GAP: the egui app resolves Search cards through cached
/// `/search?q=` fetches and folds AND/OR/NOT into a dim set via
/// `QueryModel::evaluate` → `set_selected` (app.rs:3776). Here the card
/// model + UI are fully ported and persisted, but that async evaluator is
/// not wired — `render::set_search_highlights` (the `set_selected` port)
/// is owned by main.rs's Search-panel effect and would be clobbered.
///
fn edit_cards(f: impl FnOnce(&mut QueryModel)) {
    // Auto-snapshot attribution — the egui Filter section stamps
    // `snapshot_source = Some("Filter")` every frame it renders.
    crate::appstate::note_source("Filter");
    f(&mut QUERY.write());
    persist();
}

/// Active-filter mutation: persist + re-push the GPU mask.
pub(crate) fn edit_filters(f: impl FnOnce(&mut QueryModel)) {
    crate::appstate::note_source("Filter");
    f(&mut QUERY.write());
    persist();
    sync_gpu();
}

pub(crate) fn toggle_behavior() {
    crate::appstate::note_source("Filter");
    let next = BEHAVIOR.peek().toggled();
    *BEHAVIOR.write() = next;
    persist();
    sync_gpu();
}

// --- card mutations (port of sections/filter.rs apply-queued-mutations) -----------

fn delete_card(idx: usize) {
    edit_cards(|q| {
        if idx < q.cards.len() {
            let removed = q.cards.remove(idx);
            match removed {
                Card::ParenOpen => remove_matching_paren_close(&mut q.cards, idx),
                Card::ParenClose => remove_matching_paren_open(&mut q.cards, idx),
                _ => {}
            }
        }
    });
}

/// Tail "+" button: append a default Filter (preceded by AND). Only prepend
/// a connector if the last card isn't already a connector / paren-open / NOT.
fn append_filter(q: &mut QueryModel) {
    let needs_connector = !matches!(
        q.cards.last(),
        Some(Card::Connector { .. }) | Some(Card::ParenOpen) | Some(Card::Not) | None
    );
    if needs_connector {
        q.cards.push(Card::Connector { op: ConnectorOp::And });
    }
    q.cards.push(Card::Filter { field: "tag".into(), op: Op::Eq, value: String::new() });
}

// --- panel -------------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    ensure_field_index();
    crate::appstate::ensure_init();
    let q = QUERY.read().clone();
    let cards = q.cards.clone();
    let active_total: usize = q.active_filters.by_field.values().map(|s| s.len()).sum();

    // Field browser snapshot: (field, [(value, count, active)]) — values
    // sorted by bucket size desc then name, fields by name.
    let fields_el: Element = match FIELD_INDEX.read().as_ref() {
        None => rsx! { div { class: "fil-note", "loading field index…" } },
        Some(Err(e)) => rsx! { div { class: "fil-note", "meta_summary failed: {e}" } },
        Some(Ok(fi)) => {
            let mut fields: Vec<(String, Vec<(String, usize, bool)>)> = fi
                .by_field
                .iter()
                .map(|(f, vals)| {
                    let mut vv: Vec<(String, usize, bool)> = vals
                        .iter()
                        .map(|(v, idxs)| (v.clone(), idxs.len(), q.is_filter_active(f, v)))
                        .collect();
                    vv.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    (f.clone(), vv)
                })
                .collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            rsx! {
                for (field, values) in fields {
                    details { key: "{field}", class: "fil-field",
                        summary {
                            span { class: "fil-fname", "{field}" }
                            span { class: "fil-fcount", { format!("{} values", values.len()) } }
                        }
                        div { class: "fil-buckets",
                            for (value, count, active) in values {
                                {
                                    let f2 = field.clone();
                                    let v2 = value.clone();
                                    rsx! {
                                        button {
                                            key: "{value}",
                                            class: if active { "fil-bucket active" } else { "fil-bucket" },
                                            title: "Toggle this (field, value) filter",
                                            onclick: move |_| edit_filters(|q| q.toggle_field_filter(&f2, &v2)),
                                            span { class: "fil-bv", "{value}" }
                                            span { class: "fil-bc", "{count}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        div { class: "fil",
            // Active (field, value) chips + combinators + Filter/Dim toggle —
            // merged in from the former standalone "Filters" strip panel.
            { active_filters_section(&q) }

            // Right-aligned reset row (ui/widgets.rs::reset_row): back to the
            // default model — just the system search card, no active filters.
            div { class: "fil-reset",
                button { class: "fil-card-btn",
                    onclick: move |_| edit_filters(|q| *q = QueryModel::default()),
                    "↺ Reset"
                }
            }

            // Card stream + tail "+".
            div { class: "fil-cards",
                for (i, card) in cards.into_iter().enumerate() {
                    { card_el(i, card) }
                }
                button { class: "fil-card-btn fil-plus", title: "Add filter",
                    onclick: move |_| edit_cards(append_filter),
                    "+"
                }
            }

            // Secondary add-buttons row.
            div { class: "fil-addrow",
                button { class: "fil-card-btn",
                    onclick: move |_| edit_cards(|q| q.cards.push(Card::Connector { op: ConnectorOp::And })),
                    "+ and"
                }
                button { class: "fil-card-btn",
                    onclick: move |_| edit_cards(|q| q.cards.push(Card::Connector { op: ConnectorOp::Or })),
                    "+ or"
                }
                button { class: "fil-card-btn",
                    onclick: move |_| edit_cards(|q| {
                        q.cards.push(Card::ParenOpen);
                        q.cards.push(Card::ParenClose);
                    }),
                    "+ ( )"
                }
                button { class: "fil-card-btn",
                    onclick: move |_| edit_cards(|q| q.cards.push(Card::Not)),
                    "+ not"
                }
                button { class: "fil-card-btn fil-clear",
                    onclick: move |_| edit_filters(|q| q.clear()),
                    "Clear"
                }
            }

            // Field/value bucket browser (FieldIndex from /graph/meta_summary).
            div { class: "fil-fields",
                div { class: "fil-fhead",
                    span { class: "fil-ftitle", "fields" }
                    if active_total >= 1 {
                        button { class: "fil-card-btn fil-clear fil-clearall",
                            onclick: move |_| edit_filters(|q| q.clear_all_filters()),
                            "clear all"
                        }
                    }
                }
                {fields_el}
            }
        }
    }
}

/// The active-filter strip: cross-field combinator + Filter/Dim behavior
/// toggle in the header, one row per active field below. Empty state when no
/// filters are active. (Merged in from the former `filter_strip.rs` panel; all
/// mutations route through `edit_filters` so the GPU push stays in one place.)
fn active_filters_section(q: &QueryModel) -> Element {
    let behavior = *BEHAVIOR.read();
    let total: usize = q.active_filters.by_field.values().map(|s| s.len()).sum();
    if total == 0 {
        return rsx! { div { class: "fst-empty", "no active filters" } };
    }

    let cross = q.active_filters.cross_field_combinator;
    let cross_label = format!("{} fields", cross.label());
    let behavior_label = behavior.label();
    let behavior_tip = behavior.tooltip();

    // Fields render in user-insertion order, not BTreeMap name order.
    let order: Vec<String> = q
        .active_filters
        .insertion_order
        .iter()
        .filter(|f| q.active_filters.by_field.contains_key(*f))
        .cloned()
        .collect();

    rsx! {
        div { class: "fst",
            div { class: "fst-head",
                span { class: "fst-k", "match" }
                button { class: "fst-toggle",
                    title: "How field-level results combine: `all` = AND (intersect), `any` = OR (union).",
                    onclick: move |_| {
                        let next = cross.toggled();
                        edit_filters(move |q| q.active_filters.cross_field_combinator = next);
                    },
                    "{cross_label}"
                }
                span { class: "fst-sep" }
                // Filter / Dim behavior toggle.
                span { class: "fst-k", "when matched:" }
                button { class: "fst-toggle", title: "{behavior_tip}",
                    onclick: move |_| toggle_behavior(),
                    "{behavior_label}"
                }
                // Clear-all on the right — only worth a button from 2 chips up.
                if total >= 2 {
                    button { class: "fst-toggle fst-clear",
                        onclick: move |_| edit_filters(|q| q.clear_all_filters()),
                        "clear filters"
                    }
                }
            }
            div { class: "fst-rows",
                for field in order {
                    { active_field_row(q, field) }
                }
            }
        }
    }
}

/// One row per active field: `field_name [any|all] [chip] [chip] …`.
fn active_field_row(q: &QueryModel, field: String) -> Element {
    let combinator = q.active_filters.combinator_for(&field);
    let comb_label = combinator.label();
    let values: Vec<String> = q
        .active_filters
        .by_field
        .get(&field)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    // Only meaningful with ≥ 2 values; still rendered (greyed) otherwise so the
    // affordance stays visible.
    let multi = values.len() > 1;
    let f_clear = field.clone();
    let f_comb = field.clone();
    rsx! {
        div { class: "fst-row", key: "{field}",
            // Field-name lozenge with ✕ — clears the whole field.
            Badge {
                field: "{field}",
                value: "{field}",
                kind: BadgeKind::Generic,
                active: true,
                with_x: true,
                on_action: move |_| edit_filters(|q| q.clear_field(&f_clear)),
            }
            button {
                class: if multi { "fst-toggle fst-comb" } else { "fst-toggle fst-comb dim" },
                title: "Toggle how this field's values combine: `any` = OR (match any value), `all` = AND (match all values).",
                onclick: move |_| {
                    let next = combinator.toggled();
                    edit_filters(|q| q.active_filters.set_combinator_for(&f_comb, next));
                },
                "{comb_label}"
            }
            // Value chips — click removes.
            for value in values {
                { active_chip(field.clone(), value) }
            }
        }
    }
}

fn active_chip(field: String, value: String) -> Element {
    let kind = badge_kind_for(&field);
    let (f, v) = (field.clone(), value.clone());
    rsx! {
        Badge {
            key: "{value}",
            field: "{field}",
            value: "{value}",
            kind,
            active: true,
            with_x: true,
            on_action: move |_| edit_filters(|q| q.toggle_field_filter(&f, &v)),
        }
    }
}

/// One card widget — same controls per variant as the egui renderer's
/// `render_card`, with mutations routed through `edit_cards`/`delete_card`.
fn card_el(i: usize, card: Card) -> Element {
    match card {
        Card::Search { value, regex } => {
            let regex_label = if regex { ".*" } else { "abc" };
            rsx! {
                div { class: "fil-card",
                    span { class: "fil-k", "search:" }
                    input { class: "fil-in search", placeholder: "text…", value: "{value}",
                        oninput: move |e| {
                            let v = e.value();
                            edit_cards(|q| {
                                if let Some(Card::Search { value, .. }) = q.cards.get_mut(i) {
                                    *value = v;
                                }
                            });
                        },
                    }
                    button { class: "fil-card-btn", title: "Toggle regex",
                        onclick: move |_| edit_cards(|q| {
                            if let Some(Card::Search { regex, .. }) = q.cards.get_mut(i) {
                                *regex = !*regex;
                            }
                        }),
                        "{regex_label}"
                    }
                    // No delete: system card.
                }
            }
        }
        Card::Filter { field, op, value } => {
            let op_label = op.label();
            rsx! {
                div { class: "fil-card",
                    input { class: "fil-in field", placeholder: "field", value: "{field}",
                        oninput: move |e| {
                            let v = e.value();
                            edit_cards(|q| {
                                if let Some(Card::Filter { field, .. }) = q.cards.get_mut(i) {
                                    *field = v;
                                }
                            });
                        },
                    }
                    button { class: "fil-card-btn", title: "Cycle operator",
                        onclick: move |_| edit_cards(|q| {
                            if let Some(Card::Filter { op, .. }) = q.cards.get_mut(i) {
                                *op = op.cycle();
                            }
                        }),
                        "{op_label}"
                    }
                    input { class: "fil-in value", placeholder: "value", value: "{value}",
                        oninput: move |e| {
                            let v = e.value();
                            edit_cards(|q| {
                                if let Some(Card::Filter { value, .. }) = q.cards.get_mut(i) {
                                    *value = v;
                                }
                            });
                        },
                    }
                    button { class: "fil-card-btn", title: "Delete filter",
                        onclick: move |_| delete_card(i),
                        "×"
                    }
                }
            }
        }
        Card::Connector { op } => {
            let op_label = op.label();
            rsx! {
                div { class: "fil-card",
                    button { class: "fil-card-btn fil-strong", title: "Click to toggle AND/OR",
                        onclick: move |_| edit_cards(|q| {
                            if let Some(Card::Connector { op }) = q.cards.get_mut(i) {
                                *op = match *op {
                                    ConnectorOp::And => ConnectorOp::Or,
                                    ConnectorOp::Or => ConnectorOp::And,
                                };
                            }
                        }),
                        "{op_label}"
                    }
                    button { class: "fil-card-btn", title: "Delete connector",
                        onclick: move |_| delete_card(i),
                        "×"
                    }
                }
            }
        }
        Card::ParenOpen => rsx! {
            div { class: "fil-card",
                span { class: "fil-glyph", "(" }
                button { class: "fil-card-btn", title: "Delete paren pair",
                    onclick: move |_| delete_card(i),
                    "×"
                }
            }
        },
        Card::ParenClose => rsx! {
            div { class: "fil-card",
                span { class: "fil-glyph", ")" }
                button { class: "fil-card-btn", title: "Delete paren pair",
                    onclick: move |_| delete_card(i),
                    "×"
                }
            }
        },
        Card::Not => rsx! {
            div { class: "fil-card",
                span { class: "fil-glyph", "not" }
                button { class: "fil-card-btn", title: "Delete NOT",
                    onclick: move |_| delete_card(i),
                    "×"
                }
            }
        },
    }
}
