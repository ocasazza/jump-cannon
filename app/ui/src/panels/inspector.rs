//! Inspector panel — Dioxus port of crates/graph-renderer/src/ui/inspector.rs
//! at commit 723af10 (`render_body` + its section helpers).
//!
//! Sections, top to bottom, mirroring the egui body renderer:
//!   - active-filter chip strip (show_active_filter_bar) — removable chips
//!     for every (field, value) pair, hidden when no filter is active;
//!   - empty state: the vault-wide tag browser (show_browse_tags) — fuzzy
//!     search over the FieldIndex tag buckets, top-N by frequency chips;
//!   - identity + metric rows (show_metadata) — id, idx, degree, pagerank,
//!     community, kcore (typed NodeMeta fields here instead of the egui
//!     metric-vector lookups; same keys + formatting);
//!   - badges (show_badges) — tags/doctype/folder + frontmatter chips via
//!     `crate::badges`, tinted with the node's community swatch;
//!   - leftover-frontmatter grid (show_frontmatter_section +
//!     frontmatter_grid.rs) — the fields the chip walker does NOT render;
//!   - community membership (show_community) + neighbours
//!     (show_neighbors_section) as clickable community-tinted pills; the
//!     Neighbours dropdown is skipped when it would duplicate the
//!     community sibling list, exactly like the egui `show_community`
//!     return-value contract. Neighbour pills additionally carry the link
//!     direction (in / out / both) derived from the directed edge list.
//!
//! The embedded page-content editor the egui inspector carried
//! (show_page_content) lives in the dedicated Document panel here —
//! `panels::document` owns the shared per-node editor state, so the two
//! surfaces don't double-render one editor.
//!
//! Clicking any node pill writes `ctx.selected` — the egui
//! `requested_selection` channel — which drives the meta fetch + renderer
//! highlight through the main.rs effects.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use panel_kit::badge::{Badge, BadgeAction, BadgeKind, BadgeSpec, Rgb};
use panel_kit::widgets::frontmatter::frontmatter_grid;
use panel_kit::widgets::pills::pill_list;
use panel_kit::Spinner;
use panel_kit_core::widgets::selection::{build_pills, neighbor_set, Pill, PillDirection};
use serde::{Deserialize, Serialize};

use crate::panels::filter;
use crate::{api, badges, Ctx};

// --- persisted state ---------------------------------------------------------------

const STORE_KEY: &str = "jc_inspector_v1";

/// localStorage shape — the egui app persisted `tag_browser_query` on
/// `AppState` via eframe Storage.
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    tag_query: String,
}

/// Empty-state tag-browser query (egui `AppState::tag_browser_query`).
static TAG_QUERY: GlobalSignal<String> = Signal::global(|| {
    LocalStorage::get::<Persisted>(STORE_KEY)
        .map(|p| p.tag_query)
        .unwrap_or_default()
});

fn persist() {
    let _ = LocalStorage::set(
        STORE_KEY,
        &Persisted {
            tag_query: TAG_QUERY.peek().clone(),
        },
    );
}

// --- community metric --------------------------------------------------------------

/// Vault-wide community metric vector (`/graph/metrics/community`). The
/// egui inspector read it from the App's metric cache; the Dioxus bootstrap
/// (graph_canvas::load) discards its copy after building the colour buffer,
/// so the inspector keeps its own one-shot fetch. `None` while in flight or
/// failed — sections degrade exactly like the egui "no community metric"
/// arm (neighbours become the de-facto community).
static COMMUNITY: GlobalSignal<Option<Vec<f32>>> = Signal::global(|| None);
static COMMUNITY_STARTED: GlobalSignal<bool> = Signal::global(|| false);

fn ensure_community_metric(ctx: Ctx) {
    if !ctx.graph_session.peek().is_server_backed() {
        return;
    }
    if *COMMUNITY_STARTED.peek() {
        return;
    }
    *COMMUNITY_STARTED.write() = true;
    spawn(async move {
        match api::metric("community").await {
            Ok(Some(v)) => *COMMUNITY.write() = Some(v),
            Ok(None) => tracing::warn!("[inspector] server has no community metric"),
            Err(e) => tracing::warn!("[inspector] community metric fetch failed: {e}"),
        }
    });
}

pub(crate) fn reset_for_graph_session() {
    *COMMUNITY_STARTED.write() = false;
    *COMMUNITY.write() = None;
}

/// Community swatch for node `idx` — the egui `node_color_for_key` path at
/// the default `ColorBy::Community` + `PaletteId::Tableau20`, reusing the
/// renderer's palette table so chips match the canvas colours.
fn community_tint(idx: u32) -> Option<Rgb> {
    let guard = COMMUNITY.read();
    let v = guard.as_ref()?;
    let bucket = (*v.get(idx as usize)?).max(0.0) as u32;
    let [r, g, b] = crate::render::data::palette_color(bucket);
    Some((
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ))
}

// --- badge action routing ------------------------------------------------------------

/// Route a badge outcome into the app channels — port of the egui
/// `dispatch_inspector_badge`. `body_target` is the node a content badge's
/// body-click focuses (the currently-inspected node for tag/doctype/folder
/// chips, the link target for wikilinks). Selection-set is the Dioxus
/// equivalent of `requested_focus_node`/`requested_navigate`: main.rs folds
/// it into the meta fetch + renderer highlight. Shared with the Document
/// panel's chip strip.
pub(crate) fn badge_dispatch(ctx: Ctx, body_target: String) -> EventHandler<BadgeAction> {
    EventHandler::new(move |a: BadgeAction| match a {
        BadgeAction::Toggle { field, value } | BadgeAction::AddFilter { field, value } => {
            filter::edit_filters(|q| q.toggle_field_filter(&field, &value));
        }
        BadgeAction::Clicked { .. } => {
            let mut selected = ctx.selected;
            selected.set(Some(body_target.clone()));
        }
        BadgeAction::Navigate { target } => {
            let mut selected = ctx.selected;
            selected.set(Some(target));
        }
        BadgeAction::OpenUrl { href } => open_url(&href),
        BadgeAction::Hovered { .. } => {}
    })
}

/// URL chip click → new tab (egui `requested_open_url` → `window.open`).
fn open_url(href: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(href, "_blank");
    }
}

// --- neighbours -----------------------------------------------------------------------

/// Wrapped row of clickable node pills over panel-kit's pill painter:
/// body-click selects the node (camera/highlight/meta follow from main.rs).
fn pill_row(ctx: Ctx, pills: Vec<Pill>, truncated: usize) -> Element {
    let mut selected = ctx.selected;
    pill_list(
        "node",
        pills,
        truncated,
        EventHandler::new(move |id: String| selected.set(Some(id))),
    )
}

// --- active filter strip ----------------------------------------------------------------

/// Active-filter chip strip — port of `show_active_filter_bar`. Hidden when
/// no filters are active; each chip's ✕ routes the same toggle the badge
/// clicks below use, so both fold into `QueryModel::toggle_field_filter`.
fn active_filter_strip() -> Element {
    let q = filter::QUERY.read();
    if q.active_filters.by_field.is_empty() {
        return rsx! {};
    }
    // Fields render in user-insertion order, not BTreeMap name order.
    let chips: Vec<(String, String)> = q
        .active_filters
        .insertion_order
        .iter()
        .filter_map(|f| q.active_filters.by_field.get(f).map(|vs| (f, vs)))
        .flat_map(|(f, vs)| vs.iter().map(move |v| (f.clone(), v.clone())))
        .collect();
    rsx! {
        div { class: "ins-strip",
            for (field, value) in chips {
                {
                    let kind = badges::badge_kind_for(&field);
                    let spec = BadgeSpec {
                        active: true,
                        with_x: true,
                        small: true,
                        ..BadgeSpec::new(field.clone(), value.clone(), kind)
                    };
                    rsx! {
                        Badge {
                            key: "{field}={value}",
                            spec,
                            on_action: move |a| {
                                if let BadgeAction::Toggle { field, value } = a {
                                    filter::edit_filters(|q| q.toggle_field_filter(&field, &value));
                                }
                            },
                        }
                    }
                }
            }
        }
        div { class: "ins-rule" }
    }
}

// --- empty-state tag browser ---------------------------------------------------------------

/// Subsequence fuzzy score — same scorer shape as panels/nodes.rs (the egui
/// side used SkimMatcherV2; fuzzy-matcher isn't a dep here, and the local
/// fzf-style bonuses rank close enough for an 80-chip browser).
fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    let hay_lower = hay.to_lowercase();
    let hay_bytes = hay_lower.as_bytes();
    let mut score = 0i32;
    let mut hi = 0usize;
    let mut prev: Option<usize> = None;
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
        if prev == Some(pos.wrapping_sub(1)) {
            score += 3;
        }
        if pos == 0 || matches!(hay_bytes[pos - 1], b'/' | b'-' | b'_' | b' ' | b'.') {
            score += 2;
        }
        prev = Some(pos);
        hi = pos + 1;
    }
    Some(score - (hay.len() / 16) as i32)
}

/// Empty-state vault-wide tag browser — port of `show_browse_tags`. Top of
/// the panel is a fuzzy search input; empty query falls back to "top N by
/// frequency". Click toggles the (tags, value) filter; active chips show
/// the halo.
fn browse_tags() -> Element {
    const MAX_CHIPS: usize = 80;

    let query = TAG_QUERY.read().clone();
    let trimmed = query.trim().to_string();

    let fi_guard = filter::FIELD_INDEX.read();
    let tag_buckets = fi_guard
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|fi| fi.by_field.get("tags"));
    let Some(tag_buckets) = tag_buckets else {
        let msg = if fi_guard.is_none() {
            "(no node selected — loading tags…)"
        } else {
            "(no node selected)"
        };
        return rsx! { div { class: "ins-note", "{msg}" } };
    };
    if tag_buckets.is_empty() {
        return rsx! { div { class: "ins-note", "(no node selected — vault has no tags)" } };
    }
    let total = tag_buckets.len();

    // Rank tags. Empty query → top-N by frequency desc, alpha asc as the
    // stable tiebreak. Non-empty query → fuzzy score desc, frequency desc.
    let active_q = filter::QUERY.read();
    let ranked: Vec<(String, usize, bool)> = if trimmed.is_empty() {
        let mut v: Vec<(&String, usize)> = tag_buckets
            .iter()
            .map(|(v, idxs)| (v, idxs.len()))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v.truncate(MAX_CHIPS);
        v.into_iter()
            .map(|(v, c)| (v.clone(), c, active_q.is_filter_active("tags", v)))
            .collect()
    } else {
        let needle = trimmed.to_lowercase();
        let mut scored: Vec<(i32, &String, usize)> = tag_buckets
            .iter()
            .filter_map(|(value, idxs)| fuzzy_score(&needle, value).map(|s| (s, value, idxs.len())))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
        scored.truncate(MAX_CHIPS);
        scored
            .into_iter()
            .map(|(_, v, c)| (v.clone(), c, active_q.is_filter_active("tags", v)))
            .collect()
    };
    drop(active_q);
    drop(fi_guard);

    let n_matches = ranked.len();
    rsx! {
        div { class: "ins-tagbrowse",
            div { class: "ins-tagtitle", "Tags ({total} total)" }
            div { class: "ins-search",
                input {
                    class: "filter",
                    placeholder: "filter tags…",
                    value: "{query}",
                    oninput: move |e| {
                        TAG_QUERY.write().clone_from(&e.value());
                        persist();
                    },
                }
                if !query.is_empty() {
                    button { class: "btn",
                        onclick: move |_| {
                            TAG_QUERY.write().clear();
                            persist();
                        },
                        "clear"
                    }
                }
            }
            if ranked.is_empty() {
                div { class: "ins-note", { format!("(no tags match {trimmed:?})") } }
            } else {
                if !trimmed.is_empty() {
                    div { class: "ins-count",
                        { format!("{n_matches} match{}", if n_matches == 1 { "" } else { "es" }) }
                    }
                }
                div { class: "ins-chips",
                    for (value, count, active) in ranked {
                    {
                        let label = format!("{value} ({count})");
                        let v = value.clone();
                        let spec = BadgeSpec {
                            active,
                            small: true,
                            ..BadgeSpec::new("tags", label, BadgeKind::Tag)
                        };
                        rsx! {
                            Badge {
                                key: "{value}",
                                spec,
                                on_action: move |a| {
                                    // The badge's value carries the composite
                                    // "name (count)" label; route the raw tag
                                    // value through the toggle instead so
                                    // subsequent renders re-match.
                                    if let BadgeAction::Toggle { .. } = a {
                                        let v = v.clone();
                                        filter::edit_filters(move |q| {
                                            q.toggle_field_filter("tags", &v)
                                        });
                                    }
                                },
                            }
                        }
                    }
                }
                }
            }
        }
    }
}

// --- panel -----------------------------------------------------------------------------------

pub(crate) fn panel(ctx: Ctx) -> Element {
    if !ctx.graph_session.read().is_server_backed() {
        let selected = ctx.selected.read().clone();
        return rsx! {
            div { class: "inspector",
                div { class: "empty",
                    if let Some(id) = selected {
                        "{id}\n\nClient-only graph: graph-api does not own this node, so metadata, \
                         neighbors, tags, and documents are unavailable."
                    } else {
                        "Client-only graph: select a node to see its generated id. \
                         Host the graph in graph-api to inspect metadata."
                    }
                }
            }
        };
    }
    // Both fetches are one-shot and shared — opening the inspector alone
    // must arm them (the egui app fetched both at boot).
    filter::ensure_field_index(ctx);
    ensure_community_metric(ctx);

    let sel = ctx.selected.read().clone();
    let body = match sel {
        // Empty state: the vault-wide tag browser, so the panel has a
        // useful default surface when nothing's selected.
        None => browse_tags(),
        Some(id) => node_view(ctx, id),
    };
    rsx! {
        div { class: "inspector",
            { active_filter_strip() }
            { body }
        }
    }
}

/// Everything below the chip strip for a selected node.
fn node_view(ctx: Ctx, id: String) -> Element {
    let busy = *ctx.meta_busy.read();
    let g_guard = ctx.graph.read();
    let g = g_guard.as_ref();
    // Dense index — None for stub/external ids that aren't in the loaded
    // graph (a wikilink navigate target the vault doesn't contain). The
    // meta sections still render from the server's stub NodeMeta.
    let idx = g.and_then(|g| g.id_to_idx.get(&id).copied());
    let tint = idx.and_then(community_tint);

    // Stale-fetch guard: when the cached meta's id has drifted from the
    // selected node's id (mid-fetch / stale click), render the skeleton
    // rather than badges for the wrong page — same guard as the egui
    // show_badges / show_frontmatter_section.
    let meta_guard = ctx.meta.read();
    let meta = meta_guard
        .as_ref()
        .filter(|m| m.id.is_empty() || m.id == id);

    // Meta-derived sections (identity rows, badges, frontmatter), built
    // eagerly while the guards are alive.
    let meta_el: Element = match meta {
        _ if busy => rsx! { div { class: "skeleton", Spinner { label: "loading node…" } } },
        None => rsx! { div { class: "empty", "node failed to load" } },
        Some(m) => {
            let has_frontmatter = !m.frontmatter_json.is_empty()
                && m.frontmatter_json != "{}"
                && m.frontmatter_json != "null";
            let badge_row = {
                let q = filter::QUERY.read();
                let is_active = |f: &str, v: &str| q.is_filter_active(f, v);
                let on_action = badge_dispatch(ctx, id.clone());
                // Tags / doctype / folder + frontmatter chips — hidden
                // entirely when the node carries none of them (egui
                // show_badges early-out).
                if m.tags.is_empty()
                    && m.folder.is_empty()
                    && m.doctype.is_none()
                    && !has_frontmatter
                {
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "ins-rule" }
                        div { class: "tags",
                            { badges::node_badges(m, &is_active, tint, on_action) }
                            if has_frontmatter {
                                { badges::frontmatter_chips(&m.frontmatter_json, &is_active, tint, on_action) }
                            }
                        }
                    }
                }
            };
            let facts = {
                let query = filter::QUERY.read();
                let is_active = |field: &str, value: &str| {
                    query.is_filter_active(field, value)
                };
                badges::node_facts(
                    m,
                    &is_active,
                    tint,
                    badge_dispatch(ctx, id.clone()),
                )
            };
            let provenance = badges::node_provenance(m);
            // Identity + metric rows — the egui show_metadata id header and
            // its metric keys (degree/pagerank/community/kcore), plus the
            // extra typed fields the /node/:id wire carries (title, path,
            // betweenness, wcc, in/out splits).
            rsx! {
                div { class: "ins-id", "{id}" }
                div { class: "kv", span { class: "k", "title" } span { class: "v", "{m.title}" } }
                div { class: "kv", span { class: "k", "path" } span { class: "v", "{m.path}" } }
                div { class: "metrics-grid",
                    if let Some(i) = idx {
                        div { class: "kv", span { class: "k", "idx" } span { class: "v", "{i}" } }
                    }
                    div { class: "kv", span { class: "k", "degree" } span { class: "v", "{m.degree} ({m.indegree} in / {m.outdegree} out)" } }
                    div { class: "kv", span { class: "k", "pagerank" } span { class: "v", { format!("{:.4}", m.pagerank) } } }
                    div { class: "kv", span { class: "k", "betweenness" } span { class: "v", { format!("{:.4}", m.betweenness) } } }
                    div { class: "kv", span { class: "k", "community" } span { class: "v", "{m.community}" } }
                    div { class: "kv", span { class: "k", "kcore" } span { class: "v", "{m.kcore}" } }
                    div { class: "kv", span { class: "k", "wcc" } span { class: "v", "{m.wcc}" } }
                    // Phase 0 water-energetic metrics — only visible when
                    // the layout pass has populated these fields.
                    if let Some(energy) = m.node_energy {
                        div { class: "kv ins-energy",
                            span { class: "k", "energy" }
                            span { class: "v",
                                { format!("{:.3} (attr {:.3} + rep {:.3})",
                                    energy,
                                    m.node_energy_attractive.unwrap_or(0.0),
                                    m.node_energy_repulsive.unwrap_or(0.0),
                                ) }
                            }
                        }
                    }
                    if let Some(var) = m.stability_variance {
                        {
                            let drift = m.stability_drift.unwrap_or(0.0);
                            let class_suffix = if drift > 0.01 { "ins-unstable" } else { "ins-stable" };
                            rsx! {
                                div { class: "kv ins-stability {class_suffix}",
                                    span { class: "k", "stability" }
                                    span { class: "v",
                                        { format!("var {:.6}  drift {:.4}",
                                            var, drift,
                                        ) }
                                    }
                                }
                            }
                        }
                    }
                    if m.anomaly_flag {
                        div { class: "kv ins-anomaly",
                            span { class: "k", "⚠ anomaly" }
                            span { class: "v", "edge stress exceeds threshold" }
                        }
                    }
                }
                { badge_row }
                { frontmatter_grid(&m.frontmatter_json) }
                { facts }
                { provenance }
            }
        }
    };

    // Community + neighbours — pure graph-buffer sections, available even
    // while the meta fetch is in flight (matches the egui inspector, which
    // rendered these from the metric/edge buffers).
    let graph_sections: Element = match (g, idx) {
        (Some(g), Some(idx)) => {
            let neighbors = neighbor_set(idx, &g.scene.edges);
            community_and_neighbors(ctx, &id, idx, &neighbors, &g.ids)
        }
        _ => rsx! {},
    };

    rsx! {
        { meta_el }
        { graph_sections }
    }
}

/// The Community section + (when it adds signal) the Neighbours section —
/// port of `show_community` / `show_neighbors_section` including the
/// fold-the-dropdowns rule: when the labelled community siblings equal the
/// neighbour set, or no community metric exists (neighbours ARE the
/// de-facto community), only one section renders.
fn community_and_neighbors(
    ctx: Ctx,
    id: &str,
    idx: u32,
    neighbors: &[(u32, PillDirection)],
    ids: &[String],
) -> Element {
    let comm_guard = COMMUNITY.read();
    let my_comm = comm_guard
        .as_ref()
        .and_then(|v| v.get(idx as usize))
        .map(|&f| f as i64);

    let Some(my_comm) = my_comm else {
        // No labelled community → neighbours are the de-facto community.
        let n = neighbors.len();
        let items: Vec<(u32, Option<PillDirection>)> =
            neighbors.iter().map(|&(i, d)| (i, Some(d))).collect();
        let (pills, truncated) = build_pills(&items, ids, community_tint);
        return rsx! {
            details { class: "ins-section", open: true, key: "comm-n:{id}",
                summary { "Community ({n} members, neighbours)" }
                if pills.is_empty() {
                    div { class: "ins-note", "(no neighbours)" }
                } else {
                    { pill_row(ctx, pills, truncated) }
                }
            }
        };
    };

    // Collect siblings (same community id, excluding self), stable order.
    let siblings: Vec<u32> = comm_guard
        .as_ref()
        .map(|v| {
            v.iter()
                .enumerate()
                .filter_map(|(i, &c)| (c as i64 == my_comm && i as u32 != idx).then_some(i as u32))
                .collect()
        })
        .unwrap_or_default();
    drop(comm_guard);

    let n_sib = siblings.len();
    let sib_items: Vec<(u32, Option<PillDirection>)> = siblings.iter().map(|&i| (i, None)).collect();
    let (sib_pills, sib_trunc) = build_pills(&sib_items, ids, community_tint);

    // The Neighbours dropdown only adds signal when its set diverges from
    // the labelled community siblings — identical sets would just repeat
    // the list under a different header.
    let neighbor_idxs: Vec<u32> = neighbors.iter().map(|&(i, _)| i).collect();
    let show_neighbors = siblings != neighbor_idxs;
    let n_nb = neighbors.len();
    let nb_items: Vec<(u32, Option<PillDirection>)> =
        neighbors.iter().map(|&(i, d)| (i, Some(d))).collect();
    let (nb_pills, nb_trunc) = build_pills(&nb_items, ids, community_tint);

    rsx! {
        details { class: "ins-section", open: true, key: "comm:{id}",
            summary { "Community {my_comm} ({n_sib} members)" }
            if sib_pills.is_empty() {
                div { class: "ins-note", "(no siblings)" }
            } else {
                { pill_row(ctx, sib_pills, sib_trunc) }
            }
        }
        if show_neighbors {
            details { class: "ins-section", open: true, key: "nb:{id}",
                summary { "Neighbours ({n_nb})" }
                if nb_pills.is_empty() {
                    div { class: "ins-note", "(no neighbours)" }
                } else {
                    { pill_row(ctx, nb_pills, nb_trunc) }
                }
            }
        }
    }
}
