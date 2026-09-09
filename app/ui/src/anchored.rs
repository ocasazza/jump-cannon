//! Canvas hover/click focus engine + node hints.
//!
//! Ports of the egui pipeline at commit 723af10:
//! - `crates/graph-renderer/src/ui/focus_set.rs` — FocusMode + compute.
//! - `crates/graph-renderer/src/app.rs` — `update_hover_focus` (50 ms
//!   raycast throttle, 250 ms release hold, sticky-beats-hover) and
//!   `apply_focus_set_to_gpu` (focused = sticky.or(hover) → dim mask;
//!   no-focus → filter-behavior dispatch).
//!
//! The mouse handlers in `graph_canvas.rs` feed `hover_at` / `canvas_click`
//! / `canvas_leave`; a 16 ms driver loop inside [`driver`] advances the
//! timers and pushes the focus dim mask. The hovered node and the selected
//! node are published to the header hint bar (`hints.rs`); `Super+V`
//! ([`view_node`]) opens the hinted node in the Inspector.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;

use crate::graph_canvas::GraphData;
use crate::hints;
use crate::panels::filter::{self, FilterBehavior};
use crate::proto::NodeMeta;
use crate::render;
use crate::Ctx;

// --- tuning constants (values + rationale from app.rs at 723af10) -------------

/// Hover throttle interval — ~50ms gives a comfortable 20 Hz max raycast
/// cadence (app.rs::HOVER_THROTTLE_MS).
const HOVER_THROTTLE_MS: f64 = 50.0;
/// Dwell on one node before its `/node/:id` meta is fetched for the hint
/// bar — sweeping the pointer across a cluster must not fan out fetches.
const HINT_META_DELAY_MS: f64 = 150.0;
/// Hover-release hold — keep the previous hover focus engaged this long
/// after the cursor leaves a node (app.rs::HOVER_HOLD_MS).
const HOVER_HOLD_MS: f64 = 250.0;

/// Hint-bar source keys.
const HINT_HOVER: &str = "node-hover";
const HINT_SELECTED: &str = "node-selected";

// --- focus sets (port of ui/focus_set.rs) --------------------------------------

/// Membership criterion for the focused community
/// (ui/focus_set.rs::FocusMode, same order / labels / default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum FocusMode {
    /// Focus disabled — only the focused node lights up.
    None,
    /// `metrics["community"][i] == focused community id`.
    #[default]
    SameCommunityId,
    /// Direct neighbors via the flat edge list (`[s,t,…]`).
    SharedEdge,
    /// Any tag bucket containing the focused node lights up whole.
    SharedTag,
    /// Matches the active filter selection.
    Filter,
}

// The ALL/label/enabled surface + set_focus_mode/focused_node below are
// the Camera panel's wiring hooks (its Focus-mode picker still carries a
// `PARITY GAP` note); the panel agent connects them, so they're allowed
// to be unreferenced inside this file.
#[allow(dead_code)]
impl FocusMode {
    pub(crate) const ALL: &'static [FocusMode] = &[
        FocusMode::None,
        FocusMode::SameCommunityId,
        FocusMode::SharedEdge,
        FocusMode::SharedTag,
        FocusMode::Filter,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            FocusMode::None => "None (single node)",
            FocusMode::SameCommunityId => "Same community id",
            FocusMode::SharedEdge => "Shared edge",
            FocusMode::SharedTag => "Shared tag",
            FocusMode::Filter => "Active filter",
        }
    }

    /// All modes enabled (matches the egui app post field_index plumb).
    pub(crate) fn enabled(self) -> bool {
        true
    }
}

/// Return the node-index set that belongs to the focused community under
/// `mode`; the focused node itself is always included
/// (ui/focus_set.rs::compute). The egui `FocusCtx` borrow-bag collapses to
/// three arguments here; the `field_index` / `query` members read the
/// filter panel's module globals directly (SharedTag + Filter arms).
pub(crate) fn compute_focus_set(
    focused_idx: u32,
    mode: FocusMode,
    n_nodes: u32,
    metrics: &HashMap<String, Vec<f32>>,
    edges: &[u32],
) -> HashSet<u32> {
    let mut set = HashSet::new();
    if focused_idx >= n_nodes {
        return set;
    }
    set.insert(focused_idx);
    match mode {
        FocusMode::None => {}
        FocusMode::SameCommunityId => {
            let Some(comm) = metrics.get("community") else {
                return set;
            };
            let Some(&target) = comm.get(focused_idx as usize) else {
                return set;
            };
            for (j, &v) in comm.iter().enumerate() {
                if (v - target).abs() < 0.5 {
                    set.insert(j as u32);
                }
            }
        }
        FocusMode::SharedEdge => {
            for chunk in edges.chunks_exact(2) {
                if chunk[0] == focused_idx {
                    set.insert(chunk[1]);
                } else if chunk[1] == focused_idx {
                    set.insert(chunk[0]);
                }
            }
        }
        FocusMode::SharedTag => {
            // Reverse-lookup via the field index: union every `tags`
            // bucket that contains `focused_idx` (buckets are sorted +
            // deduped by FieldIndex::from_proto, so binary_search holds).
            if let Some(Ok(fi)) = filter::FIELD_INDEX.peek().as_ref() {
                if let Some(buckets) = fi.by_field.get("tags") {
                    for idxs in buckets.values() {
                        if idxs.binary_search(&focused_idx).is_ok() {
                            set.extend(idxs.iter().copied());
                        }
                    }
                }
            }
        }
        FocusMode::Filter => {
            if let Some(matched) = filter::current_matches() {
                set.extend(matched);
            }
        }
    }
    set
}

/// Active focus criterion. Default mirrors the egui `FocusState` default
/// (SameCommunityId). The Camera panel's Focus-mode picker should call
/// [`set_focus_mode`] (see the panel's `PARITY GAP` comment).
pub(crate) static FOCUS_MODE: GlobalSignal<FocusMode> = Signal::global(FocusMode::default);

/// Lazily-fetched per-node metric cache for the focus computation. The
/// egui app kept its bootstrap `metrics` map on `App` (FocusCtx::metrics);
/// this shell's `GraphData` doesn't retain metrics past color/size
/// derivation, so SameCommunityId re-fetches `/graph/metrics/community`
/// once per graph (keyed on n_nodes so a Generate swap refreshes it).
#[derive(Default, Clone, PartialEq)]
struct MetricCache {
    n_nodes: u32,
    metrics: HashMap<String, Vec<f32>>,
    in_flight: bool,
    /// Bumped on every cache write — participates in the focus push's
    /// change-detect key so a late metric arrival re-pushes the dim mask.
    version: u64,
}

static METRICS: GlobalSignal<MetricCache> = Signal::global(MetricCache::default);

fn ensure_metrics(n_nodes: u32) {
    {
        let c = METRICS.peek();
        if c.n_nodes == n_nodes && (c.in_flight || c.metrics.contains_key("community")) {
            return;
        }
    }
    let version = METRICS.peek().version + 1;
    *METRICS.write() = MetricCache { n_nodes, metrics: HashMap::new(), in_flight: true, version };
    spawn(async move {
        let fetched = crate::api::metric("community").await;
        let mut c = METRICS.write();
        if c.n_nodes != n_nodes {
            return; // a newer graph superseded this fetch
        }
        c.in_flight = false;
        c.version += 1;
        match fetched {
            // A length mismatch (e.g. a generated graph the server has no
            // vault metrics for) degrades SameCommunityId to {focused} —
            // the same graceful fallback focus_set.rs::compute uses for a
            // missing metric.
            Ok(Some(v)) if v.len() == n_nodes as usize => {
                c.metrics.insert("community".to_string(), v);
            }
            Ok(Some(v)) => tracing::warn!(
                "[anchored] community metric len {} != n_nodes {n_nodes}; focus degrades",
                v.len()
            ),
            Ok(None) => tracing::warn!("[anchored] server has no community metric; focus degrades"),
            Err(e) => tracing::warn!("[anchored] community metric fetch: {e}"),
        }
    });
}

/// Camera-panel hook: select a focus criterion. The driver loop
/// change-detects on the mode and re-pushes the dim mask next tick.
#[allow(dead_code)] // wired by the Camera panel's Focus-mode picker
pub(crate) fn set_focus_mode(mode: FocusMode) {
    if *FOCUS_MODE.peek() != mode {
        *FOCUS_MODE.write() = mode;
    }
}

/// The node currently driving focus dimming: sticky click beats hover
/// (app.rs::apply_focus_set_to_gpu's `focused` resolution). Exposed for
/// the Camera panel wiring.
#[allow(dead_code)] // wired by the Camera panel's Focus-mode picker
pub(crate) fn focused_node() -> Option<u32> {
    STICKY_IDX.peek().or(*HOVER_IDX.peek())
}

// --- hover / click state --------------------------------------------------------

/// Transient hover focus (app.rs::focus_hover_idx). Held for
/// HOVER_HOLD_MS after release; suppressed entirely while sticky is set.
static HOVER_IDX: GlobalSignal<Option<u32>> = Signal::global(|| None);
/// Sticky focused node from a click; click on empty canvas clears
/// (app.rs::focus_sticky_idx).
static STICKY_IDX: GlobalSignal<Option<u32>> = Signal::global(|| None);

/// Non-rendered bookkeeping (timers, change-detect mirrors, hint meta
/// cache). Thread-local rather than signal so the 60 Hz driver doesn't
/// dirty the component for state nothing renders from.
#[derive(Default)]
struct Timing {
    /// app.rs::last_hover_raycast_at (ms since epoch).
    last_raycast_ms: f64,
    /// app.rs::hover_clear_at.
    hover_clear_at: Option<f64>,
    /// Node the hover hint was last published for.
    hint_idx: Option<u32>,
    /// When the pointer landed on `hint_idx`; cleared once the meta fetch
    /// is kicked (or served from cache).
    hint_armed_at: Option<f64>,
    /// Id with an in-flight hover-hint meta fetch (dedupe + stale gate).
    hint_fetch_for: Option<String>,
    /// Last fetched hover meta; re-entering the same node skips the fetch.
    hint_meta: Option<NodeMeta>,
    /// Change-detect mirror for the focus-set push: (focused, mode,
    /// filter signature, mount generation, metric-cache version) — the
    /// egui focus_pushed_idx/mode/filter_pushed_sig trio, plus the mount
    /// generation so a canvas rebuild re-pushes the dim mask, plus the
    /// metric version so a late community-metric arrival does too.
    pushed: Option<(Option<u32>, FocusMode, u64, u64, u64)>,
}

thread_local! {
    static TIMING: RefCell<Timing> = RefCell::new(Timing::default());
}

// --- canvas-event entry points (called from graph_canvas.rs handlers) ----------

/// Filter-out gate shared by hover + click picking: when the filter
/// behavior is `Filter` (non-matches discarded by the shader), raycast
/// hits on filtered-out nodes are ignored — otherwise the rim + hint
/// would make an invisible node reappear (app.rs raw_hit gate).
/// `Focus` behavior keeps filtered nodes hoverable, which is the point.
fn pick_allowed(idx: u32) -> bool {
    if !matches!(*filter::BEHAVIOR.peek(), FilterBehavior::Filter) {
        return true;
    }
    filter::current_matches()
        .map(|set| set.contains(&idx))
        .unwrap_or(true)
}

/// Arm the hover-release hold timer if it isn't already running
/// (app.rs::maybe_clear_hover_after_hold's None arm; the elapsed-clear
/// arm runs in the driver loop).
fn arm_hover_clear(now: f64) {
    TIMING.with(|t| {
        let mut t = t.borrow_mut();
        if HOVER_IDX.peek().is_some() && t.hover_clear_at.is_none() {
            t.hover_clear_at = Some(now);
        }
    });
}

/// Throttled hover→focus pipeline (app.rs::update_hover_focus). Called
/// from the canvas mousemove handler while no drag is live.
pub(crate) fn hover_at(x: f32, y: f32) {
    // Sticky wins; a sticky-focus user gesture overrides hover (no hover
    // focus and no hover rim while a click selection is held).
    if STICKY_IDX.peek().is_some() {
        if HOVER_IDX.peek().is_some() {
            *HOVER_IDX.write() = None;
        }
        render::set_hover_feedback(None, None);
        return;
    }
    let now = js_sys::Date::now();
    let throttled = TIMING.with(|t| {
        let mut t = t.borrow_mut();
        if now - t.last_raycast_ms < HOVER_THROTTLE_MS {
            return true;
        }
        t.last_raycast_ms = now;
        false
    });
    if throttled {
        return;
    }
    let hit = render::pick(x, y).filter(|&i| pick_allowed(i));
    match hit {
        Some(idx) => {
            if *HOVER_IDX.peek() != Some(idx) {
                *HOVER_IDX.write() = Some(idx);
            }
            // Hovering — cancel any pending clear timer. Node hover takes
            // priority over edge hover.
            TIMING.with(|t| t.borrow_mut().hover_clear_at = None);
            render::set_hover_feedback(Some(idx), None);
        }
        None => {
            // Fall back to edge picking — only highlights when no node is
            // under the cursor. Node hover focus holds for HOVER_HOLD_MS.
            let edge = render::pick_edge(x, y);
            render::set_hover_feedback(*HOVER_IDX.peek(), edge);
            arm_hover_clear(now);
        }
    }
}

/// Click pick + the egui click semantics (app.rs `if let Some((rect,
/// pos)) = click` block). Returns the accepted node hit so the caller can
/// mirror it into `selected` (the egui `selected_node_idx`).
///
/// - node hit → sticky focus;
/// - empty canvas → clear sticky focus only. The `selected` signal
///   intentionally survives (egui: "clicking empty canvas should not
///   dismiss a promoted panel").
pub(crate) fn canvas_click(x: f32, y: f32) -> Option<u32> {
    let hit = render::pick(x, y).filter(|&i| pick_allowed(i));
    match hit {
        Some(idx) => {
            *STICKY_IDX.write() = Some(idx);
            // Sticky suppresses hover focus + rim immediately.
            if HOVER_IDX.peek().is_some() {
                *HOVER_IDX.write() = None;
            }
            render::set_hover_feedback(None, None);
            Some(idx)
        }
        None => {
            if STICKY_IDX.peek().is_some() {
                *STICKY_IDX.write() = None;
            }
            None
        }
    }
}

/// Pointer left the canvas: edge hover clears immediately; node hover
/// focus (and the rim) holds for HOVER_HOLD_MS, so a quick gap between
/// two nodes doesn't flash everything bright.
pub(crate) fn canvas_leave() {
    render::set_hover_feedback(*HOVER_IDX.peek(), None);
    arm_hover_clear(js_sys::Date::now());
}

/// `Super+V`: open the hinted node — hovered first, else the current
/// selection — in the Inspector. Returns whether there was a target.
pub(crate) fn view_node(mut ctx: Ctx) -> bool {
    let id = HOVER_IDX
        .peek()
        .and_then(|idx| id_for(&ctx.graph, idx))
        .or_else(|| ctx.selected.peek().clone());
    let Some(id) = id else {
        return false;
    };
    if ctx.selected.peek().as_deref() != Some(id.as_str()) {
        ctx.selected.set(Some(id));
    }
    *crate::OPEN_PANEL.write() = Some(crate::Panel::Inspector);
    true
}

// --- hover hint -------------------------------------------------------------------

fn kick_hint_fetch(ctx: Ctx, id: String) {
    let already = TIMING.with(|t| t.borrow().hint_fetch_for.as_deref() == Some(id.as_str()));
    if already {
        return;
    }
    TIMING.with(|t| t.borrow_mut().hint_fetch_for = Some(id.clone()));
    let epoch = ctx.graph_session.peek().epoch;
    spawn(async move {
        let fetched = crate::api::node_meta(&id).await;
        let live = TIMING.with(|t| {
            let mut t = t.borrow_mut();
            let live = t.hint_fetch_for.as_deref() == Some(id.as_str());
            if live {
                t.hint_fetch_for = None;
            }
            live
        });
        // A newer hover or a graph replacement retires this response.
        if !live || ctx.graph_session.peek().epoch != epoch {
            return;
        }
        match fetched {
            Ok(m) => {
                let hovered = HOVER_IDX.peek().and_then(|idx| id_for(&ctx.graph, idx));
                if hovered.as_deref() == Some(id.as_str()) {
                    hints::publish(hints::node_hint(HINT_HOVER, &id, Some(&m)));
                }
                TIMING.with(|t| t.borrow_mut().hint_meta = Some(m));
            }
            Err(e) => tracing::warn!("[anchored] hint meta fetch {id}: {e}"),
        }
    });
}

/// Hover-hint step of the driver tick: publish on hover change (cached
/// meta or bare id), fetch meta after a short dwell, clear on hover-out.
fn tick_hover_hint(ctx: Ctx, now: f64) {
    let hover = *HOVER_IDX.peek();
    let changed = TIMING.with(|t| t.borrow().hint_idx != hover);
    if changed {
        let id = hover.and_then(|idx| id_for(&ctx.graph, idx));
        let hint = TIMING.with(|t| {
            let mut t = t.borrow_mut();
            t.hint_idx = hover;
            t.hint_fetch_for = None;
            t.hint_armed_at = None;
            let id = id.as_deref()?;
            let t = &mut *t;
            let cached = t.hint_meta.as_ref().filter(|m| m.id == id);
            // A cache hit needs no dwell timer; a miss arms the fetch.
            if cached.is_none() {
                t.hint_armed_at = Some(now);
            }
            Some(hints::node_hint(HINT_HOVER, id, cached))
        });
        match hint {
            Some(h) => hints::publish(h),
            None => hints::clear(HINT_HOVER),
        }
        return;
    }
    let Some(idx) = hover else {
        return;
    };
    let dwelled = TIMING.with(|t| {
        t.borrow().hint_armed_at.map(|at| now - at >= HINT_META_DELAY_MS).unwrap_or(false)
    });
    if !dwelled {
        return;
    }
    TIMING.with(|t| t.borrow_mut().hint_armed_at = None);
    if !ctx.graph_session.peek().is_server_backed() {
        return;
    }
    if let Some(id) = id_for(&ctx.graph, idx) {
        kick_hint_fetch(ctx, id);
    }
}

// --- driver loop ----------------------------------------------------------------

/// Stable signature of the last successfully applied expression + behavior.
/// Draft edits do not disturb the active focus set until validation/evaluation
/// succeeds. It also repairs the dim mask after `filter::sync_gpu` clobbers a
/// node-focus write (the egui app re-ran change-detected every frame; here the
/// next 16 ms tick catches it).
fn filter_sig() -> u64 {
    filter::applied_version().wrapping_mul(2) | (*filter::BEHAVIOR.peek() as u64)
}

fn id_for(graph: &Signal<Option<GraphData>>, idx: u32) -> Option<String> {
    graph.peek().as_ref().and_then(|g| g.ids.get(idx as usize).cloned())
}

/// One 16 ms tick: hover-release hold → hover hint → focus-set dim push.
/// The hold and the push mirror the egui per-frame calls
/// (`maybe_clear_hover_after_hold`, `apply_focus_set_to_gpu`).
fn drive(ctx: Ctx) {
    let now = js_sys::Date::now();
    let graph = ctx.graph;

    // -- hover-release hold ------------------------------------------------
    let clear = TIMING.with(|t| {
        let mut t = t.borrow_mut();
        match t.hover_clear_at {
            Some(at) if now - at >= HOVER_HOLD_MS => {
                t.hover_clear_at = None;
                true
            }
            _ => false,
        }
    });
    if clear && HOVER_IDX.peek().is_some() {
        *HOVER_IDX.write() = None;
        render::set_hover_feedback(None, None);
    }

    tick_hover_hint(ctx, now);

    // -- focus-set dim push --------------------------------------------------
    let focused = STICKY_IDX.peek().or(*HOVER_IDX.peek());
    if focused.is_some() {
        // Warm the community-metric cache as soon as any node is focused
        // so the SameCommunityId arm has data by the time it computes.
        if let Some(g) = graph.peek().as_ref() {
            ensure_metrics(g.n_nodes);
        }
    }
    let mode = *FOCUS_MODE.peek();
    let sig = filter_sig();
    let generation = render::mount_generation();
    let mver = METRICS.peek().version;
    let key = (focused, mode, sig, generation, mver);
    let stale = TIMING.with(|t| t.borrow().pushed != Some(key));
    if stale {
        match focused {
            Some(idx) => {
                if let Some(g) = graph.peek().as_ref() {
                    let mc = METRICS.peek();
                    let members =
                        compute_focus_set(idx, mode, g.n_nodes, &mc.metrics, &g.scene.edges);
                    render::push_focus_set(Some(idx), &members);
                }
            }
            // No node focus → the filter panel owns the dim/mask dispatch
            // (the egui no-focus arm of apply_focus_set_to_gpu is exactly
            // its sync_gpu).
            None => filter::sync_gpu(),
        }
        TIMING.with(|t| t.borrow_mut().pushed = Some(key));
    }
}

// --- driver component -------------------------------------------------------------

/// Hover/click focus driver, mounted unconditionally at the app root.
/// Renders nothing; hosts the tick loop and the selection-hint effect.
pub(crate) fn driver(ctx: Ctx) -> Element {
    rsx! {
        FocusDriver { ctx }
    }
}

#[component]
fn FocusDriver(ctx: Ctx) -> Element {
    // The driver loop. 16 ms ≈ one vsync tick — the same cadence the egui
    // app got from running inside its per-frame update. (The renderer's
    // rAF closure can't host this: GlobalSignal writes need the Dioxus
    // runtime context.)
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(16).await;
            drive(ctx);
        }
    });

    // Selection hint: follows `selected` and upgrades from bare id to the
    // Inspector's meta once main.rs's selection fetch lands.
    use_effect(move || match ctx.selected.read().clone() {
        Some(id) => {
            let meta = ctx.meta.read();
            let meta = meta.as_ref().filter(|m| m.id == id);
            hints::publish(hints::node_hint(HINT_SELECTED, &id, meta));
        }
        None => hints::clear(HINT_SELECTED),
    });

    rsx! {}
}
