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
mod selection_card;
mod client_log;
mod graph_canvas;
mod palette;
mod panels;
mod proto;
mod render;
mod worker;

mod github;
use std::collections::HashSet;
use std::sync::Arc;

use dioxus::events::{Key, KeyboardEvent};
use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use panel_kit::{LayoutBuilder, PanelWin, Spinner};
use serde::{Deserialize, Serialize};
use session_manager::{EmbeddedSessionManager, HttpSessionManager, UserIdentity, WorldHost};

use graph_canvas::GraphData;

/// Top-level app surface. `User` is the classic single-graph workspace;
/// `Sessions` is the world/session workspace (versioned worlds, branches,
/// GPU sessions) backed by a `WorldHost`. One view is mounted at a time —
/// the single wgpu RenderHost and the global graph-derived state stay valid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AppView {
    User,
    Sessions,
}

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
    // Ship panics to `POST /log/client` too — chains onto the hook above, so
    // the console output and red banner are unchanged.
    client_log::install_panic_hook();
    // Boot marker for crates/test-browser: it greps the console for this
    // exact line to know the wasm app booted. console.log directly (NOT
    // tracing) — tracing is filtered to WARN above.
    web_sys::console::log_1(&"[jump-cannon-ui] boot".into());
    // Boot marker for the browser regression: the suite detects the app
    // runtime by this value (and detects full-page reloads — e.g. the
    // AppState apply path — by it changing).
    if let Some(w) = web_sys::window() {
        let _ = js_sys::Reflect::set(
            w.as_ref(),
            &"__jc_boot".into(),
            &wasm_bindgen::JsValue::from_f64(js_sys::Date::now()),
        );
    }
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
    Filter,
    Metrics,
    Instances,
    Generate,
    Timeline,
    Debug,
    // Sessions view (versioned shared worlds — crates/session-manager):
    Worlds,
    History,
    Branches,
    Merge,
    GitHub,
    GpuSessions,
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
            Panel::Filter => "Filter",
            Panel::Metrics => "Metrics",
            Panel::Instances => "Instances",
            Panel::Generate => "Generate",
            Panel::Timeline => "Timeline",
            Panel::Debug => "Debug",
            Panel::Worlds => "Worlds",
            Panel::History => "History",
            Panel::GitHub => "GitHub",
            Panel::Branches => "Branches",
            Panel::Merge => "Merge",
            Panel::GpuSessions => "GPU Sessions",
        }
    }
}

const WORKSPACE_LAYOUT_KEY: &str = "jc_layout_v9";
const SESSIONS_LAYOUT_KEY: &str = "jc_sessions_layout_v1";
const LEGACY_WORKSPACE_LAYOUT_KEYS: &[&str] = &["jc_layout_v8", "jc_layout_v7", "jc_layout_v6"];

/// Panel identity used by the three workspace layouts immediately preceding
/// the Settings consolidation. Keeping this separate from [`Panel`] lets us
/// decode and collapse old Layout/Style/Camera windows without retaining them
/// as live Panel Kit kinds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
enum LegacyPanel {
    Graph,
    Nodes,
    Inspector,
    Document,
    Progress,
    Settings,
    Help,
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

#[derive(Deserialize)]
struct LegacyWorkspaceLayout {
    panels: Vec<PanelWin<LegacyPanel>>,
    tiling: bool,
}

#[derive(Serialize, Deserialize)]
struct WorkspaceLayout {
    panels: Vec<PanelWin<Panel>>,
    tiling: bool,
}

fn current_panel(kind: LegacyPanel) -> Option<Panel> {
    Some(match kind {
        LegacyPanel::Graph => Panel::Graph,
        LegacyPanel::Nodes => Panel::Nodes,
        LegacyPanel::Inspector => Panel::Inspector,
        LegacyPanel::Document => Panel::Document,
        LegacyPanel::Progress => Panel::Progress,
        LegacyPanel::Settings => Panel::Settings,
        LegacyPanel::Help => Panel::Help,
        LegacyPanel::Filter => Panel::Filter,
        LegacyPanel::Metrics => Panel::Metrics,
        LegacyPanel::Instances => Panel::Instances,
        LegacyPanel::Generate => Panel::Generate,
        LegacyPanel::Timeline => Panel::Timeline,
        LegacyPanel::Debug => Panel::Debug,
        LegacyPanel::Layout | LegacyPanel::Style | LegacyPanel::Camera => return None,
    })
}

fn legacy_settings_tab(kind: LegacyPanel) -> Option<panels::settings::SettingsTab> {
    use panels::settings::SettingsTab;
    match kind {
        LegacyPanel::Settings => Some(SettingsTab::Connection),
        LegacyPanel::Layout => Some(SettingsTab::Layout),
        LegacyPanel::Style => Some(SettingsTab::Appearance),
        LegacyPanel::Camera => Some(SettingsTab::Camera),
        _ => None,
    }
}

fn convert_panel<K>(panel: PanelWin<K>, kind: Panel) -> PanelWin<Panel> {
    // Panel Kit's historical 1x2 default is too narrow and short for the
    // editor-style Nodes workbench. Upgrade that exact legacy span while
    // preserving every explicitly different user size.
    let (tile_w, tile_h) = if kind == Panel::Nodes && panel.tile_w == 1 && panel.tile_h == 2 {
        (2, 4)
    } else {
        (panel.tile_w, panel.tile_h)
    };
    PanelWin {
        kind,
        x: panel.x,
        y: panel.y,
        w: panel.w,
        h: panel.h,
        state: panel.state,
        z: panel.z,
        tile_w,
        tile_h,
    }
}

/// Collapse an old layout into one Settings window. If a legacy configuration
/// panel was visible, the frontmost configuration panel's geometry becomes the
/// Settings geometry and its destination becomes the active tab; minimized
/// legacy dock chips are simply removed.
fn convert_legacy_layout(
    legacy: LegacyWorkspaceLayout,
) -> (WorkspaceLayout, Option<panels::settings::SettingsTab>) {
    let visible_config = legacy
        .panels
        .iter()
        .filter_map(|panel| {
            legacy_settings_tab(panel.kind)
                .filter(|_| panel.state != panel_kit::WinState::Minimized)
                .map(|tab| (panel.z, *panel, tab))
        })
        .max_by_key(|(z, _, _)| *z);

    let mut panels: Vec<PanelWin<Panel>> = legacy
        .panels
        .into_iter()
        .filter_map(|panel| current_panel(panel.kind).map(|kind| convert_panel(panel, kind)))
        .collect();

    if let Some((_, source, _)) = visible_config {
        let settings = convert_panel(source, Panel::Settings);
        if let Some(existing) = panels
            .iter_mut()
            .find(|panel| panel.kind == Panel::Settings)
        {
            *existing = settings;
        } else {
            panels.push(settings);
        }
    }

    (
        WorkspaceLayout {
            panels,
            tiling: legacy.tiling,
        },
        visible_config.map(|(_, _, tab)| tab),
    )
}

fn migrate_workspace_layout() {
    if LocalStorage::get::<WorkspaceLayout>(WORKSPACE_LAYOUT_KEY).is_ok() {
        return;
    }
    for key in LEGACY_WORKSPACE_LAYOUT_KEYS {
        let Ok(legacy) = LocalStorage::get::<LegacyWorkspaceLayout>(key) else {
            continue;
        };
        let (layout, tab) = convert_legacy_layout(legacy);
        if LocalStorage::set(WORKSPACE_LAYOUT_KEY, layout).is_ok() {
            if let Some(tab) = tab {
                panels::settings::select_tab(tab);
            }
        }
        break;
    }
}

/// Default layout: graph and the editor-style Nodes workbench share the main
/// row; the detachable Inspector and Document views start in the dock.
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
        min(b, Panel::Filter, 800.0, 120.0, 340.0, 420.0),
        min(b, Panel::Metrics, 840.0, 160.0, 320.0, 380.0),
        min(b, Panel::Instances, 860.0, 180.0, 360.0, 420.0),
        min(b, Panel::Generate, 880.0, 200.0, 360.0, 440.0),
        min(b, Panel::Timeline, 900.0, 220.0, 380.0, 320.0),
        min(b, Panel::GitHub, 890.0, 210.0, 360.0, 440.0),
        min(b, Panel::Debug, 920.0, 240.0, 320.0, 360.0),
        min(b, Panel::Inspector, 940.0, 260.0, 330.0, 300.0),
        min(b, Panel::Document, 960.0, 280.0, 430.0, 460.0),
        min(b, Panel::Help, 980.0, 300.0, 330.0, 180.0),
    ];
    // Floating mode: the 1280px browser-regression viewport can show both
    // primary surfaces without overlap. The Nodes panel is wide enough for
    // its navigator + focused-content split.
    // Tiling mode: the graph starts full-width × 3 rows (with_tile replaces
    // the old .panel-graph CSS override; the grip resizes it in snapped
    // steps now).
    v.extend([
        b.at(Panel::Graph, 12.0, 44.0, 640.0, 620.0).with_tile(4, 3),
        b.at(Panel::Nodes, 660.0, 44.0, 608.0, 620.0)
            .with_tile(2, 4),
        b.at(Panel::Progress, 12.0, 672.0, 640.0, 200.0),
        b.at(Panel::Settings, 660.0, 672.0, 608.0, 420.0)
            .with_tile(2, 3),
    ]);
    v
}

/// Sessions view default layout: world management and history share the main
/// row; the graph surface of the open world sits below, with branch/merge/GPU
/// consoles in the dock until needed.
fn sessions_default_layout() -> Vec<PanelWin<Panel>> {
    let mut b = LayoutBuilder::new();
    fn min(b: &mut LayoutBuilder, kind: Panel, x: f64, y: f64, w: f64, h: f64) -> PanelWin<Panel> {
        let mut p = b.at(kind, x, y, w, h);
        p.state = panel_kit::WinState::Minimized;
        p
    }
    let b = &mut b;
    let mut v = vec![
        min(b, Panel::Branches, 840.0, 120.0, 400.0, 360.0),
        min(b, Panel::Merge, 860.0, 160.0, 420.0, 400.0),
        min(b, Panel::GpuSessions, 880.0, 200.0, 380.0, 340.0),
        min(b, Panel::Nodes, 900.0, 240.0, 608.0, 620.0),
        min(b, Panel::Progress, 920.0, 280.0, 640.0, 200.0),
        // Settings carries the session-manager URL + x-user identity the
        // Worlds/GPU panels depend on — reachable from the dock here, not
        // only back in the User view.
        min(b, Panel::Settings, 930.0, 300.0, 608.0, 420.0),
        min(b, Panel::Help, 940.0, 320.0, 330.0, 180.0),
    ];
    v.extend([
        b.at(Panel::Worlds, 12.0, 44.0, 420.0, 420.0).with_tile(2, 2),
        b.at(Panel::History, 444.0, 44.0, 824.0, 420.0)
            .with_tile(4, 2),
        b.at(Panel::Graph, 12.0, 476.0, 1256.0, 460.0)
            .with_tile(6, 2),
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

/// Build the standalone (no session-manager URL) host. wasm32 uses the
/// localStorage-persistent backend so browser-only worlds survive reloads,
/// falling back to pure in-memory when persistence is unavailable; native
/// (tests, tooling) stays in-memory.
fn new_embedded_host() -> Result<EmbeddedSessionManager, session_manager::SessionError> {
    #[cfg(target_arch = "wasm32")]
    {
        EmbeddedSessionManager::open_persistent().or_else(|e| {
            tracing::warn!(
                "persistent embedded host unavailable ({e}); falling back to in-memory"
            );
            EmbeddedSessionManager::in_memory()
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        EmbeddedSessionManager::in_memory()
    }
}

/// Every app signal, bundled `Copy` so panel renderers and handlers can grab
/// what they need without prop-drilling through components.
#[derive(Clone, Copy, PartialEq)]
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
    // Sessions view: active surface, world host, and the world whose graph is
    // mounted. `sm_url`/`user` are the Settings edit buffers that rebuild the
    // host on change.
    pub(crate) view: Signal<AppView>,
    pub(crate) host: Signal<Arc<dyn WorldHost>>,
    /// The concrete embedded host whenever `host` is one (no session-manager
    /// URL configured). World export/import are inherent methods on
    /// `EmbeddedSessionManager` — the `WorldHost` trait stays stable — so
    /// the Sessions panels reach them through this handle.
    pub(crate) embedded: Signal<Option<Arc<EmbeddedSessionManager>>>,
    pub(crate) active_world: Signal<Option<String>>,
    pub(crate) sm_url: Signal<String>,
    pub(crate) user: Signal<String>,
}

/// Point the graph surface at a world's serving routes and reload. The
/// session manager serves each open world's full graph-api surface at
/// `/worlds/:name/...`; with no manager configured (embedded host) the
/// world's `main` head is materialized in-browser instead (see
/// [`rematerialize_embedded`]).
pub(crate) fn open_world_in_view(mut ctx: Ctx, world: String) {
    ctx.active_world.set(Some(world.clone()));
    // Persist the selection so page reloads and Sessions-view returns
    // re-open the same world (see [`restore_persisted_world`]).
    let _ = LocalStorage::set(WORLD_KEY, &world);
    match api::session_manager_url() {
        Some(sm) => {
            *api::WORLD_BASE.write() = Some(format!("{sm}/worlds/{world}"));
            spawn(reload_graph(ctx));
        }
        None => {
            *api::WORLD_BASE.write() = None;
            spawn_rematerialize_embedded(ctx);
        }
    }
}

/// Detach the graph surface from any world (world closed, host changed).
pub(crate) fn clear_world_base(mut ctx: Ctx) {
    ctx.active_world.set(None);
    let _ = LocalStorage::delete(WORLD_KEY);
    *api::WORLD_BASE.write() = None;
    *EMBEDDED_HEAD.write() = None;
}

/// The (world, head) pair currently mounted from the embedded host, so
/// repeat rematerializations that would produce the identical graph skip
/// the full canvas reload.
pub(crate) static EMBEDDED_HEAD: GlobalSignal<Option<(String, String)>> = Signal::global(|| None);

/// Materialize the open embedded world's `main` head into the canvas as a
/// browser-owned graph. No-op when a session manager is configured, no
/// world is open, or the mounted head is already current.
pub(crate) async fn rematerialize_embedded(mut ctx: Ctx) {
    if api::session_manager_url().is_some() {
        return;
    }
    let Some(world) = ctx.active_world.read().clone() else {
        return;
    };
    let Ok(wid) = session_manager::WorldId::parse(&world) else {
        return;
    };
    let host = ctx.host.read().clone();
    let result = async {
        let vcs = match host.vcs(&wid).await {
            Ok(vcs) => vcs,
            // A world replayed from localStorage at boot comes back CLOSED
            // (`open_persistent`), so the first materialize after a reload
            // hits require_open. Re-open a world the host still knows; a
            // genuinely unknown world keeps the original error rather than
            // being silently re-created empty.
            Err(session_manager::SessionError::WorldNotFound { .. }) => {
                let known = host
                    .worlds()
                    .await
                    .map_err(|e| e.to_string())?
                    .iter()
                    .any(|w| w.id == wid);
                if !known {
                    return Err(format!("world not found: {}", wid.0));
                }
                host.open_world(session_manager::WorldSpec {
                    name: wid.0.clone(),
                    description: None,
                })
                .await
                .map_err(|e| e.to_string())?;
                host.vcs(&wid).await.map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        let head = vcs
            .head("main")
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "world has no main branch".to_string())?;
        let snapshot = vcs.materialize(&head).await.map_err(|e| e.to_string())?;
        Ok((head, snapshot))
    }
    .await;
    match result {
        Ok((head, snapshot)) => {
            if EMBEDDED_HEAD.peek().as_ref() == Some(&(world.clone(), head.0.clone())) {
                return;
            }
            let graph = graph_canvas::graph_data_from_snapshot(&snapshot);
            ctx.load_error.set(None);
            replace_with_client_graph(ctx, graph, "embedded world");
            // Set AFTER the replace, which clears the record for any other
            // client graph (the Generate panel's local path).
            *EMBEDDED_HEAD.write() = Some((world, head.0));
        }
        Err(e) => {
            tracing::warn!("embedded world materialize failed: {e}");
            ctx.load_error.set(Some(e));
        }
    }
}

/// Fire-and-forget [`rematerialize_embedded`] for VCS-mutation handlers:
/// only spawns when the embedded host is active and a world is open.
pub(crate) fn spawn_rematerialize_embedded(ctx: Ctx) {
    if api::session_manager_url().is_some() || ctx.active_world.peek().is_none() {
        return;
    }
    spawn(rematerialize_embedded(ctx));
}

/// Switch the top-level view in place. The two views own separate workspaces,
/// servers, and world hosts; exactly one workspace is mounted at a time, and
/// `UserWorkspaceView`/`SessionsWorkspaceView` are distinct component types so
/// the switch unmounts one hook scope and initializes the other cleanly (the
/// rules-of-hooks fix — see the wrapper comment below). The graph surface
/// re-bases on switch: standalone graph-api in User, the open world's routes
/// (or embedded rematerialization) in Sessions. The choice persists
/// (`jc_view`) so a later full-page reload boots into the same view, and the
/// open world persists separately (`jc_world`) so leaving Sessions for User
/// and returning re-mounts it.
pub(crate) fn switch_view(mut ctx: Ctx, view: AppView) {
    if *ctx.view.peek() == view {
        return;
    }
    let _ = LocalStorage::set(VIEW_KEY, view_slug(view));
    match view {
        AppView::User => {
            // Detach the world routes without dropping the selection: the
            // world stays open (and persisted) and re-mounts on return.
            *api::WORLD_BASE.write() = None;
            *EMBEDDED_HEAD.write() = None;
            ctx.view.set(view);
            spawn(reload_graph(ctx));
        }
        AppView::Sessions => {
            ctx.view.set(view);
            if let Some(world) = ctx.active_world.peek().clone() {
                open_world_in_view(ctx, world);
            }
        }
    }
}

const VIEW_KEY: &str = "jc_view";
/// localStorage key for the world open in the Sessions view. Written by
/// [`open_world_in_view`], cleared by [`clear_world_base`]; read at boot by
/// [`restore_persisted_world`] and on returning to the Sessions view, so
/// neither a page reload nor a view switch strands the selection. Applies
/// to both embedded and HTTP session-manager hosts.
const WORLD_KEY: &str = "jc_world";

fn view_slug(view: AppView) -> &'static str {
    match view {
        AppView::User => "user",
        AppView::Sessions => "sessions",
    }
}

/// The view boot restores: the last one chosen via the topbar switcher.
pub(crate) fn persisted_view() -> AppView {
    match LocalStorage::get::<String>(VIEW_KEY)
        .unwrap_or_default()
        .as_str()
    {
        "sessions" => AppView::Sessions,
        _ => AppView::User,
    }
}

/// Re-open the world that was open when the page last unloaded (before
/// in-place view switching landed, every view switch reloaded the page and
/// stranded the selection; full reloads still happen via the AppState apply
/// path). Only the Sessions view mounts world graphs, and only after the
/// boot host build has run — the caller is the first run of the
/// host-rebuild effect in [`App`], so `ctx.host` already points at the host
/// the world was opened on.
fn restore_persisted_world(ctx: Ctx) {
    if *ctx.view.peek() != AppView::Sessions {
        return;
    }
    let world = LocalStorage::get::<String>(WORLD_KEY).unwrap_or_default();
    if world.is_empty() {
        return;
    }
    open_world_in_view(ctx, world);
}

/// Whether `view`'s workspace layout holds `kind`. The palette filters its
/// jump-to-section commands through this so it never offers a restore that
/// would silently no-op (`Workspace::restore` on a panel the active view's
/// layout doesn't contain).
pub(crate) fn panel_in_view(kind: Panel, view: AppView) -> bool {
    let layout = match view {
        AppView::User => default_layout(),
        AppView::Sessions => sessions_default_layout(),
    };
    layout.iter().any(|p| p.kind == kind)
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
    panels::style::reset_for_graph_session(false);
    panels::timeline::reset_for_graph_session();
    palette::reset_for_graph_session();
    render::set_selected_node(None);
    render::set_search_highlights(None);
}

fn begin_server_graph_load(mut ctx: Ctx) -> u64 {
    let endpoint = api::server_url();
    // A server load supersedes any embedded-world mount record.
    *EMBEDDED_HEAD.write() = None;
    let initial_load = ctx.graph_session.peek().epoch == 0 && ctx.graph.peek().is_none();
    let connection_changed = match &ctx.graph_session.peek().origin {
        GraphOrigin::Server { endpoint: old } => old != &endpoint,
        GraphOrigin::ClientGenerated { .. } => false,
    };
    let epoch = ctx.graph_session.peek().epoch.wrapping_add(1);
    // All derived stores already start empty. Writing every GlobalSignal from
    // the initial use_future can race Dioxus's first render, which still holds
    // read guards for those stores and panics with AlreadyBorrowed. Reloads and
    // graph replacements still need the full invalidation.
    if !initial_load {
        invalidate_graph_derived_state(ctx);
    }
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
    panels::style::reset_for_graph_session(true);
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
    // Any client-graph replacement supersedes the embedded-world mount
    // record; `rematerialize_embedded` re-sets it after calling this.
    *EMBEDDED_HEAD.write() = None;
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
    ctx.graph.set(Some(graph));
}

pub(crate) async fn reload_graph(mut ctx: Ctx) {
    let epoch = begin_server_graph_load(ctx);
    match graph_canvas::load().await {
        Ok(g) => {
            commit_server_graph(ctx, epoch, g);
        }
        Err(e) if ctx.graph_session.peek().epoch == epoch => ctx.load_error.set(Some(e)),
        Err(_) => {}
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
    // v7 intentionally adopts the two-pane Nodes workbench default once;
    // subsequent panel movement/resizing persists as before.
    // v9 consolidates Layout, Style, and Camera into one tabbed Settings
    // surface. Migrate v6-v8 layout geometry and let the
    // independent jc_* control state continue unchanged.
    migrate_workspace_layout();
    // Both views' workspaces are created unconditionally (hook rules); only
    // the active view's workspace is rendered, so the single wgpu render host
    // and localStorage layout persistence (`jc_layout_v9` vs
    // `jc_sessions_layout_v1`) stay consistent per view.
    let ws_user = panel_kit::use_workspace(WORKSPACE_LAYOUT_KEY, default_layout);
    let ws_sessions = panel_kit::use_workspace(SESSIONS_LAYOUT_KEY, sessions_default_layout);
    // Hoisted above the palette-drain effect so both it and Ctx share one
    // view signal. Boot restores the view last chosen via the switcher.
    let view = use_signal(persisted_view);

    // Drain palette jump-to-section requests into the workspace, logging
    // the same `("section", "<title>: open")` event the egui app pushed.
    use_effect(move || {
        let req = *OPEN_PANEL.read();
        if let Some(kind) = req {
            let ws = match *view.read() {
                AppView::User => ws_user,
                AppView::Sessions => ws_sessions,
            };
            ws.restore(kind);
            appstate::note_mutation(
                "section",
                &format!("{}: open", panel_kit::PanelKind::title(kind)),
            );
            *OPEN_PANEL.write() = None;
        }
    });

    let initial_embedded = Arc::new(new_embedded_host().expect("embedded host cannot fail"));
    let initial_host = initial_embedded.clone();
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
        view,
        host: use_signal(move || initial_host.clone() as Arc<dyn WorldHost>),
        embedded: use_signal(move || Some(initial_embedded.clone())),
        active_world: use_signal(|| None),
        sm_url: use_signal(|| api::session_manager_url().unwrap_or_default()),
        user: use_signal(api::user_name),
    };

    // Rebuild the world host when the session-manager URL or user identity
    // changes: remote (`HttpSessionManager`) when a manager is configured,
    // embedded single-user otherwise. The first run is the boot build — it
    // restores the world persisted across a view-switch reload instead of
    // dropping the selection; on later (real) host changes any world
    // selection is dropped, since the new host does not know it.
    {
        let mut host = ctx.host;
        let mut embedded = ctx.embedded;
        let mut booted = use_signal(|| false);
        use_effect(move || {
            let sm = api::session_manager_url();
            let name = ctx.user.read().clone();
            let first_run = !*booted.peek();
            booted.set(true);
            spawn(async move {
                let identity = UserIdentity {
                    name,
                    groups: Vec::new(),
                };
                let next: Arc<dyn WorldHost> = match sm {
                    Some(url) => match HttpSessionManager::connect(url, identity).await {
                        Ok(h) => {
                            embedded.set(None);
                            Arc::new(h)
                        }
                        Err(e) => {
                            tracing::warn!("session manager connect failed: {e}");
                            return;
                        }
                    },
                    None => match new_embedded_host() {
                        Ok(h) => {
                            let h = Arc::new(h);
                            embedded.set(Some(h.clone()));
                            h
                        }
                        Err(e) => {
                            tracing::warn!("embedded host init failed: {e}");
                            return;
                        }
                    },
                };
                host.set(next);
                if first_run {
                    restore_persisted_world(ctx);
                } else {
                    clear_world_base(ctx);
                }
            });
        });
    }

    // Query evaluation is app-owned rather than panel-owned: persisted
    // expressions and mutations from badges/palette actions must keep applying
    // even when the Filter panel is minimized or never mounted.
    panels::filter::use_query_evaluator(ctx);

    // GitHub import mode: when the URL carries a `?gh=` spec or the page is
    // served from a github.io host (no graph-api server exists there), the
    // boot path is an in-browser GitHub import — the resilient server-load
    // retry below is skipped instead of 404ing against the Pages host
    // forever. `use_hook` computes the decision ONCE per mount: reading it in
    // the component body would re-run the import on every App re-render
    // (each graph promotion re-renders App — an import loop).
    let github_boot = use_hook(crate::github::boot_spec);
    let skip_server_load = github_boot.is_some();

    // Resilient initial load: retry until the backend answers, so a server
    // that's still indexing (or starting up) self-heals instead of leaving
    // the canvas permanently empty.
    {
        let mut load_error = ctx.load_error;
        use_future(move || async move {
            if skip_server_load {
                return;
            }
            let epoch = begin_server_graph_load(ctx);
            loop {
                match graph_canvas::load().await {
                    Ok(g) => {
                        if commit_server_graph(ctx, epoch, g) {
                            load_error.set(None);
                        }
                        break;
                    }
                    Err(e) => {
                        if ctx.graph_session.peek().epoch != epoch {
                            break;
                        }
                        load_error.set(Some(e));
                        gloo_timers::future::TimeoutFuture::new(1500).await;
                    }
                }
            }
        });
    }

    // Boot import in GitHub mode: drive the panel's shared import routine so
    // progress and errors surface in the GitHub panel, and open that panel.
    // The future body reads no signals, so it runs exactly once.
    use_future(move || {
        let boot = github_boot.clone();
        async move {
            if let Some(spec) = boot {
                *OPEN_PANEL.write() = Some(Panel::GitHub);
                panels::github::spawn_import(spec, ctx);
            }
        }
    });
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
                    let fetched = api::node_meta(&id).await;
                    // A graph replacement or a newer selection invalidates
                    // this response even when the node id happens to match.
                    if graph_session.peek().epoch != epoch
                        || selected.peek().as_deref() != Some(id.as_str())
                    {
                        return;
                    }
                    match fetched {
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
            } else {
                meta.set(None);
                meta_busy.set(false);
                draft.set(String::new());
                save_msg.set(String::new());
            }
        });
    }

    // Poll the server progress log (import reloads, search reindex, …).
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
    let view_now = *ctx.view.read();
    let ws = match view_now {
        AppView::User => ws_user,
        AppView::Sessions => ws_sessions,
    };
    let mode_label = match ws.effective_mode() {
        panel_kit::Mode::Tiling => "tiling",
        panel_kit::Mode::Floating => "floating",
    };
    let world_label = ctx.active_world.read().clone();

    rsx! {
        // app.css is not inlined here: index.html loads it via a Trunk
        // <link rel="css"> so the static boot shell can paint pre-WASM.
        style { {panel_kit::CSS} }
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
                // View switcher: each view owns its workspace + dock (its own
                // localStorage layout); the graph surface re-bases on switch.
                nav { class: "view-switch",
                    button {
                        class: if view_now == AppView::User { "view-btn active" } else { "view-btn" },
                        r#type: "button",
                        title: "User workspace: explore the served graph",
                        aria_pressed: if view_now == AppView::User { "true" } else { "false" },
                        onclick: move |_| switch_view(ctx, AppView::User),
                        "User"
                    }
                    button {
                        class: if view_now == AppView::Sessions { "view-btn active" } else { "view-btn" },
                        r#type: "button",
                        title: "Sessions workspace: versioned shared worlds",
                        aria_pressed: if view_now == AppView::Sessions { "true" } else { "false" },
                        onclick: move |_| switch_view(ctx, AppView::Sessions),
                        "Sessions"
                    }
                }
                // Node/edge/community counts live in the Settings panel (the
                // superset — it also has components); the topbar just shows the
                // workspace mode + connection state to avoid a third copy.
                span { class: "hint",
                    if view_now == AppView::Sessions {
                        if let Some(w) = &world_label {
                            "sessions · world {w}"
                        } else {
                            "sessions · no world open"
                        }
                    } else if g_now.is_some() {
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

            {match view_now {
                AppView::User => rsx! { UserWorkspaceView { ws: ViewWs(ws_user), ctx } },
                AppView::Sessions => rsx! { SessionsWorkspaceView { ws: ViewWs(ws_sessions), ctx } },
            }}

            // Phase-4 overlays: both render empty until active.
            {palette::overlay(ctx)}
            {anchored::overlay(ctx)}
        }
    }
}

// --- per-view workspace wrappers ------------------------------------------------

// `Workspace::render_with_header` calls each visible panel's body fn INLINE,
// so panel hooks land in the caller's scope. Both views must therefore mount
// under DISTINCT component types: switching views unmounts one scope and
// initializes the other cleanly, instead of scrambling hook indices inside a
// single shared scope (the rules-of-hooks panic this fixes).

/// Prop wrapper: Dioxus 0.6 requires `PartialEq` props and `Workspace` has
/// none. Signal equality is identity-based, which is exactly what a prop
/// comparison needs.
#[derive(Clone, Copy)]
struct ViewWs(panel_kit::Workspace<Panel>);

impl PartialEq for ViewWs {
    fn eq(&self, other: &Self) -> bool {
        self.0.panels == other.0.panels
            && self.0.mode == other.0.mode
            && self.0.drag == other.0.drag
            && self.0.tile_drag == other.0.tile_drag
            && self.0.is_mobile == other.0.is_mobile
            && self.0.viewport == other.0.viewport
            && self.0.ws_scroll == other.0.ws_scroll
    }
}

#[component]
fn UserWorkspaceView(ws: ViewWs, ctx: Ctx) -> Element {
    let ws = ws.0;
    rsx! {
        {ws.render_with_header(
            move |kind, maximized| panel_body(kind, maximized, ctx),
            move |kind, _maximized| panel_header_actions(kind, ctx),
        )}
        {ws.dock()}
    }
}

#[component]
fn SessionsWorkspaceView(ws: ViewWs, ctx: Ctx) -> Element {
    let ws = ws.0;
    rsx! {
        {ws.render_with_header(
            move |kind, maximized| panel_body(kind, maximized, ctx),
            move |kind, _maximized| panel_header_actions(kind, ctx),
        )}
        {ws.dock()}
    }
}

// --- panel bodies ----------------------------------------------------------------

fn panel_body(kind: Panel, _maximized: bool, ctx: Ctx) -> Element {
    match kind {
        Panel::Graph => {
            // In the Sessions view with no world mounted there is nothing to
            // draw. With a world open on the embedded host the canvas mounts
            // the materialized `main` head as a client graph (see
            // `rematerialize_embedded`), so the hint is only for the
            // no-world-open case.
            if *ctx.view.read() == AppView::Sessions
                && api::WORLD_BASE.read().is_none()
                && ctx.active_world.read().is_none()
            {
                let embedded = api::session_manager_url().is_none();
                return rsx! {
                    div { class: "skeleton",
                        if embedded {
                            "embedded single-user mode: open a world in the Worlds panel to edit its graph locally"
                        } else {
                            "open a world in the Worlds panel to load its graph"
                        }
                    }
                };
            }
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
        Panel::Settings => panels::settings::panel(ctx),
        Panel::Filter => panels::filter::panel(ctx),
        Panel::Metrics => panels::metrics::panel(ctx),
        Panel::Instances => panels::instances::panel(ctx),
        Panel::Generate => panels::generate::panel(ctx),
        Panel::Timeline => panels::timeline::panel(ctx),
        Panel::Debug => panels::debug::panel(ctx),
        Panel::Worlds => panels::worlds::panel(ctx),
        Panel::History => panels::history::panel(ctx),
        Panel::Branches => panels::branches::panel(ctx),
        Panel::Merge => panels::merge::panel(ctx),
        Panel::GpuSessions => panels::gpu_sessions::panel(ctx),
        Panel::GitHub => panels::github::panel(ctx),
        Panel::Help => rsx! {
            div { class: "help",
                p { "canvas: drag rotate · wheel zoom · WASD pan · QE fwd/back · Shift boost · F fit · click select" }
                p { "nodes: Flat/Tags navigator → selected content editor; type for indexed search" }
                hr {}
                p { "🔵 tiling ⇄ floating" }
                p { "🟡 minimize → dock" }
                p { "🩷 maximize ⇄ restore" }
            }
        },
    }
}

fn panel_header_actions(kind: Panel, ctx: Ctx) -> Element {
    // The Layout surface is a Settings tab, so its segmented backend switch
    // (This Device / Compute Cluster + worker status dot) rides the
    // Settings panel's header-actions slot while that tab is active.
    if kind == Panel::Settings
        && panels::settings::active_tab() == panels::settings::SettingsTab::Layout
    {
        return panels::layout::backend_switch_header();
    }
    if kind != Panel::Graph {
        return rsx! {};
    }
    let remote = panels::layout::active_is_remote();
    let following = *render::SIM_RUNNING.read();
    let disabled = ctx.graph.read().is_none();
    rsx! {
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
