//! Dioxus panel-workspace UI for jump-cannon.
//!
//! The workspace shell (floating/tiling panels, traffic lights, dock,
//! layout persistence) comes from `panel-kit`; this crate supplies the
//! jump-cannon panels: the graph canvas, node browser, full-text search,
//! node inspector, document editor, server progress feed, and settings.
//!
//! The frontend is a pure HTTP client of graph-api (`just dev-up` starts the
//! backend); the Tauri shell is a webview container with no IPC commands —
//! same architecture as snake-pit.
//!
//! Build: `cargo tauri dev` inside `app/` (nix devshell provides trunk +
//! protoc + cargo-tauri).

mod anchored;
mod api;
mod appstate;
mod badges;
mod graph_canvas;
mod palette;
mod panels;
mod proto;
mod render;
mod worker;

use std::collections::HashSet;

use dioxus::events::{Key, KeyboardEvent};
use dioxus::prelude::*;
use panel_kit::{LayoutBuilder, PanelWin, Spinner};
use serde::{Deserialize, Serialize};

use graph_canvas::GraphData;

/// Provenance of the graph currently mounted in the browser.  This is
/// deliberately separate from where an expression was evaluated: a graph
/// returned by `POST /generate` is still browser-owned until graph-api hosts
/// it as an active snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GraphOrigin {
    Server { endpoint: String },
    ClientGenerated { evaluator: String },
}

/// Identity for browser state derived from one graph load.  `epoch` changes
/// on every replacement (including same-server reloads); `graph_revision` is
/// the server's topology identity and is absent for browser-only graphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GraphSession {
    pub(crate) epoch: u64,
    pub(crate) graph_revision: Option<u64>,
    pub(crate) origin: GraphOrigin,
}

impl GraphSession {
    fn loading_server(endpoint: String) -> Self {
        Self {
            epoch: 0,
            graph_revision: None,
            origin: GraphOrigin::Server { endpoint },
        }
    }

    pub(crate) fn is_server_backed(&self) -> bool {
        matches!(self.origin, GraphOrigin::Server { .. })
    }

    pub(crate) fn short_label(&self) -> String {
        match &self.origin {
            GraphOrigin::Server { .. } => self
                .graph_revision
                .filter(|r| *r != 0)
                .map(|r| format!("server graph r{r}"))
                .unwrap_or_else(|| "server graph".to_string()),
            GraphOrigin::ClientGenerated { evaluator } => {
                format!("generated locally ({evaluator}; client-only)")
            }
        }
    }

    fn replaced_by_server_revision(&self, advertised_revision: u64) -> bool {
        self.is_server_backed()
            && advertised_revision != 0
            && self
                .graph_revision
                .is_some_and(|revision| revision != advertised_revision)
    }
}

#[cfg(test)]
mod graph_session_tests {
    use super::{GraphOrigin, GraphSession};

    #[test]
    fn progress_revision_only_invalidates_a_loaded_server_graph() {
        let server = GraphSession {
            epoch: 3,
            graph_revision: Some(17),
            origin: GraphOrigin::Server {
                endpoint: "http://graph-api".into(),
            },
        };
        assert!(!server.replaced_by_server_revision(0));
        assert!(!server.replaced_by_server_revision(17));
        assert!(server.replaced_by_server_revision(18));

        let loading = GraphSession {
            graph_revision: None,
            ..server.clone()
        };
        assert!(!loading.replaced_by_server_revision(18));

        let generated = GraphSession {
            origin: GraphOrigin::ClientGenerated {
                evaluator: "browser".into(),
            },
            ..server
        };
        assert!(!generated.replaced_by_server_revision(18));
    }
}

fn main() {
    // WARN, not the tracing-wasm default (TRACE): at TRACE every Dioxus VDOM
    // diff and signal write hits console.log — tens of thousands of calls per
    // second, which reads as a frozen UI.
    tracing_wasm::set_as_global_default_with_config(
        tracing_wasm::WASMLayerConfigBuilder::new()
            .set_max_level(tracing::Level::WARN)
            .build(),
    );
    // A wasm panic kills the whole app silently (frozen UI, dead canvas).
    // Surface it: console.error + a red banner so the failure names itself
    // instead of presenting as "the app froze".
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        web_sys::console::error_1(&msg.clone().into());
        // JS stack at the panic point: dev-build wasm keeps function names,
        // so this names the actual call chain (panic locations alone can be
        // misattributed across inlined frames).
        let err = js_sys::Error::new("panic stack");
        if let Ok(stack) = js_sys::Reflect::get(&err, &"stack".into()) {
            web_sys::console::error_1(&stack);
        }
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(div) = doc.create_element("div") {
                let _ = div.set_attribute(
                    "style",
                    "position:fixed;top:0;left:0;right:0;z-index:99999;background:#7a1010;\
                     color:#fff;font:12px ui-monospace,monospace;padding:6px 10px;white-space:pre-wrap;",
                );
                div.set_text_content(Some(&format!(
                    "wasm panic — the app is dead, reload the window:\n{msg}"
                )));
                if let Some(body) = doc.body() {
                    let _ = body.append_child(&div);
                }
            }
        }
    }));
    // Boot marker for crates/test-browser: it greps the console for this
    // exact line to know the wasm app booted. console.log directly (NOT
    // tracing) — tracing is filtered to WARN above.
    web_sys::console::log_1(&"[jump-cannon-ui] boot".into());
    launch(App);
}

// --- panels -------------------------------------------------------------------

/// One variant per panel. The first block is app plumbing; the second block
/// mirrors the egui app's footer tray — its `Section` enum plus the filter
/// chip strip (`crates/graph-renderer/src/ui/state.rs::{Section, PanelId}`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub(crate) enum Panel {
    Graph,
    Nodes,
    Inspector,
    Document,
    Progress,
    Settings,
    Help,
    // egui-tray parity panels (see docs/dioxus-migration.md phases 2-3):
    Layout,
    Style,
    Camera,
    Filter,
    Metrics,
    Instances,
    Generate,
    Timeline,
    Debug,
}

impl panel_kit::PanelKind for Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::Graph => "Graph",
            Panel::Nodes => "Nodes",
            Panel::Inspector => "Inspector",
            Panel::Document => "Document",
            Panel::Progress => "Progress",
            Panel::Settings => "Settings",
            Panel::Help => "Help",
            Panel::Layout => "Layout",
            Panel::Style => "Style",
            Panel::Camera => "Camera",
            Panel::Filter => "Filter",
            Panel::Metrics => "Metrics",
            Panel::Instances => "Instances",
            Panel::Generate => "Generate",
            Panel::Timeline => "Timeline",
            Panel::Debug => "Debug",
        }
    }
}

/// Default layout: the graph canvas dominates the left; browse/search in the
/// middle column; inspector + document on the right; progress along the bottom.
fn default_layout() -> Vec<PanelWin<Panel>> {
    let mut b = LayoutBuilder::new();
    // The tray-parity panels start minimized: the dock is this app's
    // equivalent of the egui footer launcher row — click a chip to open.
    fn min(b: &mut LayoutBuilder, kind: Panel, x: f64, y: f64, w: f64, h: f64) -> PanelWin<Panel> {
        let mut p = b.at(kind, x, y, w, h);
        p.state = panel_kit::WinState::Minimized;
        p
    }
    let b = &mut b;
    let mut v = vec![
        min(b, Panel::Layout, 740.0, 60.0, 340.0, 480.0),
        min(b, Panel::Style, 760.0, 80.0, 320.0, 440.0),
        min(b, Panel::Camera, 780.0, 100.0, 320.0, 360.0),
        min(b, Panel::Filter, 800.0, 120.0, 340.0, 420.0),
        min(b, Panel::Metrics, 840.0, 160.0, 320.0, 380.0),
        min(b, Panel::Instances, 860.0, 180.0, 360.0, 420.0),
        min(b, Panel::Generate, 880.0, 200.0, 360.0, 440.0),
        min(b, Panel::Timeline, 900.0, 220.0, 380.0, 320.0),
        min(b, Panel::Debug, 920.0, 240.0, 320.0, 360.0),
    ];
    // Floating mode: the graph view starts dominant (~2x everything else);
    // clamp_to_viewport pulls the right column in on narrower screens.
    // Tiling mode: the graph starts full-width × 3 rows (with_tile replaces
    // the old .panel-graph CSS override; the grip resizes it in snapped
    // steps now).
    v.extend([
        b.at(Panel::Graph, 12.0, 44.0, 920.0, 620.0).with_tile(4, 3),
        b.at(Panel::Nodes, 940.0, 44.0, 290.0, 620.0),
        b.at(Panel::Inspector, 1238.0, 44.0, 330.0, 300.0),
        b.at(Panel::Document, 1238.0, 352.0, 330.0, 460.0),
        b.at(Panel::Progress, 12.0, 672.0, 920.0, 200.0),
        b.at(Panel::Settings, 940.0, 672.0, 290.0, 200.0),
        b.at(Panel::Help, 1238.0, 820.0, 330.0, 150.0),
    ]);
    v
}

// --- progress feed --------------------------------------------------------------

/// One server task folded out of the /progress event stream.
#[derive(Clone, PartialEq)]
pub(crate) struct TaskRow {
    id: u64,
    group: String,
    label: String,
    progress: Option<f32>,
    state: u8, // 0 running, 1 done, 2 failed
}

#[derive(Clone, PartialEq)]
pub(crate) struct LogRow {
    level: api::LogLevel,
    group: String,
    message: String,
}

fn fold_progress(tasks: &mut Vec<TaskRow>, logs: &mut Vec<LogRow>, ev: api::ProgressEvent) {
    use api::ProgressEvent as E;
    match ev {
        E::Start { id, group, label } => {
            tasks.push(TaskRow {
                id,
                group,
                label,
                progress: None,
                state: 0,
            });
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
        E::Log {
            level,
            group,
            message,
        } => {
            logs.push(LogRow {
                level,
                group,
                message,
            });
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
pub(crate) struct Ctx {
    pub(crate) graph: Signal<Option<GraphData>>,
    pub(crate) graph_session: Signal<GraphSession>,
    pub(crate) load_error: Signal<Option<String>>,
    pub(crate) selected: Signal<Option<String>>,
    pub(crate) meta: Signal<Option<proto::NodeMeta>>,
    pub(crate) meta_busy: Signal<bool>,
    pub(crate) draft: Signal<String>,
    pub(crate) save_msg: Signal<String>,
    pub(crate) query: Signal<String>,
    pub(crate) results: Signal<Vec<String>>,
    pub(crate) result_total: Signal<u32>,
    pub(crate) searching: Signal<bool>,
    pub(crate) server: Signal<String>,
    pub(crate) tasks: Signal<Vec<TaskRow>>,
    pub(crate) logs: Signal<Vec<LogRow>>,
}

/// Clear every value whose node indices or server ownership came from the old
/// graph. Persisted user preferences remain; caches and live masks do not.
fn invalidate_graph_derived_state(mut ctx: Ctx) {
    ctx.selected.set(None);
    ctx.meta.set(None);
    ctx.meta_busy.set(false);
    ctx.draft.set(String::new());
    ctx.save_msg.set(String::new());
    ctx.query.set(String::new());
    ctx.results.set(Vec::new());
    ctx.result_total.set(0);
    ctx.searching.set(false);
    panels::nodes::reset_for_graph_session();
    panels::filter::reset_for_graph_session();
    panels::inspector::reset_for_graph_session();
    panels::style::reset_for_graph_session(None);
    panels::timeline::reset_for_graph_session();
    anchored::reset_for_graph_session();
    palette::reset_for_graph_session();
    render::set_selected_node(None);
    render::set_search_highlights(None);
}

fn begin_server_graph_load(mut ctx: Ctx) -> u64 {
    let endpoint = api::server_url();
    let connection_changed = match &ctx.graph_session.peek().origin {
        GraphOrigin::Server { endpoint: old } => old != &endpoint,
        // A client-only session no longer retains the endpoint its cached
        // documents came from. Clear them before reattaching to any server;
        // preserving a dirty buffer under a same-named node is less safe.
        GraphOrigin::ClientGenerated { .. } => true,
    };
    let epoch = ctx.graph_session.peek().epoch.wrapping_add(1);
    invalidate_graph_derived_state(ctx);
    if connection_changed {
        panels::document::reset_for_server_change();
        ctx.tasks.set(Vec::new());
        ctx.logs.set(Vec::new());
    }
    panels::layout::set_expected_graph_revision(None);
    ctx.graph.set(None);
    ctx.load_error.set(None);
    ctx.graph_session.set(GraphSession {
        epoch,
        graph_revision: None,
        origin: GraphOrigin::Server { endpoint },
    });
    epoch
}

fn commit_server_graph(mut ctx: Ctx, epoch: u64, graph: GraphData) -> bool {
    if ctx.graph_session.peek().epoch != epoch {
        return false;
    }
    let revision = graph.graph_revision.filter(|r| *r != 0);
    ctx.graph_session.write().graph_revision = revision;
    panels::layout::set_expected_graph_revision(revision);
    panels::style::reset_for_graph_session(revision);
    ctx.graph.set(Some(graph));
    true
}

/// Replace the canvas with a graph which graph-api does not host. Server-only
/// panels use the session origin to avoid silently querying the old vault.
pub(crate) fn replace_with_client_graph(
    mut ctx: Ctx,
    graph: GraphData,
    evaluator: impl Into<String>,
) {
    let epoch = ctx.graph_session.peek().epoch.wrapping_add(1);
    invalidate_graph_derived_state(ctx);
    panels::layout::set_expected_graph_revision(None);
    ctx.graph_session.set(GraphSession {
        epoch,
        graph_revision: None,
        origin: GraphOrigin::ClientGenerated {
            evaluator: evaluator.into(),
        },
    });
    let scene = graph.scene.clone();
    ctx.graph.set(Some(graph));
    render::mount_canvas(scene);
}

async fn reload_graph(mut ctx: Ctx) {
    let epoch = begin_server_graph_load(ctx);
    loop {
        match graph_canvas::load().await {
            Ok(g) => {
                if commit_server_graph(ctx, epoch, g) {
                    ctx.load_error.set(None);
                }
                break;
            }
            Err(e) if ctx.graph_session.peek().epoch == epoch => {
                ctx.load_error.set(Some(e));
                gloo_timers::future::TimeoutFuture::new(1500).await;
            }
            Err(_) => break,
        }
    }
}

// --- app ---------------------------------------------------------------------

/// Open-panel request bridge: modules that can't reach the workspace hook
/// (the command palette's jump-to-section actions) park a `Panel` here; the
/// drain effect in [`App`] restores + raises it. The egui analog is
/// `AppAction::JumpToSection` mutating `state.sections`.
pub(crate) static OPEN_PANEL: GlobalSignal<Option<Panel>> = Signal::global(|| None);

#[component]
fn App() -> Element {
    // Arm the AppState system (snapshot ticker, `#s=`/`?config=` boot
    // handling, style/camera loops) before any panel renders — a saved
    // layout with every panel minimized must still process boot presets.
    appstate::ensure_init();
    // _v6: the FilterStrip ("Filters") panel merged into the Filter panel, so
    // its enum variant is gone — a saved v5 layout referencing it would fail to
    // deserialize; re-seed instead. (v5: re-seed so a minimized Graph panel
    // can't leave the wgpu canvas unmounted → blank graph, plus pre-resize-fix
    // geometry; v4: tiling spans tile_w/tile_h; v3: Search merged into Nodes;
    // v2: 2x graph view + docked tray panels.)
    let ws = panel_kit::use_workspace("jc_layout_v6", default_layout);

    // Drain palette jump-to-section requests into the workspace, logging
    // the same `("section", "<title>: open")` event the egui app pushed.
    use_effect(move || {
        let req = *OPEN_PANEL.read();
        if let Some(kind) = req {
            ws.restore(kind);
            appstate::note_mutation(
                "section",
                &format!("{}: open", panel_kit::PanelKind::title(kind)),
            );
            *OPEN_PANEL.write() = None;
        }
    });

    let ctx = Ctx {
        graph: use_signal(|| None),
        graph_session: use_signal(|| GraphSession::loading_server(api::server_url())),
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
        server: use_signal(api::server_url),
        tasks: use_signal(Vec::new),
        logs: use_signal(Vec::new),
    };

    // Resilient initial load: retry until the backend answers, so a server
    // that's still indexing (or starting up) self-heals instead of leaving
    // the canvas permanently empty.
    {
        use_future(move || async move {
            reload_graph(ctx).await;
        });
    }

    // Selection -> fetch full node meta + seed the document editor.
    {
        let selected = ctx.selected;
        let mut meta = ctx.meta;
        let mut meta_busy = ctx.meta_busy;
        let mut draft = ctx.draft;
        let mut save_msg = ctx.save_msg;
        let graph_session = ctx.graph_session;
        use_effect(move || {
            let sel = selected.read().clone();
            let session = graph_session.read().clone();
            if !session.is_server_backed() {
                meta.set(None);
                meta_busy.set(false);
                draft.set(String::new());
                save_msg.set(
                    "Client-only graph: node metadata and document editing are unavailable.".into(),
                );
                return;
            }
            if let Some(id) = sel {
                let epoch = session.epoch;
                meta_busy.set(true);
                save_msg.set(String::new());
                spawn(async move {
                    let fetched = api::revisioned_node_meta(&id).await;
                    // A graph replacement or a newer selection invalidates
                    // this response even when the node id happens to match.
                    if graph_session.peek().epoch != epoch
                        || selected.peek().as_deref() != Some(id.as_str())
                    {
                        return;
                    }
                    match fetched {
                        Ok(response)
                            if response.revision == 0
                                || session.graph_revision == Some(response.revision) =>
                        {
                            let m = response.value;
                            draft.set(m.body.clone());
                            meta.set(Some(m));
                        }
                        Ok(response) => {
                            meta.set(None);
                            save_msg.set(format!(
                                "graph changed while loading node metadata (r{} → r{})",
                                session.graph_revision.unwrap_or(0),
                                response.revision
                            ));
                        }
                        Err(e) => {
                            meta.set(None);
                            save_msg.set(format!("load failed: {e}"));
                        }
                    }
                    meta_busy.set(false);
                });
            } else {
                meta.set(None);
                meta_busy.set(false);
                draft.set(String::new());
                save_msg.set(String::new());
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
                    let session = ctx.graph_session.peek().clone();
                    let server_replaced_graph =
                        session.replaced_by_server_revision(resp.graph_revision);
                    if server_replaced_graph {
                        // `/progress` is the global, always-mounted poller, so
                        // it also acts as the graph-session invalidation feed.
                        // Reload before any panel can continue applying dense
                        // metrics/field indices from the replacement snapshot
                        // to the old renderer buffers.
                        reload_graph(ctx).await;
                    }
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

    // Selection + search highlights → wgpu renderer. The renderer's own
    // rAF loop handles per-frame drawing and panel resizes; these effects
    // only push state when the signals actually change.
    {
        let graph = ctx.graph;
        let selected = ctx.selected;
        let results = ctx.results;
        use_effect(move || {
            let g = graph.read();
            let sel = selected.read();
            if let Some(g) = g.as_ref() {
                let sel_idx = sel.as_ref().and_then(|id| g.id_to_idx.get(id)).copied();
                render::set_selected_node(sel_idx);
            }
        });
        use_effect(move || {
            let g = graph.read();
            let res = results.read();
            if let Some(g) = g.as_ref() {
                let hl: HashSet<u32> = res
                    .iter()
                    .filter_map(|id| g.id_to_idx.get(id))
                    .copied()
                    .collect();
                render::set_search_highlights(Some(hl));
            }
        });
    }

    let g_now = ctx.graph.read().clone();
    let session_label = ctx.graph_session.read().short_label();
    // Mirrors the egui tray's right-side running indicator: a live count of
    // in-progress server tasks, grey idle dot otherwise.
    let n_running = ctx.tasks.read().iter().filter(|t| t.state == 0).count();
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
            // WASDQE camera pan + Shift boost + F fit, fed to the wgpu
            // renderer's held-key state. Single-key shortcuts must not
            // fire while the user is typing in an input/textarea.
            onkeydown: move |e: KeyboardEvent| {
                // Palette chord first: it must open even while an input has
                // focus, and a consumed event never reaches the camera.
                if palette::handle_key(&e, ctx) {
                    return;
                }
                if panel_kit::is_editing() {
                    render::clear_keys();
                    return;
                }
                match e.key() {
                    Key::Shift => render::key_event("Shift", true),
                    Key::Character(c) => {
                        if c.eq_ignore_ascii_case("f") {
                            render::fit_camera();
                        } else {
                            render::key_event(&c, true);
                        }
                    }
                    _ => {}
                }
            },
            onkeyup: move |e: KeyboardEvent| {
                match e.key() {
                    Key::Shift => render::key_event("Shift", false),
                    Key::Character(c) => render::key_event(&c, false),
                    _ => {}
                }
            },

            header { class: "topbar",
                h1 { "JUMP CANNON" }
                // Node/edge/community counts live in the Settings panel (the
                // superset — it also has components); the topbar just shows the
                // workspace mode + connection state to avoid a third copy.
                span { class: "hint",
                    if g_now.is_some() {
                        "{mode_label} · {session_label}"
                    } else {
                        "{mode_label} · connecting…"
                    }
                }
                if n_running > 0 {
                    span { class: "activity", Spinner {} " running {n_running}" }
                } else if g_now.is_none() {
                    span { class: "activity", "○ waiting for graph-api" }
                } else {
                    span { class: "activity idle", "●" }
                }
            }

            {ws.render_with_header(
                move |kind, maximized| panel_body(kind, maximized, ctx),
                move |kind, _maximized| panel_header_actions(kind, ctx),
            )}

            {ws.dock()}

            // Phase-4 overlays: both render empty until active.
            {palette::overlay(ctx)}
            {anchored::overlay(ctx)}
        }
    }
}

// --- panel bodies ----------------------------------------------------------------

fn panel_body(kind: Panel, _maximized: bool, ctx: Ctx) -> Element {
    match kind {
        Panel::Graph => {
            if ctx.graph.read().is_some() {
                rsx! { graph_canvas::GraphCanvas { graph: ctx.graph, selected: ctx.selected } }
            } else if let Some(e) = ctx.load_error.read().clone() {
                rsx! { div { class: "skeleton", Spinner { label: "retrying: {e}" } } }
            } else {
                rsx! { div { class: "skeleton", Spinner { label: "loading graph…" } } }
            }
        }
        Panel::Nodes => panels::nodes::panel(ctx),
        Panel::Inspector => panels::inspector::panel(ctx),
        Panel::Document => panels::document::panel(ctx),
        Panel::Progress => progress_panel(ctx),
        Panel::Settings => settings_panel(ctx),
        Panel::Layout => panels::layout::panel(ctx),
        Panel::Style => panels::style::panel(ctx),
        Panel::Camera => panels::camera::panel(ctx),
        Panel::Filter => panels::filter::panel(ctx),
        Panel::Metrics => panels::metrics::panel(ctx),
        Panel::Instances => panels::instances::panel(ctx),
        Panel::Generate => panels::generate::panel(ctx),
        Panel::Timeline => panels::timeline::panel(ctx),
        Panel::Debug => panels::debug::panel(ctx),
        Panel::Help => rsx! {
            div { class: "help",
                p { "canvas: drag rotate · wheel zoom · WASD pan · QE fwd/back · Shift boost · F fit · click select" }
                p { "nodes: type → fuzzy files + content matches + filter chips" }
                hr {}
                p { "🔵 tiling ⇄ floating" }
                p { "🟡 minimize → dock" }
                p { "🩷 maximize ⇄ restore" }
            }
        },
    }
}

fn panel_header_actions(kind: Panel, ctx: Ctx) -> Element {
    if kind != Panel::Graph {
        return rsx! {};
    }
    let run_control = panels::layout::header_run_control(
        ctx.graph_session.peek().is_server_backed(),
    );
    let remote = matches!(
        run_control,
        Some(panels::layout::HeaderRunControl::RemoteAttached)
    );
    let following = *render::SIM_RUNNING.read();
    let disabled = ctx.graph.read().is_none();
    rsx! {
        if run_control.is_some() {
            panel_kit::PanelHeaderButton {
                label: if following { "Ⅱ" } else { "▶" },
                title: if remote {
                    if following {
                        "Freeze worker position updates in this view; the remote solver keeps running"
                    } else {
                        "Follow live worker position updates again"
                    }
                } else if following {
                    "Pause the local layout solver"
                } else {
                    "Resume the local layout solver"
                },
                active: !following,
                disabled,
                on_press: move |_| render::set_sim_running(!following),
            }
        }
        panel_kit::PanelHeaderButton {
            label: "Fit",
            title: "Fit the camera to the graph (F)",
            disabled,
            on_press: move |_| render::fit_camera(),
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
    let Ctx {
        mut server,
        graph,
        graph_session,
        ..
    } = ctx;
    let g = graph.read().clone();
    let session = graph_session.read().clone();
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
            div { class: "note", "active: {session.short_label()}" }
            div { class: "note",
                if session.is_server_backed() {
                    "Graph metadata/search/documents are served by graph-api. Compute-worker \
                     layouts are accepted only for this graph revision."
                } else {
                    "This generated graph is browser-owned. Metadata/search/documents and \
                     compute-worker layouts are disabled until it is hosted by graph-api."
                }
            }
        }
    }
}
