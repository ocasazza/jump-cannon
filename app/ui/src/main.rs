//! Dioxus panel-workspace UI for jump-cannon.
//!
//! The workspace shell (floating/tiling panels, traffic lights, dock,
//! layout persistence) comes from `panel-kit`; this crate supplies the
//! jump-cannon panels: the graph canvas, node browser, full-text search,
//! node inspector, document editor, server progress feed, and settings.
//!
//! The frontend is a pure HTTP client of graph-api (`just dev-up` starts the
//! backend); the Tauri shell is a webview container with no IPC commands —
//! same architecture as apple-notes-ocr-flow.
//!
//! Build: `cargo tauri dev` inside `app/` (nix devshell provides trunk +
//! protoc + cargo-tauri).

mod api;
mod graph_canvas;
mod proto;

use std::collections::HashSet;

use dioxus::events::{Key, KeyboardEvent};
use dioxus::prelude::*;
use panel_kit::{LayoutBuilder, PanelWin, Spinner};
use serde::{Deserialize, Serialize};

use graph_canvas::GraphData;

fn main() {
    tracing_wasm::set_as_global_default();
    launch(App);
}

// --- panels -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
enum Panel {
    Graph,
    Nodes,
    Search,
    Inspector,
    Document,
    Progress,
    Settings,
    Help,
}

impl panel_kit::PanelKind for Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::Graph => "Graph",
            Panel::Nodes => "Nodes",
            Panel::Search => "Search",
            Panel::Inspector => "Inspector",
            Panel::Document => "Document",
            Panel::Progress => "Progress",
            Panel::Settings => "Settings",
            Panel::Help => "Help",
        }
    }
}

/// Default layout: the graph canvas dominates the left; browse/search in the
/// middle column; inspector + document on the right; progress along the bottom.
fn default_layout() -> Vec<PanelWin<Panel>> {
    let mut b = LayoutBuilder::new();
    vec![
        b.at(Panel::Graph, 12.0, 44.0, 700.0, 540.0),
        b.at(Panel::Nodes, 720.0, 44.0, 300.0, 264.0),
        b.at(Panel::Search, 720.0, 316.0, 300.0, 268.0),
        b.at(Panel::Inspector, 1028.0, 44.0, 400.0, 300.0),
        b.at(Panel::Document, 1028.0, 352.0, 400.0, 460.0),
        b.at(Panel::Progress, 12.0, 592.0, 700.0, 240.0),
        b.at(Panel::Settings, 720.0, 592.0, 300.0, 240.0),
        b.at(Panel::Help, 1028.0, 820.0, 400.0, 150.0),
    ]
}

// --- progress feed --------------------------------------------------------------

/// One server task folded out of the /progress event stream.
#[derive(Clone, PartialEq)]
struct TaskRow {
    id: u64,
    group: String,
    label: String,
    progress: Option<f32>,
    state: u8, // 0 running, 1 done, 2 failed
}

#[derive(Clone, PartialEq)]
struct LogRow {
    level: api::LogLevel,
    group: String,
    message: String,
}

fn fold_progress(tasks: &mut Vec<TaskRow>, logs: &mut Vec<LogRow>, ev: api::ProgressEvent) {
    use api::ProgressEvent as E;
    match ev {
        E::Start { id, group, label } => {
            tasks.push(TaskRow { id, group, label, progress: None, state: 0 });
        }
        E::SetProgress { id, progress } => {
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.progress = Some(progress);
            }
        }
        E::UpdateLabel { id, label } => {
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.label = label;
            }
        }
        E::Finish { id } => {
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.state = 1;
                t.progress = Some(1.0);
            }
        }
        E::Fail { id, reason } => {
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.state = 2;
                t.label = format!("{} — {}", t.label, reason);
            }
        }
        E::Log { level, group, message } => {
            logs.push(LogRow { level, group, message });
        }
    }
    // Keep the panel bounded: drop the oldest settled tasks + oldest logs.
    while tasks.len() > 40 {
        if let Some(i) = tasks.iter().position(|t| t.state != 0) {
            tasks.remove(i);
        } else {
            break;
        }
    }
    let overflow = logs.len().saturating_sub(120);
    if overflow > 0 {
        logs.drain(..overflow);
    }
}

// --- shared app context ----------------------------------------------------------

/// Every app signal, bundled `Copy` so panel renderers and handlers can grab
/// what they need without prop-drilling through components.
#[derive(Clone, Copy)]
struct Ctx {
    graph: Signal<Option<GraphData>>,
    load_error: Signal<Option<String>>,
    selected: Signal<Option<String>>,
    meta: Signal<Option<proto::NodeMeta>>,
    meta_busy: Signal<bool>,
    draft: Signal<String>,
    save_msg: Signal<String>,
    query: Signal<String>,
    results: Signal<Vec<String>>,
    result_total: Signal<u32>,
    searching: Signal<bool>,
    filter: Signal<String>,
    server: Signal<String>,
    tasks: Signal<Vec<TaskRow>>,
    logs: Signal<Vec<LogRow>>,
    view: graph_canvas::View,
}

async fn reload_graph(mut ctx: Ctx) {
    ctx.graph.set(None);
    ctx.load_error.set(None);
    match graph_canvas::load().await {
        Ok(g) => ctx.graph.set(Some(g)),
        Err(e) => ctx.load_error.set(Some(e)),
    }
}

fn render_markdown(md: &str) -> String {
    let parser = pulldown_cmark::Parser::new(md);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

// --- app ---------------------------------------------------------------------

#[component]
fn App() -> Element {
    let ws = panel_kit::use_workspace("jc_layout", default_layout);

    let ctx = Ctx {
        graph: use_signal(|| None),
        load_error: use_signal(|| None),
        selected: use_signal(|| None),
        meta: use_signal(|| None),
        meta_busy: use_signal(|| false),
        draft: use_signal(String::new),
        save_msg: use_signal(String::new),
        query: use_signal(String::new),
        results: use_signal(Vec::new),
        result_total: use_signal(|| 0),
        searching: use_signal(|| false),
        filter: use_signal(String::new),
        server: use_signal(api::server_url),
        tasks: use_signal(Vec::new),
        logs: use_signal(Vec::new),
        view: graph_canvas::View {
            zoom: use_signal(|| 1.0),
            pan: use_signal(|| (0.0, 0.0)),
            drag: use_signal(|| None),
        },
    };

    // Resilient initial load: retry until the backend answers, so a server
    // that's still indexing (or starting up) self-heals instead of leaving
    // the canvas permanently empty.
    {
        let mut graph = ctx.graph;
        let mut load_error = ctx.load_error;
        use_future(move || async move {
            loop {
                match graph_canvas::load().await {
                    Ok(g) => {
                        graph.set(Some(g));
                        load_error.set(None);
                        break;
                    }
                    Err(e) => {
                        load_error.set(Some(e));
                        gloo_timers::future::TimeoutFuture::new(1500).await;
                    }
                }
            }
        });
    }

    // Selection -> fetch full node meta + seed the document editor.
    {
        let selected = ctx.selected;
        let mut meta = ctx.meta;
        let mut meta_busy = ctx.meta_busy;
        let mut draft = ctx.draft;
        let mut save_msg = ctx.save_msg;
        use_effect(move || {
            let sel = selected.read().clone();
            if let Some(id) = sel {
                meta_busy.set(true);
                save_msg.set(String::new());
                spawn(async move {
                    match api::node_meta(&id).await {
                        Ok(m) => {
                            draft.set(m.body.clone());
                            meta.set(Some(m));
                        }
                        Err(e) => {
                            meta.set(None);
                            save_msg.set(format!("load failed: {e}"));
                        }
                    }
                    meta_busy.set(false);
                });
            }
        });
    }

    // Poll the server progress log (vault reloads, search reindex, …).
    {
        let mut tasks = ctx.tasks;
        let mut logs = ctx.logs;
        use_future(move || async move {
            let mut since = 0u64;
            loop {
                if let Ok(resp) = api::progress(since).await {
                    since = resp.next_seq;
                    if !resp.events.is_empty() {
                        let mut t = tasks.read().clone();
                        let mut l = logs.read().clone();
                        for st in resp.events {
                            fold_progress(&mut t, &mut l, st.event);
                        }
                        tasks.set(t);
                        logs.set(l);
                    }
                }
                gloo_timers::future::TimeoutFuture::new(1000).await;
            }
        });
    }

    // Canvas redraw: reactively on data/view/selection changes…
    {
        let graph = ctx.graph;
        let selected = ctx.selected;
        let results = ctx.results;
        let zoom = ctx.view.zoom;
        let pan = ctx.view.pan;
        use_effect(move || {
            let g = graph.read();
            let sel = selected.read();
            let res = results.read();
            let z = *zoom.read();
            let p = *pan.read();
            if let Some(g) = g.as_ref() {
                let sel_idx = sel.as_ref().and_then(|id| g.id_to_idx.get(id)).copied();
                let hl: HashSet<u32> =
                    res.iter().filter_map(|id| g.id_to_idx.get(id)).copied().collect();
                graph_canvas::draw(g, sel_idx, &hl, z, p);
            }
        });
        // …plus a slow ticker to catch panel resizes (panel geometry isn't a
        // dependency of the effect above; the canvas re-fits on the next tick).
        use_future(move || async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(400).await;
                let g = graph.peek();
                if let Some(g) = g.as_ref() {
                    let sel = selected.peek();
                    let sel_idx = sel.as_ref().and_then(|id| g.id_to_idx.get(id)).copied();
                    let hl: HashSet<u32> = results
                        .peek()
                        .iter()
                        .filter_map(|id| g.id_to_idx.get(id))
                        .copied()
                        .collect();
                    graph_canvas::draw(g, sel_idx, &hl, *zoom.peek(), *pan.peek());
                }
            }
        });
    }

    let g_now = ctx.graph.read().clone();
    let busy_tasks = ctx.tasks.read().iter().any(|t| t.state == 0);
    let mode_label = match ws.effective_mode() {
        panel_kit::Mode::Tiling => "tiling",
        panel_kit::Mode::Floating => "floating",
    };

    rsx! {
        style { {panel_kit::CSS} }
        style { {include_str!("../assets/app.css")} }
        div {
            class: ws.root_class(),
            tabindex: "0",
            autofocus: true,
            onmousemove: move |e| ws.handle_mouse_move(&e),
            onmouseup: move |_| ws.handle_mouse_up(),

            header { class: "topbar",
                h1 { "JUMP CANNON" }
                span { class: "hint",
                    if let Some(g) = g_now.as_ref() {
                        { format!("{mode_label} · {} nodes · {} edges · {} communities",
                            g.n_nodes, g.n_edges, g.num_communities) }
                    } else {
                        "{mode_label} · connecting…"
                    }
                }
                if busy_tasks {
                    span { class: "activity", "● server working…" }
                } else if g_now.is_none() {
                    span { class: "activity", "○ waiting for graph-api" }
                }
            }

            {ws.render(move |kind, maximized| panel_body(kind, maximized, ctx))}

            {ws.dock()}
        }
    }
}

// --- panel bodies ----------------------------------------------------------------

fn panel_body(kind: Panel, _maximized: bool, ctx: Ctx) -> Element {
    match kind {
        Panel::Graph => {
            if ctx.graph.read().is_some() {
                graph_canvas::graph_canvas(ctx.graph, ctx.selected, ctx.view)
            } else if let Some(e) = ctx.load_error.read().clone() {
                rsx! { div { class: "skeleton", Spinner { label: "retrying: {e}" } } }
            } else {
                rsx! { div { class: "skeleton", Spinner { label: "loading graph…" } } }
            }
        }
        Panel::Nodes => nodes_panel(ctx),
        Panel::Search => search_panel(ctx),
        Panel::Inspector => inspector_panel(ctx),
        Panel::Document => document_panel(ctx),
        Panel::Progress => progress_panel(ctx),
        Panel::Settings => settings_panel(ctx),
        Panel::Help => rsx! {
            div { class: "help",
                p { "canvas: drag pan · wheel zoom · click select" }
                p { "search: Enter to run · click result to inspect" }
                hr {}
                p { "🔴 tiling ⇄ floating" }
                p { "🟡 minimize → dock" }
                p { "🟢 maximize ⇄ restore" }
            }
        },
    }
}

/// Browse the raw node list (same order as the canvas buffers), with a
/// substring filter. Capped render — the canvas and search are the real
/// navigation surfaces for big vaults.
fn nodes_panel(ctx: Ctx) -> Element {
    let Ctx { graph, mut selected, mut filter, .. } = ctx;
    let g = graph.read();
    let Some(g) = g.as_ref() else {
        return rsx! { div { class: "empty", "—" } };
    };
    let needle = filter.read().to_lowercase();
    let matched: Vec<&String> = g
        .ids
        .iter()
        .filter(|id| needle.is_empty() || id.to_lowercase().contains(&needle))
        .collect();
    let total = matched.len();
    let shown: Vec<String> = matched.into_iter().take(300).cloned().collect();
    rsx! {
        div { class: "browse",
            input {
                class: "filter",
                placeholder: "filter {g.ids.len()} nodes…",
                value: "{filter}",
                oninput: move |e| filter.set(e.value()),
            }
            nav { class: "queue",
                for id in shown {
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
                if total > 300 {
                    div { class: "more", "… {total - 300} more (refine the filter)" }
                }
            }
        }
    }
}

/// Full-text search via /search (Tantivy BM25 behind graph-api). Results
/// double as canvas highlights.
fn search_panel(ctx: Ctx) -> Element {
    let Ctx { mut query, mut results, mut result_total, mut searching, mut selected, .. } = ctx;
    let mut run = move || {
        let q = query.read().clone();
        if q.trim().is_empty() {
            results.set(Vec::new());
            result_total.set(0);
            return;
        }
        searching.set(true);
        spawn(async move {
            match api::search(&q, 100).await {
                Ok(r) => {
                    result_total.set(r.total);
                    results.set(r.ids);
                }
                Err(_) => {
                    result_total.set(0);
                    results.set(Vec::new());
                }
            }
            searching.set(false);
        });
    };
    let mut run_key = run;
    let res = results.read().clone();
    let total = *result_total.read();
    rsx! {
        div { class: "search",
            div { class: "search-bar",
                input {
                    class: "filter",
                    placeholder: "full-text search…",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Enter { run_key(); }
                    },
                }
                button { class: "btn", onclick: move |_| run(), "go" }
            }
            if *searching.read() {
                div { class: "skeleton", Spinner { label: "searching…" } }
            } else if res.is_empty() {
                div { class: "empty", "no results" }
            } else {
                div { class: "result-count", "{res.len()} shown · {total} total · highlighted on canvas" }
                nav { class: "queue",
                    for id in res {
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
                }
            }
        }
    }
}

/// Node metadata: identity, tags, graph metrics, raw frontmatter.
fn inspector_panel(ctx: Ctx) -> Element {
    let Ctx { meta, meta_busy, selected, .. } = ctx;
    if *meta_busy.read() {
        return rsx! { div { class: "skeleton", Spinner { label: "loading node…" } } };
    }
    let m = meta.read();
    let Some(m) = m.as_ref() else {
        return rsx! { div { class: "empty",
            if selected.read().is_some() { "node failed to load" } else { "select a node" }
        } };
    };
    let fm_pretty = serde_json::from_str::<serde_json::Value>(&m.frontmatter_json)
        .ok()
        .filter(|v| v.as_object().map(|o| !o.is_empty()).unwrap_or(true))
        .and_then(|v| serde_json::to_string_pretty(&v).ok());
    rsx! {
        div { class: "inspector",
            h2 { class: "node-title", "{m.title}" }
            div { class: "kv", span { class: "k", "path" } span { class: "v", "{m.path}" } }
            div { class: "kv", span { class: "k", "folder" } span { class: "v", "{m.folder}" } }
            if let Some(dt) = m.doctype.as_ref() {
                div { class: "kv", span { class: "k", "doctype" } span { class: "v", "{dt}" } }
            }
            if !m.tags.is_empty() {
                div { class: "tags",
                    for t in m.tags.iter() {
                        span { key: "{t}", class: "tag", "#{t}" }
                    }
                }
            }
            div { class: "metrics-grid",
                div { class: "kv", span { class: "k", "degree" } span { class: "v", "{m.degree} ({m.indegree} in / {m.outdegree} out)" } }
                div { class: "kv", span { class: "k", "pagerank" } span { class: "v", { format!("{:.5}", m.pagerank) } } }
                div { class: "kv", span { class: "k", "betweenness" } span { class: "v", { format!("{:.5}", m.betweenness) } } }
                div { class: "kv", span { class: "k", "k-core" } span { class: "v", "{m.kcore}" } }
                div { class: "kv", span { class: "k", "community" } span { class: "v", "{m.community}" } }
                div { class: "kv", span { class: "k", "wcc" } span { class: "v", "{m.wcc}" } }
            }
            if let Some(fm) = fm_pretty {
                details {
                    summary { "frontmatter" }
                    pre { class: "code", "{fm}" }
                }
            }
        }
    }
}

/// Markdown body editor + rendered preview. Saves through PUT /vault/page
/// (body-only; on-disk frontmatter is preserved by the server).
fn document_panel(ctx: Ctx) -> Element {
    let Ctx { meta, mut draft, mut save_msg, .. } = ctx;
    let m = meta.read();
    let Some(m) = m.as_ref() else {
        return rsx! { div { class: "empty", "select a node" } };
    };
    let path = m.path.clone();
    let external = m.body.is_empty() && m.doctype.as_deref() == Some("external");
    if external {
        return rsx! { div { class: "empty", "external node — no file on disk" } };
    }
    let msg = save_msg.read().clone();
    rsx! {
        div { class: "doc",
            textarea {
                class: "md",
                spellcheck: false,
                value: "{draft}",
                oninput: move |e| draft.set(e.value()),
            }
            div { class: "doc-actions",
                button { class: "btn",
                    onclick: move |_| {
                        let path = path.clone();
                        spawn(async move {
                            save_msg.set("saving…".into());
                            match api::put_page(&path, &draft.read().clone()).await {
                                Ok(r) if r.ok => save_msg.set("saved ✓ (vault reload will follow)".into()),
                                Ok(r) => save_msg.set(format!("rejected: {}", r.error.unwrap_or_default())),
                                Err(e) => save_msg.set(format!("save failed: {e}")),
                            }
                        });
                    },
                    "Save"
                }
                span { class: "save-msg", "{msg}" }
            }
            details {
                summary { "preview" }
                div { class: "rendered-md", dangerous_inner_html: render_markdown(&draft.read()) }
            }
        }
    }
}

/// Live server progress: vault reload stages, search reindex, layout jobs —
/// the same event log the egui footer renders, polled from /progress.
fn progress_panel(ctx: Ctx) -> Element {
    let Ctx { tasks, logs, .. } = ctx;
    let ts = tasks.read().clone();
    let ls = logs.read().clone();
    rsx! {
        div { class: "jobs",
            if ts.is_empty() && ls.is_empty() {
                div { class: "empty", "no server activity yet" }
            }
            for t in ts.iter().rev() {
                div { key: "{t.id}", class: "job-row",
                    if t.state == 0 {
                        Spinner {}
                    } else {
                        span { class: if t.state == 1 { "job-glyph done" } else { "job-glyph error" },
                            { if t.state == 1 { "●" } else { "✕" } } }
                    }
                    span { class: "job-stage", "{t.group}" }
                    span { class: "job-id", "{t.label}" }
                    if let Some(p) = t.progress {
                        span { class: "job-secs", { format!("{:.0}%", p * 100.0) } }
                    }
                }
            }
            if !ls.is_empty() {
                div { class: "log-head", "log" }
                for (i, l) in ls.iter().rev().take(30).enumerate() {
                    div { key: "{i}", class: "log-row",
                        span {
                            class: match l.level {
                                api::LogLevel::Error => "log-level error",
                                api::LogLevel::Warn => "log-level warn",
                                api::LogLevel::Info => "log-level",
                            },
                            "{l.group}"
                        }
                        span { class: "log-msg", "{l.message}" }
                    }
                }
            }
        }
    }
}

/// Server connection + graph stats. The URL is persisted in localStorage so
/// the app can point at a remote graph-api (LAN/Tailscale) like the OCR app.
fn settings_panel(ctx: Ctx) -> Element {
    let Ctx { mut server, graph, .. } = ctx;
    let g = graph.read().clone();
    rsx! {
        div { class: "controls",
            div { class: "server",
                input {
                    value: "{server}",
                    oninput: move |e| server.set(e.value()),
                }
                button { class: "btn",
                    onclick: move |_| {
                        api::set_server_url(&server.read());
                        spawn(reload_graph(ctx));
                    },
                    "Connect"
                }
            }
            if let Some(g) = g {
                div { class: "stats",
                    div { class: "kv", span { class: "k", "nodes" } span { class: "v", "{g.n_nodes}" } }
                    div { class: "kv", span { class: "k", "edges" } span { class: "v", "{g.n_edges}" } }
                    div { class: "kv", span { class: "k", "communities" } span { class: "v", "{g.num_communities}" } }
                    div { class: "kv", span { class: "k", "components" } span { class: "v", "{g.num_wcc}" } }
                }
            }
            div { class: "note",
                "backend: graph-api (axum) — start with `just dev-up`; "
                "compute/HPC layers stay behind it and are not part of this app."
            }
        }
    }
}
