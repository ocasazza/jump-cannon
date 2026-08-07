//! Nodes panel — the unified browse/search surface.
//!
//! One input, three result surfaces (replaces the old separate Nodes +
//! Search panels):
//!   * empty query — the plain node list, titles only
//!   * typed query — **Identifiers**: client-side fuzzy match over node ids
//!     with the matched characters highlighted; **Indexed**: importer-defined
//!     search hits from `/search/rich`, with highlights when the importer
//!     declares a snippet-capable field
//!   * **Filters**: field values from the meta_summary index that match the
//!     query surface as toggle chips — the bridge from free-text search into
//!     the Filter panel's query model (`panels::filter::QUERY` + `sync_gpu`),
//!     so a search can be promoted into a live canvas filter.
//!
//! Indexed hits double as canvas highlights via `ctx.results` (same path the
//! old Search panel used). The active importer's searchable keys come from
//! `/graph/schema`; query failures are shown rather than treated as no hits.

use dioxus::prelude::*;
use panel_kit::Spinner;

use crate::panels::filter;
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

const DEBOUNCE_MS: u32 = 250;
const ID_CAP: usize = 40;
const LIST_CAP: usize = 300;
const SUGGESTION_CAP: usize = 8;

pub(crate) fn reset_for_graph_session() {
    *GEN.write() = GEN.peek().wrapping_add(1);
    RICH.write().clear();
    *SEARCH_ERROR.write() = None;
    *SEARCH_SCHEMA_SESSION.write() = SEARCH_SCHEMA_SESSION.peek().wrapping_add(1);
    *SEARCH_SCHEMA_STARTED.write() = false;
    *SEARCH_SCHEMA.write() = None;
}

// --- fuzzy matching -----------------------------------------------------------

/// Subsequence fuzzy match of `needle` (lowercase) against `hay`.
/// Returns (score, matched byte positions) — fzf-style bonuses: consecutive
/// runs and segment starts (`/ - _ space` boundaries) score higher, so
/// "gpl" prefers "graph-pipelines" over scattered letters.
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
            score += 3; // consecutive run
        }
        if pos == 0 || matches!(hay_bytes[pos - 1], b'/' | b'-' | b'_' | b' ' | b'.') {
            score += 2; // segment start
        }
        positions.push(pos);
        prev_match = Some(pos);
        hi = pos + 1;
    }
    // Shorter haystacks win ties — exacter matches first.
    score -= (hay.len() / 16) as i32;
    Some((score, positions))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `hay` with the matched byte positions wrapped in `<b>` — same highlight
/// vocabulary as the server-side content snippets.
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

// --- search dispatch ------------------------------------------------------------

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
    let gen = *GEN.peek();
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
        if *GEN.peek() != gen {
            return; // superseded by a newer keystroke
        }
        searching.set(true);
        let response = api::search_rich(&q, 60).await;
        if *GEN.peek() != gen || graph_session.peek().epoch != session.epoch {
            return;
        }
        match response {
            Ok(r) => {
                result_total.set(r.total as u32);
                results.set(r.results.iter().map(|h| h.id.clone()).collect());
                *RICH.write() = r.results;
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

// --- panel ----------------------------------------------------------------------

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
    let g = graph.read();
    let Some(g) = g.as_ref() else {
        return rsx! { div { class: "empty", "—" } };
    };
    let q = query.read().trim().to_string();
    let q_lower = q.to_lowercase();
    let server_backed = graph_session.read().is_server_backed();
    if server_backed {
        ensure_search_schema(ctx);
    }
    let schema = SEARCH_SCHEMA.read().clone();
    let search_error = SEARCH_ERROR.read().clone();
    let search_failed = search_error.is_some();

    // Filter-suggestion chips: meta_summary values containing the query.
    let suggestions: Vec<(String, String, usize, bool)> = if q.is_empty() || !server_backed {
        Vec::new()
    } else {
        let active_q = filter::QUERY.read();
        let mut v: Vec<(String, String, usize, bool)> = filter::FIELD_INDEX
            .read()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|fi| {
                fi.by_field
                    .iter()
                    .flat_map(|(field, values)| {
                        values
                            .iter()
                            .filter(|(val, _)| val.to_lowercase().contains(&q_lower))
                            .map(move |(val, nodes)| {
                                (field.clone(), val.clone(), nodes.len(), false)
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.2.cmp(&a.2));
        v.truncate(SUGGESTION_CAP);
        for s in v.iter_mut() {
            s.3 = active_q.is_filter_active(&s.0, &s.1);
        }
        v
    };

    // Identifiers: fuzzy over node ids, best-first.
    let identifiers: Vec<(String, String)> = if q.is_empty() {
        Vec::new()
    } else {
        let mut hits: Vec<(i32, &String, Vec<usize>)> = g
            .ids
            .iter()
            .filter_map(|id| fuzzy_match(&q_lower, id).map(|(s, p)| (s, id, p)))
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.truncate(ID_CAP);
        hits.into_iter()
            .map(|(_, id, p)| (id.clone(), highlight_positions(id, &p)))
            .collect()
    };

    let rich = RICH.read().clone();
    let total = *result_total.read();

    rsx! {
        div { class: "browse",
            input {
                class: "filter",
                placeholder: "fuzzy ids · indexed fields · filters…",
                value: "{query}",
                oninput: move |e| {
                    query.set(e.value());
                    *GEN.write() += 1;
                    filter::ensure_field_index(ctx);
                    run_search(ctx);
                },
            }

            if server_backed {
                match schema {
                    Some(Ok(schema)) => rsx! {
                        div {
                            class: "search-schema",
                            title: format!(
                                "{} v{} · graph revision {}",
                                schema.source.id,
                                schema.source.version,
                                schema.graph_revision,
                            ),
                            span { class: "search-schema-label", "{schema.source.name} search keys" }
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
            }

            if !server_backed {
                div { class: "more",
                    "Client-only graph: node-id matching works locally; indexed search, \
                     metadata filters, and document lookup require a server-hosted graph."
                }
            }

            if let Some(error) = search_error {
                div { class: "browse-error", "{error}" }
            }

            if q.is_empty() {
                // No query: the plain node list, exactly as before.
                nav { class: "queue",
                    for id in g.ids.iter().take(LIST_CAP) {
                        {
                            let active = selected.read().as_deref() == Some(id.as_str());
                            let id_click = id.clone();
                            rsx! {
                                button {
                                    key: "{id}",
                                    class: if active { "queue-item active" } else { "queue-item" },
                                    onclick: move |_| selected.set(Some(id_click.clone())),
                                    span { class: "qi-id", "{id}" }
                                }
                            }
                        }
                    }
                    if g.ids.len() > LIST_CAP {
                        div { class: "more", "… {g.ids.len() - LIST_CAP} more (type to fuzzy-find)" }
                    }
                }
            } else {
                div { class: "browse-results",
                    // Filter bridge: promote the search into field filters.
                    if !suggestions.is_empty() {
                        div { class: "browse-group", "filters" }
                        div { class: "sugg-row",
                            for (field , value , count , active) in suggestions {
                                {
                                    let f = field.clone();
                                    let v = value.clone();
                                    rsx! {
                                        button {
                                            key: "{field}:{value}",
                                            class: if active { "sugg on" } else { "sugg" },
                                            title: "toggle filter {field} = {value} ({count} nodes)",
                                            // Through `edit_filters` so the toggle
                                            // persists + stamps the snapshot source,
                                            // like a Filter-panel chip.
                                            onclick: move |_| {
                                                filter::edit_filters(|q| q.toggle_field_filter(&f, &v));
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

                    // Fuzzy node-id hits, matched characters highlighted.
                    if !identifiers.is_empty() {
                        div { class: "browse-group", "identifiers" }
                        nav { class: "queue",
                            for (id , html) in identifiers {
                                {
                                    let active = selected.read().as_deref() == Some(id.as_str());
                                    let id_click = id.clone();
                                    rsx! {
                                        button {
                                            key: "f:{id}",
                                            class: if active { "queue-item active" } else { "queue-item" },
                                            onclick: move |_| selected.set(Some(id_click.clone())),
                                            span { class: "qi-id qi-fuzzy", dangerous_inner_html: "{html}" }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Importer-indexed hits with optional highlighted snippets.
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
                        nav { class: "queue",
                            for h in rich {
                                {
                                    let active = selected.read().as_deref() == Some(h.id.as_str());
                                    let id_click = h.id.clone();
                                    rsx! {
                                        button {
                                            key: "c:{h.id}",
                                            class: if active { "queue-item rich active" } else { "queue-item rich" },
                                            onclick: move |_| selected.set(Some(id_click.clone())),
                                            span { class: "qi-id", "{h.id}" }
                                            if !h.snippet.is_empty() {
                                                span { class: "qi-snippet", dangerous_inner_html: "{h.snippet}" }
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
}
