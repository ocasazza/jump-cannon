//! Nodes workbench — browse/search navigation beside the focused node body.
//!
//! The left navigator has two browse modes when the query is empty:
//!   * **Flat** — graph-order node ids, bounded for large graphs;
//!   * **Tags** — application-wide hierarchical tag paths -> node groups from
//!     `/graph/meta_summary`.
//!
//! Discovery schema v1 requires every importer to publish `/` as the tag path
//! separator. `foo/bar/baz` therefore becomes a nested path for every source;
//! single-segment tags remain at the root. A node with multiple tags appears
//! under every declared path, while nodes with no tags appear under synthetic
//! `(untagged)`. Groups expand lazily so a large graph does not create a hidden
//! DOM containing every tag assignment.
//!
//! A non-empty query keeps the existing source-neutral search surfaces:
//! identifier fuzzy matches, importer-indexed hits, and filter suggestions.
//! The right editor area follows the shared `ctx.selected` signal and embeds
//! the same readable/writable content viewer as the detachable Document panel.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use panel_kit::Spinner;
use serde::{Deserialize, Serialize};

use crate::panels::{document, filter};
use crate::{api, Ctx};

/// Rich indexed hits for the current query (display list only — the id set
/// for canvas highlighting lives on `ctx.results`).
static RICH: GlobalSignal<Vec<api::RichHit>> = Signal::global(Vec::new);
/// Active importer's discovery contract, fetched once per hosted graph.
static SEARCH_SCHEMA: GlobalSignal<Option<Result<api::GraphSchema, String>>> =
    Signal::global(|| None);
static SEARCH_SCHEMA_STARTED: GlobalSignal<bool> = Signal::global(|| false);
static SEARCH_SCHEMA_SESSION: GlobalSignal<u64> = Signal::global(|| 0);
/// Last server-side query error. Invalid field-qualified queries are user
/// input errors, not empty result sets, so keep them visible beside results.
static SEARCH_ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);
/// Debounce generation — a keystroke invalidates older in-flight searches.
static GEN: GlobalSignal<u32> = Signal::global(|| 0);

const NAVIGATOR_STORE_KEY: &str = "jc_nodes_navigator_v1";
const DEBOUNCE_MS: u32 = 250;
const ID_CAP: usize = 40;
const LIST_CAP: usize = 300;
const TAG_GROUP_CAP: usize = 500;
const TAG_NODE_CAP: usize = 300;
const SUGGESTION_CAP: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum NavigatorMode {
    #[default]
    Flat,
    Tags,
}

impl NavigatorMode {
    fn label(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::Tags => "Tags",
        }
    }
}

fn load_navigator_mode() -> NavigatorMode {
    LocalStorage::get(NAVIGATOR_STORE_KEY).unwrap_or_default()
}

static NAVIGATOR_MODE: GlobalSignal<NavigatorMode> = Signal::global(load_navigator_mode);
static EXPANDED_TAGS: GlobalSignal<BTreeSet<String>> = Signal::global(BTreeSet::new);
static TAG_HIERARCHY: GlobalSignal<Option<Arc<TagHierarchy>>> = Signal::global(|| None);

#[derive(Clone, Debug, PartialEq, Eq)]
struct TagGroup {
    tag: String,
    nodes: Arc<[u32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TagBranch {
    label: String,
    path: String,
    group_index: Option<usize>,
    aggregate_count: usize,
    children: Vec<Arc<TagBranch>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TagHierarchy {
    groups: Arc<[TagGroup]>,
    roots: Vec<Arc<TagBranch>>,
    untagged: Arc<[u32]>,
}

#[derive(Default)]
struct TagBranchBuilder {
    group_index: Option<usize>,
    children: BTreeMap<String, TagBranchBuilder>,
}

impl TagHierarchy {
    fn from_buckets(
        tags: Option<&HashMap<String, Vec<u32>>>,
        node_count: usize,
        hierarchy_separator: char,
    ) -> Self {
        let mut tagged = vec![false; node_count];
        let mut groups = Vec::new();

        if let Some(tags) = tags {
            for (tag, indices) in tags {
                if tag.trim().is_empty() {
                    continue;
                }
                let mut nodes: Vec<u32> = indices
                    .iter()
                    .copied()
                    .filter(|index| (*index as usize) < node_count)
                    .collect();
                nodes.sort_unstable();
                nodes.dedup();
                if nodes.is_empty() {
                    continue;
                }
                for index in &nodes {
                    tagged[*index as usize] = true;
                }
                groups.push(TagGroup {
                    tag: tag.clone(),
                    nodes: nodes.into(),
                });
            }
        }

        groups.sort_by(|left, right| {
            left.tag
                .to_lowercase()
                .cmp(&right.tag.to_lowercase())
                .then_with(|| left.tag.cmp(&right.tag))
        });
        let groups: Arc<[TagGroup]> = groups.into();
        let mut builders = BTreeMap::<String, TagBranchBuilder>::new();
        for (group_index, group) in groups.iter().enumerate() {
            let segments = tag_segments(&group.tag, hierarchy_separator);
            let segment_count = segments.len();
            let mut level = &mut builders;
            for (segment_index, segment) in segments.into_iter().enumerate() {
                let branch = level.entry(segment).or_default();
                if segment_index + 1 == segment_count {
                    branch.group_index = Some(group_index);
                }
                level = &mut branch.children;
            }
        }
        let roots = freeze_tag_branches(builders, "", hierarchy_separator, &groups);
        let untagged: Arc<[u32]> = tagged
            .into_iter()
            .enumerate()
            .filter_map(|(index, tagged)| (!tagged).then_some(index as u32))
            .collect::<Vec<_>>()
            .into();
        Self {
            groups,
            roots,
            untagged,
        }
    }
}

fn tag_segments(tag: &str, hierarchy_separator: char) -> Vec<String> {
    let segments: Vec<&str> = tag.split(hierarchy_separator).collect();
    if segments.len() > 1 && segments.iter().all(|segment| !segment.is_empty()) {
        segments.into_iter().map(str::to_string).collect()
    } else {
        vec![tag.to_string()]
    }
}

fn freeze_tag_branches(
    builders: BTreeMap<String, TagBranchBuilder>,
    parent_path: &str,
    separator: char,
    groups: &[TagGroup],
) -> Vec<Arc<TagBranch>> {
    let mut branches: Vec<Arc<TagBranch>> = builders
        .into_iter()
        .map(|(label, builder)| {
            let path = if parent_path.is_empty() {
                label.clone()
            } else {
                format!("{parent_path}{separator}{label}")
            };
            let children = freeze_tag_branches(builder.children, &path, separator, groups);
            let mut members = BTreeSet::new();
            if let Some(group_index) = builder.group_index {
                if let Some(group) = groups.get(group_index) {
                    members.extend(group.nodes.iter().copied());
                }
            }
            for child in &children {
                collect_branch_members(child, groups, &mut members);
            }
            Arc::new(TagBranch {
                label,
                path,
                group_index: builder.group_index,
                aggregate_count: members.len(),
                children,
            })
        })
        .collect();
    branches.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.label.cmp(&right.label))
    });
    branches
}

fn collect_branch_members(branch: &TagBranch, groups: &[TagGroup], members: &mut BTreeSet<u32>) {
    if let Some(group_index) = branch.group_index {
        if let Some(group) = groups.get(group_index) {
            members.extend(group.nodes.iter().copied());
        }
    }
    for child in &branch.children {
        collect_branch_members(child, groups, members);
    }
}

pub(crate) fn reset_for_graph_session() {
    *GEN.write() = GEN.peek().wrapping_add(1);
    RICH.write().clear();
    *SEARCH_ERROR.write() = None;
    *SEARCH_SCHEMA_SESSION.write() = SEARCH_SCHEMA_SESSION.peek().wrapping_add(1);
    *SEARCH_SCHEMA_STARTED.write() = false;
    *SEARCH_SCHEMA.write() = None;
    EXPANDED_TAGS.write().clear();
    *TAG_HIERARCHY.write() = None;
}

// --- fuzzy matching -----------------------------------------------------------

/// Subsequence fuzzy match of `needle` (lowercase) against `hay`.
/// Returns (score, matched byte positions) — fzf-style bonuses: consecutive
/// runs and segment starts (`/ - _ space` boundaries) score higher.
fn fuzzy_match(needle: &str, hay: &str) -> Option<(i32, Vec<usize>)> {
    let hay_lower = hay.to_lowercase();
    let hay_bytes = hay_lower.as_bytes();
    let mut score = 0i32;
    let mut positions = Vec::with_capacity(needle.len());
    let mut hi = 0usize;
    let mut prev_match: Option<usize> = None;
    for nc in needle.bytes() {
        let mut found = None;
        while hi < hay_bytes.len() {
            if hay_bytes[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let pos = found?;
        score += 2;
        if prev_match == Some(pos.wrapping_sub(1)) {
            score += 3;
        }
        if pos == 0 || matches!(hay_bytes[pos - 1], b'/' | b'-' | b'_' | b' ' | b'.') {
            score += 2;
        }
        positions.push(pos);
        prev_match = Some(pos);
        hi = pos + 1;
    }
    score -= (hay.len() / 16) as i32;
    Some((score, positions))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn highlight_positions(hay: &str, positions: &[usize]) -> String {
    let mut out = String::with_capacity(hay.len() + positions.len() * 7);
    for (i, ch) in hay.char_indices() {
        let piece = escape_html(&ch.to_string());
        if positions.binary_search(&i).is_ok() {
            out.push_str("<b>");
            out.push_str(&piece);
            out.push_str("</b>");
        } else {
            out.push_str(&piece);
        }
    }
    out
}

// --- search dispatch ----------------------------------------------------------

fn ensure_search_schema(ctx: Ctx) {
    if !ctx.graph_session.peek().is_server_backed() || *SEARCH_SCHEMA_STARTED.peek() {
        return;
    }
    *SEARCH_SCHEMA_STARTED.write() = true;
    let session = *SEARCH_SCHEMA_SESSION.peek();
    spawn(async move {
        let fetched = api::graph_schema().await;
        if *SEARCH_SCHEMA_SESSION.peek() != session {
            return;
        }
        if let Err(error) = &fetched {
            tracing::warn!("[nodes] graph schema fetch failed: {error}");
        }
        *SEARCH_SCHEMA.write() = Some(fetched);
    });
}

fn run_search(ctx: Ctx) {
    let Ctx {
        mut results,
        mut result_total,
        mut searching,
        query,
        graph_session,
        ..
    } = ctx;
    let q = query.peek().trim().to_string();
    *SEARCH_ERROR.write() = None;
    let generation = *GEN.peek();
    let session = graph_session.peek().clone();
    if !session.is_server_backed() {
        RICH.write().clear();
        results.set(Vec::new());
        result_total.set(0);
        searching.set(false);
        return;
    }
    if q.is_empty() {
        RICH.write().clear();
        results.set(Vec::new());
        result_total.set(0);
        searching.set(false);
        return;
    }
    spawn(async move {
        gloo_timers::future::TimeoutFuture::new(DEBOUNCE_MS).await;
        if *GEN.peek() != generation {
            return;
        }
        searching.set(true);
        let response = api::search_rich(&q, 60).await;
        if *GEN.peek() != generation || graph_session.peek().epoch != session.epoch {
            return;
        }
        match response {
            Ok(response) => {
                result_total.set(response.total as u32);
                results.set(response.results.iter().map(|hit| hit.id.clone()).collect());
                *RICH.write() = response.results;
            }
            Err(error) => {
                result_total.set(0);
                results.set(Vec::new());
                RICH.write().clear();
                *SEARCH_ERROR.write() = Some(format!("search failed: {error}"));
            }
        }
        searching.set(false);
    });
}

// --- navigator models + rendering --------------------------------------------

fn set_navigator_mode(mode: NavigatorMode) {
    *NAVIGATOR_MODE.write() = mode;
    let _ = LocalStorage::set(NAVIGATOR_STORE_KEY, mode);
}

fn toggle_tag(tag: &str) {
    let mut expanded = EXPANDED_TAGS.write();
    if expanded.contains(tag) {
        expanded.remove(tag);
    } else {
        expanded.insert(tag.to_string());
    }
}

fn tags_are_facetable(schema: &Option<Result<api::GraphSchema, String>>) -> Option<bool> {
    schema.as_ref().and_then(|schema| {
        schema.as_ref().ok().map(|schema| {
            schema
                .schema
                .fields
                .iter()
                .find(|field| field.key == "tags")
                .is_some_and(|field| field.facetable)
        })
    })
}

fn tag_hierarchy_separator(schema: &Option<Result<api::GraphSchema, String>>) -> Option<char> {
    schema
        .as_ref()
        .and_then(|schema| schema.as_ref().ok())
        .map(|schema| schema.schema.tag_hierarchy.separator)
}

fn ensure_tag_hierarchy(node_count: usize, hierarchy_separator: char) {
    if TAG_HIERARCHY.peek().is_some() {
        return;
    }
    let hierarchy = {
        let index = filter::FIELD_INDEX.read();
        index.as_ref().and_then(|result| {
            result.as_ref().ok().map(|index| {
                TagHierarchy::from_buckets(
                    index.by_field.get("tags"),
                    node_count,
                    hierarchy_separator,
                )
            })
        })
    };
    if let Some(hierarchy) = hierarchy {
        *TAG_HIERARCHY.write() = Some(Arc::new(hierarchy));
    }
}

fn node_button(
    id: String,
    key: String,
    mut selected: Signal<Option<String>>,
    class: &'static str,
) -> Element {
    let active = selected.read().as_deref() == Some(id.as_str());
    let click_id = id.clone();
    let class_name = if active {
        format!("{class} active")
    } else {
        class.to_string()
    };
    rsx! {
        button {
            key: "{key}",
            class: "{class_name}",
            "data-node-id": id.clone(),
            "aria-current": if active { "page" } else { "false" },
            onclick: move |_| selected.set(Some(click_id.clone())),
            span { class: "qi-id", "{id}" }
        }
    }
}

fn flat_navigation(ids: &[String], selected: Signal<Option<String>>) -> Element {
    let mut shown: Vec<String> = ids.iter().take(LIST_CAP).cloned().collect();
    let selected_id = selected.read().clone();
    if let Some(selected_id) = selected_id {
        if ids.iter().any(|id| id == &selected_id) && !shown.iter().any(|id| id == &selected_id) {
            shown.push(selected_id);
        }
    }
    let omitted = ids.len().saturating_sub(shown.len());
    rsx! {
        nav { class: "queue", "aria-label": "Flat node list",
            for id in shown {
                { node_button(id.clone(), format!("flat:{id}"), selected, "queue-item") }
            }
            if omitted > 0 {
                div { class: "more", "… {omitted} more (type to fuzzy-find)" }
            }
        }
    }
}

fn tag_group(
    identity: String,
    label: String,
    nodes: Arc<[u32]>,
    ids: &[String],
    selected: Signal<Option<String>>,
    synthetic: bool,
) -> Element {
    let expanded = EXPANDED_TAGS.read().contains(&identity);
    let toggle_identity = identity.clone();
    let section_key = identity.clone();
    let node_key_prefix = identity.clone();
    let count = nodes.len();
    let mut shown_nodes: Vec<String> = if expanded {
        nodes
            .iter()
            .take(TAG_NODE_CAP)
            .filter_map(|index| ids.get(*index as usize))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let selected_id = selected.read().clone();
    if expanded {
        if let Some(selected_id) = selected_id {
            if let Some(selected_index) = ids.iter().position(|id| id == &selected_id) {
                if nodes.binary_search(&(selected_index as u32)).is_ok()
                    && !shown_nodes.iter().any(|id| id == &selected_id)
                {
                    shown_nodes.push(selected_id);
                }
            }
        }
    }
    let omitted = count.saturating_sub(shown_nodes.len());
    rsx! {
        section {
            class: if synthetic { "nodes-tag-group synthetic" } else { "nodes-tag-group" },
            key: "{section_key}",
            "data-tag": label.clone(),
            "data-tag-kind": if synthetic { "synthetic" } else { "exact" },
            "data-synthetic-group": if synthetic { "untagged" } else { "" },
            button {
                class: "nodes-tag-summary",
                "aria-expanded": if expanded { "true" } else { "false" },
                onclick: move |_| toggle_tag(&toggle_identity),
                span { class: "nodes-tag-chevron", if expanded { "▾" } else { "▸" } }
                span { class: "nodes-tag-label", "{label}" }
                span { class: "nodes-tag-count", "{count}" }
            }
            if expanded {
                div { class: "nodes-tag-children",
                    for id in shown_nodes {
                        { node_button(
                            id.clone(),
                            format!("{node_key_prefix}:{id}"),
                            selected,
                            "queue-item nodes-tag-node",
                        ) }
                    }
                    if omitted > 0 {
                        div { class: "more", "… {omitted} more in this tag (type to search)" }
                    }
                }
            }
        }
    }
}

fn tag_branch_is_visible(branch: &TagBranch, visible_groups: &BTreeSet<usize>) -> bool {
    branch
        .group_index
        .is_some_and(|group_index| visible_groups.contains(&group_index))
        || branch
            .children
            .iter()
            .any(|child| tag_branch_is_visible(child, visible_groups))
}

fn tag_branch(
    branch: Arc<TagBranch>,
    depth: usize,
    groups: Arc<[TagGroup]>,
    visible_groups: Arc<BTreeSet<usize>>,
    ids: &[String],
    selected: Signal<Option<String>>,
) -> Element {
    let expanded = EXPANDED_TAGS.read().contains(&branch.path);
    let toggle_identity = branch.path.clone();
    let section_key = branch.path.clone();
    let node_key_prefix = branch.path.clone();
    let exact_group = branch
        .group_index
        .filter(|group_index| visible_groups.contains(group_index))
        .and_then(|group_index| groups.get(group_index));
    let exact_tag = exact_group.map(|group| group.tag.clone());
    let direct_nodes = exact_group.map(|group| Arc::clone(&group.nodes));
    let visible_children: Vec<Arc<TagBranch>> = branch
        .children
        .iter()
        .filter(|child| tag_branch_is_visible(child, &visible_groups))
        .cloned()
        .collect();
    let mut shown_nodes: Vec<String> = if expanded {
        direct_nodes
            .as_deref()
            .unwrap_or_default()
            .iter()
            .take(TAG_NODE_CAP)
            .filter_map(|index| ids.get(*index as usize))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let selected_id = selected.read().clone();
    if expanded {
        if let (Some(selected_id), Some(direct_nodes)) = (selected_id, direct_nodes.as_ref()) {
            if let Some(selected_index) = ids.iter().position(|id| id == &selected_id) {
                if direct_nodes.binary_search(&(selected_index as u32)).is_ok()
                    && !shown_nodes.iter().any(|id| id == &selected_id)
                {
                    shown_nodes.push(selected_id);
                }
            }
        }
    }
    let direct_count = direct_nodes.as_ref().map_or(0, |nodes| nodes.len());
    let omitted = direct_count.saturating_sub(shown_nodes.len());
    let label = branch.label.clone();
    let path = branch.path.clone();
    let aggregate_count = branch.aggregate_count;
    rsx! {
        section {
            class: "nodes-tag-group",
            key: "{section_key}",
            role: "treeitem",
            "aria-level": "{depth}",
            "data-tag": exact_tag.unwrap_or_else(|| path.clone()),
            "data-tag-path": path.clone(),
            "data-tag-segment": label.clone(),
            "data-tag-kind": if direct_nodes.is_some() { "exact" } else { "branch" },
            "data-tag-depth": "{depth}",
            button {
                class: "nodes-tag-summary",
                title: "{path}",
                "aria-expanded": if expanded { "true" } else { "false" },
                onclick: move |_| toggle_tag(&toggle_identity),
                span { class: "nodes-tag-chevron", if expanded { "▾" } else { "▸" } }
                span { class: "nodes-tag-label", "{label}" }
                span { class: "nodes-tag-count", "{aggregate_count}" }
            }
            if expanded {
                div { class: "nodes-tag-children", role: "group",
                    for id in shown_nodes {
                        { node_button(
                            id.clone(),
                            format!("{node_key_prefix}:{id}"),
                            selected,
                            "queue-item nodes-tag-node",
                        ) }
                    }
                    if omitted > 0 {
                        div { class: "more", "… {omitted} more in this tag (type to search)" }
                    }
                    for child in visible_children {
                        { tag_branch(
                            child,
                            depth + 1,
                            Arc::clone(&groups),
                            Arc::clone(&visible_groups),
                            ids,
                            selected,
                        ) }
                    }
                }
            }
        }
    }
}

fn tag_navigation(
    hierarchy: Arc<TagHierarchy>,
    ids: &[String],
    selected: Signal<Option<String>>,
) -> Element {
    let group_count = hierarchy.groups.len();
    let selected_index = selected
        .read()
        .as_ref()
        .and_then(|selected_id| ids.iter().position(|id| id == selected_id))
        .map(|index| index as u32);
    // Keep the complete importer facet index behind `Arc`; reactive selection
    // and metadata updates clone only labels and shared slices, not millions
    // of tag memberships.
    let visible_groups: BTreeSet<usize> = hierarchy
        .groups
        .iter()
        .enumerate()
        .filter(|(index, group)| {
            *index < TAG_GROUP_CAP
                || selected_index
                    .is_some_and(|selected| group.nodes.binary_search(&selected).is_ok())
        })
        .map(|(index, _)| index)
        .collect();
    let omitted_groups = group_count.saturating_sub(visible_groups.len());
    let visible_groups = Arc::new(visible_groups);
    let groups = Arc::clone(&hierarchy.groups);
    let shown_roots: Vec<Arc<TagBranch>> = hierarchy
        .roots
        .iter()
        .filter(|branch| tag_branch_is_visible(branch, &visible_groups))
        .cloned()
        .collect();
    let untagged = Arc::clone(&hierarchy.untagged);
    rsx! {
        nav { class: "nodes-tag-tree", role: "tree", "aria-label": "Nodes grouped by tag",
            for branch in shown_roots {
                { tag_branch(
                    branch,
                    1,
                    Arc::clone(&groups),
                    Arc::clone(&visible_groups),
                    ids,
                    selected,
                ) }
            }
            if !untagged.is_empty() {
                { tag_group(
                    "synthetic:untagged".to_string(),
                    "(untagged)".to_string(),
                    untagged,
                    ids,
                    selected,
                    true,
                ) }
            }
            if omitted_groups > 0 {
                div { class: "more", "… {omitted_groups} more tag groups" }
            }
        }
    }
}

fn focused_node(ctx: Ctx) -> Element {
    if !ctx.graph_session.read().is_server_backed() {
        return rsx! {
            div { class: "nodes-focus-empty",
                h2 { "No hosted content" }
                p { "Client-only graphs expose topology and ids, but not source-backed node content." }
            }
        };
    }
    if *ctx.meta_busy.read() {
        return rsx! { div { class: "nodes-focus-empty", Spinner { label: "loading node…" } } };
    }
    let selected = ctx.selected.read().clone();
    let meta = ctx.meta.read().clone();
    let Some(meta) = meta else {
        let message = if selected.is_some() {
            let error = ctx.save_msg.read().clone();
            if error.is_empty() {
                "The selected node could not be loaded.".to_string()
            } else {
                error
            }
        } else {
            "Choose a node from the navigator or graph to inspect its content.".to_string()
        };
        return rsx! {
            div { class: "nodes-focus-empty",
                h2 { if selected.is_some() { "Node unavailable" } else { "Select a node" } }
                p { "{message}" }
            }
        };
    };

    let title = if meta.title.trim().is_empty() {
        meta.id.clone()
    } else {
        meta.title.clone()
    };
    let content_state = if meta.content_writable {
        ("editable", "nodes-content-state editable")
    } else if meta.content_readable {
        ("read-only", "nodes-content-state readable")
    } else {
        ("metadata only", "nodes-content-state metadata")
    };
    rsx! {
        article {
            class: "nodes-focus",
            "data-focused-node": meta.id.clone(),
            header { class: "nodes-focus-head",
                div { class: "nodes-focus-heading",
                    span { class: "nodes-focus-kind",
                        {meta.doctype.clone().unwrap_or_else(|| "node".to_string())}
                    }
                    h2 { class: "nodes-focus-title", "{title}" }
                    div { class: "nodes-focus-id", "{meta.id}" }
                    if !meta.path.is_empty() && meta.path != meta.id {
                        div { class: "nodes-focus-path", "{meta.path}" }
                    }
                }
                div { class: "nodes-focus-provenance",
                    span { class: content_state.1, "{content_state.0}" }
                    if !meta.source_id.is_empty() {
                        span { class: "nodes-source", "{meta.source_id}" }
                    }
                }
            }
            div { class: "nodes-focus-body",
                { document::viewer(ctx, "jc-nodes-src") }
            }
        }
    }
}

// --- panel --------------------------------------------------------------------

pub fn panel(ctx: Ctx) -> Element {
    crate::appstate::ensure_init();
    let Ctx {
        graph,
        graph_session,
        mut selected,
        mut query,
        searching,
        result_total,
        ..
    } = ctx;
    let graph_guard = graph.read();
    let Some(graph) = graph_guard.as_ref() else {
        return rsx! { div { class: "empty", "—" } };
    };
    let q = query.read().trim().to_string();
    let q_lower = q.to_lowercase();
    let server_backed = graph_session.read().is_server_backed();
    let mode = *NAVIGATOR_MODE.read();
    if server_backed {
        ensure_search_schema(ctx);
    }
    if mode == NavigatorMode::Tags && server_backed {
        filter::ensure_field_index(ctx);
    }

    let schema = SEARCH_SCHEMA.read().clone();
    let search_error = SEARCH_ERROR.read().clone();
    let search_failed = search_error.is_some();
    let tag_contract = tags_are_facetable(&schema);
    let tag_schema_error = schema
        .as_ref()
        .and_then(|schema| schema.as_ref().err())
        .cloned();
    let hierarchy_separator = tag_hierarchy_separator(&schema);
    if mode == NavigatorMode::Tags && tag_contract == Some(true) {
        if let Some(hierarchy_separator) = hierarchy_separator {
            ensure_tag_hierarchy(graph.ids.len(), hierarchy_separator);
        }
    }
    let hierarchy = TAG_HIERARCHY.read().clone();

    let suggestions: Vec<(String, String, usize, bool)> = if q.is_empty() || !server_backed {
        Vec::new()
    } else {
        let active_q = filter::QUERY.read();
        let mut values: Vec<(String, String, usize, bool)> = filter::FIELD_INDEX
            .read()
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|index| {
                index
                    .by_field
                    .iter()
                    .flat_map(|(field, values)| {
                        values
                            .iter()
                            .filter(|(value, _)| value.to_lowercase().contains(&q_lower))
                            .map(move |(value, nodes)| {
                                (field.clone(), value.clone(), nodes.len(), false)
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        values.sort_by_key(|value| Reverse(value.2));
        values.truncate(SUGGESTION_CAP);
        for suggestion in &mut values {
            suggestion.3 = active_q.is_filter_active(&suggestion.0, &suggestion.1);
        }
        values
    };

    let identifiers: Vec<(String, String)> = if q.is_empty() {
        Vec::new()
    } else {
        let mut hits: Vec<(i32, &String, Vec<usize>)> = graph
            .ids
            .iter()
            .filter_map(|id| {
                fuzzy_match(&q_lower, id).map(|(score, positions)| (score, id, positions))
            })
            .collect();
        hits.sort_by_key(|hit| Reverse(hit.0));
        hits.truncate(ID_CAP);
        hits.into_iter()
            .map(|(_, id, positions)| (id.clone(), highlight_positions(id, &positions)))
            .collect()
    };

    let rich = RICH.read().clone();
    let total = *result_total.read();
    let flat_active = mode == NavigatorMode::Flat;
    let tags_active = mode == NavigatorMode::Tags;

    rsx! {
        div {
            class: "nodes-workbench",
            "data-node-editor": "ready",
            "data-testid": "nodes-editor",
            div { class: "nodes-toolbar",
                div { class: "nodes-search-row",
                    input {
                        class: "filter nodes-search",
                        placeholder: "fuzzy ids · indexed fields · filters…",
                        value: "{query}",
                        oninput: move |event| {
                            query.set(event.value());
                            *GEN.write() += 1;
                            filter::ensure_field_index(ctx);
                            run_search(ctx);
                        },
                    }
                    div { class: "nodes-view-toggle", role: "group", "aria-label": "Node navigator view",
                        button {
                            class: if flat_active { "nodes-view-mode active" } else { "nodes-view-mode" },
                            "data-node-list-mode": "flat",
                            "aria-pressed": if flat_active { "true" } else { "false" },
                            onclick: move |_| set_navigator_mode(NavigatorMode::Flat),
                            "Flat"
                        }
                        button {
                            class: if tags_active { "nodes-view-mode active" } else { "nodes-view-mode" },
                            "data-node-list-mode": "tags",
                            "aria-pressed": if tags_active { "true" } else { "false" },
                            disabled: !server_backed,
                            title: if server_backed {
                                "Group nodes by their exact importer tags"
                            } else {
                                "Tag hierarchy requires a server-hosted graph"
                            },
                            onclick: move |_| set_navigator_mode(NavigatorMode::Tags),
                            "Tags"
                        }
                    }
                }

                if server_backed {
                    match &schema {
                        Some(Ok(schema)) => rsx! {
                            div {
                                class: "search-schema",
                                "aria-label": "Search fields from active importer schema",
                                "data-search-schema-source": "{schema.source.id}",
                                title: format!(
                                    "{} v{} · graph revision {}",
                                    schema.source.id,
                                    schema.source.version,
                                    schema.graph_revision,
                                ),
                                span {
                                    class: "search-schema-label",
                                    "data-search-schema-label": "true",
                                    "Search fields"
                                }
                                for field in schema.schema.fields.iter().filter(|field| field.searchable) {
                                    span {
                                        key: "{field.key}",
                                        class: "search-schema-key",
                                        title: if field.facetable {
                                            "field-qualified search; also available as a filter"
                                        } else {
                                            "field-qualified search"
                                        },
                                        "{field.key}:"
                                    }
                                }
                            }
                        },
                        Some(Err(error)) => rsx! {
                            div { class: "browse-error", "search schema unavailable: {error}" }
                        },
                        None => rsx! {},
                    }
                } else {
                    div { class: "more",
                        "Client-only graph: node-id matching works locally; indexed search, tags, metadata filters, and document lookup require a server-hosted graph."
                    }
                }

                if let Some(error) = search_error {
                    div { class: "browse-error", "{error}" }
                }
            }

            div { class: "nodes-editor-grid",
                aside { class: "nodes-nav", "data-testid": "node-sidebar",
                    div { class: "nodes-nav-head",
                        span { if q.is_empty() { "{mode.label()} nodes" } else { "Search results" } }
                        span { class: "nodes-nav-count", "{graph.ids.len()} total" }
                    }
                    div { class: "nodes-nav-scroll",
                        if q.is_empty() {
                            if mode == NavigatorMode::Flat {
                                { flat_navigation(&graph.ids, selected) }
                            } else if !server_backed {
                                div { class: "nodes-nav-state", "Tag hierarchy is unavailable for client-only graphs." }
                            } else {
                                match tag_contract {
                                    Some(false) => rsx! {
                                        div { class: "nodes-nav-state error",
                                            "This importer does not expose facetable tags, so a reliable hierarchy cannot be built."
                                        }
                                    },
                                    Some(true) => match hierarchy {
                                        Some(hierarchy) => tag_navigation(hierarchy, &graph.ids, selected),
                                        None => match filter::FIELD_INDEX.read().as_ref() {
                                            Some(Err(error)) => rsx! {
                                                div { class: "nodes-nav-state error", "tag index failed: {error}" }
                                            },
                                            _ => rsx! {
                                                div { class: "nodes-nav-state", Spinner { label: "loading tags…" } }
                                            },
                                        },
                                    },
                                    None => rsx! {
                                        if let Some(error) = tag_schema_error {
                                            div { class: "nodes-nav-state error", "tag schema unavailable: {error}" }
                                        } else {
                                            div { class: "nodes-nav-state", Spinner { label: "checking tag schema…" } }
                                        }
                                    },
                                }
                            }
                        } else {
                            div { class: "browse-results",
                                if !suggestions.is_empty() {
                                    div { class: "browse-group", "filters" }
                                    div { class: "sugg-row",
                                        for (field, value, count, active) in suggestions {
                                            {
                                                let filter_field = field.clone();
                                                let filter_value = value.clone();
                                                rsx! {
                                                    button {
                                                        key: "{field}:{value}",
                                                        class: if active { "sugg on" } else { "sugg" },
                                                        title: "toggle filter {field} = {value} ({count} nodes)",
                                                        onclick: move |_| {
                                                            filter::edit_filters(|query| {
                                                                query.toggle_field_filter(&filter_field, &filter_value)
                                                            });
                                                        },
                                                        span { class: "sugg-field", "{field}:" }
                                                        " {value} "
                                                        span { class: "sugg-count", "{count}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                if !identifiers.is_empty() {
                                    div { class: "browse-group", "identifiers" }
                                    nav { class: "queue", "aria-label": "Identifier matches",
                                        for (id, html) in identifiers {
                                            {
                                                let active = selected.read().as_deref() == Some(id.as_str());
                                                let click_id = id.clone();
                                                rsx! {
                                                    button {
                                                        key: "f:{id}",
                                                        class: if active { "queue-item active" } else { "queue-item" },
                                                        "data-node-id": id.clone(),
                                                        onclick: move |_| selected.set(Some(click_id.clone())),
                                                        span { class: "qi-id qi-fuzzy", dangerous_inner_html: "{html}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                if server_backed {
                                    div { class: "browse-group",
                                        "indexed"
                                        if *searching.read() {
                                            Spinner {}
                                        } else {
                                            span { class: "sugg-count", " {rich.len()} shown · {total} total" }
                                        }
                                    }
                                    if rich.is_empty() && !*searching.read() && !search_failed {
                                        div { class: "more", "no indexed matches" }
                                    }
                                    nav { class: "queue", "aria-label": "Indexed matches",
                                        for hit in rich {
                                            {
                                                let active = selected.read().as_deref() == Some(hit.id.as_str());
                                                let click_id = hit.id.clone();
                                                rsx! {
                                                    button {
                                                        key: "c:{hit.id}",
                                                        class: if active { "queue-item rich active" } else { "queue-item rich" },
                                                        "data-node-id": hit.id.clone(),
                                                        onclick: move |_| selected.set(Some(click_id.clone())),
                                                        span { class: "qi-id", "{hit.id}" }
                                                        if !hit.snippet.is_empty() {
                                                            span { class: "qi-snippet", dangerous_inner_html: "{hit.snippet}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                main {
                    class: "nodes-main",
                    "data-testid": "node-main",
                    "aria-live": "polite",
                    { focused_node(ctx) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_branch<'a>(branches: &'a [Arc<TagBranch>], path: &str) -> Option<&'a TagBranch> {
        for branch in branches {
            if branch.path == path {
                return Some(branch);
            }
            if let Some(found) = find_branch(&branch.children, path) {
                return Some(found);
            }
        }
        None
    }

    fn branch_at<'a>(branches: &'a [Arc<TagBranch>], path: &str) -> &'a TagBranch {
        find_branch(branches, path).unwrap_or_else(|| panic!("missing tag branch {path}"))
    }

    fn exact_nodes<'a>(hierarchy: &'a TagHierarchy, branch: &TagBranch) -> &'a [u32] {
        branch
            .group_index
            .and_then(|group_index| hierarchy.groups.get(group_index))
            .map_or(&[], |group| group.nodes.as_ref())
    }

    #[test]
    fn single_segment_tags_remain_root_groups_and_track_untagged_nodes() {
        let buckets = HashMap::from([
            ("operations".to_string(), vec![2, 0, 2, 99]),
            ("production".to_string(), vec![1, 2]),
        ]);
        let hierarchy = TagHierarchy::from_buckets(Some(&buckets), 4, '/');

        assert_eq!(hierarchy.roots.len(), 2);
        let ops = branch_at(&hierarchy.roots, "operations");
        assert_eq!(ops.label, "operations");
        assert!(ops.children.is_empty());
        assert_eq!(exact_nodes(&hierarchy, ops), &[0, 2]);
        assert_eq!(hierarchy.untagged.as_ref(), &[3]);
    }

    #[test]
    fn application_tag_hierarchy_builds_nested_paths_and_keeps_multi_tag_membership() {
        let buckets = HashMap::from([
            ("foo/bar/baz".to_string(), vec![0]),
            ("bee/bop/baz".to_string(), vec![0]),
            ("foo/bar/qux".to_string(), vec![1]),
        ]);
        let hierarchy = TagHierarchy::from_buckets(Some(&buckets), 3, '/');

        let foo = branch_at(&hierarchy.roots, "foo");
        let foo_bar = branch_at(&hierarchy.roots, "foo/bar");
        let foo_baz = branch_at(&hierarchy.roots, "foo/bar/baz");
        let bee_baz = branch_at(&hierarchy.roots, "bee/bop/baz");
        assert_eq!(foo.aggregate_count, 2);
        assert_eq!(foo_bar.aggregate_count, 2);
        assert_eq!(foo_baz.label, "baz");
        assert_eq!(bee_baz.label, "baz");
        assert_eq!(exact_nodes(&hierarchy, foo_baz), &[0]);
        assert_eq!(exact_nodes(&hierarchy, bee_baz), &[0]);
        assert_eq!(hierarchy.untagged.as_ref(), &[2]);
    }
}
