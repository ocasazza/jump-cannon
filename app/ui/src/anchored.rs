//! Anchored hover/click node cards + focus sets (phase 4).
//!
//! Parity targets at commit 723af10:
//! - `crates/graph-renderer/src/ui/anchored.rs` — world-space anchor
//!   projected through the same proj*view as the renderer, EMA smoothing,
//!   screen-edge clamping, tether line / off-screen arrow.
//! - `crates/graph-renderer/src/ui/focus_set.rs` — FocusMode + compute.
//! - `crates/graph-renderer/src/app.rs` — the hover/click pipeline that
//!   drives both: `update_hover_focus` (50 ms raycast throttle, 250 ms
//!   release hold, sticky-beats-hover), `tick_hover_preview` (700 ms arm
//!   delay, cached-meta fast path), `render_anchored_panel` (EMA_ALPHA
//!   0.4 placement smoothing, offset (18,18), reserved 360×240),
//!   `apply_focus_set_to_gpu` (focused = sticky.or(hover) → dim mask;
//!   no-focus → filter-behavior dispatch), and the click handler
//!   (sticky focus + promoted card; empty-canvas click clears sticky but
//!   NOT the promoted card).
//!
//! Architecture: the egui app ran all of this per-frame inside
//! `App::update`. Here the mouse handlers in `graph_canvas.rs` feed
//! `hover_at` / `canvas_click` / `canvas_leave`, and a 16 ms driver loop
//! inside the overlay component advances the timers, projects the
//! anchors through `render::project_node`, EMA-smooths, and pushes the
//! focus dim mask — so cards track their node while the camera moves.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;

use crate::graph_canvas::GraphData;
use crate::panels::filter::{self, FilterBehavior};
use crate::proto::NodeMeta;
use crate::render;
use crate::Ctx;

// --- tuning constants (values + rationale from app.rs at 723af10) -------------

/// Hover throttle interval — ~50ms gives a comfortable 20 Hz max raycast
/// cadence (app.rs::HOVER_THROTTLE_MS).
const HOVER_THROTTLE_MS: f64 = 50.0;
/// Delay between cursor-landed-on-node and the preview card opening
/// (app.rs::HOVER_PREVIEW_DELAY_MS).
const HOVER_PREVIEW_DELAY_MS: f64 = 700.0;
/// Hover-release hold — keep the previous hover focus engaged this long
/// after the cursor leaves a node (app.rs::HOVER_HOLD_MS).
const HOVER_HOLD_MS: f64 = 250.0;
/// EMA blend factor for the projected screen position. Below 0.4 the card
/// visibly lags fast camera pans; above 0.6 force-sim jitter reappears
/// (app.rs::render_anchored_panel re-validated 0.4).
const EMA_ALPHA: f32 = 0.4;
/// Inset from the viewport edge when the card is clamped
/// (ui/anchored.rs::clamp_margin default).
const CLAMP_MARGIN: f32 = 40.0;
/// Hover card auto-position nudge below-and-right of the anchor so the
/// node glyph isn't covered (app.rs uses .offset(vec2(18,18))).
const HOVER_OFFSET: (f32, f32) = (18.0, 18.0);
/// Promoted card nudge (ui/anchored.rs::new default offset 16,16).
const PROMOTED_OFFSET: (f32, f32) = (16.0, 16.0);
/// Hover card reserved size for pre-clamping (app.rs:
/// .reserved_size(vec2(360,240)), tracking set_max_width(360)).
const HOVER_RESERVED: (f32, f32) = (360.0, 240.0);
/// Promoted card reserved size — wider/taller for the metrics + actions
/// block (the egui promoted form was a 480×600 FloatingPanel; anchored
/// here, see the parity note on `card`).
const PROMOTED_RESERVED: (f32, f32) = (380.0, 420.0);

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
            if let Some(Ok(fi)) = filter::FIELD_INDEX.peek().as_ref() {
                if let Some(matched) = fi.matches(&filter::QUERY.peek().active_filters) {
                    set.extend(matched);
                }
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
            Ok(v) if v.len() == n_nodes as usize => {
                c.metrics.insert("community".to_string(), v);
            }
            Ok(v) => tracing::warn!(
                "[anchored] community metric len {} != n_nodes {n_nodes}; focus degrades",
                v.len()
            ),
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
/// Sticky-promoted card idx — set on click, cleared on ✕
/// (app.rs::promoted_anchored_idx; empty-canvas click does NOT clear it).
static PROMOTED: GlobalSignal<Option<u32>> = Signal::global(|| None);
/// NodeMeta for the promoted card (app.rs::promoted_anchored_meta).
static PROMOTED_META: GlobalSignal<Option<NodeMeta>> = Signal::global(|| None);

/// Hover-preview card state machine (app.rs::hover_preview_*). `meta`
/// doubles as the re-hover cache: it survives close so re-entering the
/// same node skips the refetch.
#[derive(Clone, PartialEq, Default)]
struct Preview {
    idx: Option<u32>,
    open: bool,
    meta: Option<NodeMeta>,
}

static PREVIEW: GlobalSignal<Preview> = Signal::global(Preview::default);

/// Per-card projected placement, written by the driver loop.
/// `(x, y)` is the EMA-smoothed projection (panel placement);
/// `(ax, ay)` is the live projection (tether target — the arrow tracks
/// the true node while the panel is allowed to lead/lag, exactly the
/// `screen_pos_override` split in ui/anchored.rs). All canvas-local CSS px.
#[derive(Clone, Copy, PartialEq)]
struct CardPlace {
    x: f32,
    y: f32,
    ax: f32,
    ay: f32,
    on_screen: bool,
}

/// One overlay frame: canvas viewport rect + active card placements.
/// Nodes behind the camera have no entry (egui `BehindCamera` → hide).
#[derive(Clone, PartialEq, Default)]
struct OverlayFrame {
    /// (left, top, width, height) of the canvas in viewport CSS px.
    canvas: Option<(f32, f32, f32, f32)>,
    places: HashMap<u32, CardPlace>,
}

static FRAME: GlobalSignal<OverlayFrame> = Signal::global(OverlayFrame::default);

/// Non-rendered bookkeeping (timers, EMA history, change-detect mirrors).
/// Thread-local rather than signal so the 60 Hz driver doesn't dirty the
/// component for state nothing renders from.
#[derive(Default)]
struct Timing {
    /// app.rs::last_hover_raycast_at (ms since epoch).
    last_raycast_ms: f64,
    /// app.rs::hover_clear_at.
    hover_clear_at: Option<f64>,
    /// app.rs::hover_preview_armed_at.
    armed_at: Option<f64>,
    /// app.rs::last_anchored_screen_pos — EMA per node idx. Grows per
    /// hovered node, like the egui map (never pruned mid-session).
    ema: HashMap<u32, (f32, f32)>,
    /// Change-detect mirror for the focus-set push: (focused, mode,
    /// filter signature, mount generation, metric-cache version) — the
    /// egui focus_pushed_idx/mode/filter_pushed_sig trio, plus the mount
    /// generation so a canvas rebuild re-pushes the dim mask, plus the
    /// metric version so a late community-metric arrival does too.
    pushed: Option<(Option<u32>, FocusMode, u64, u64, u64)>,
    /// Ids with an in-flight `/node/:id` fetch (dedupe).
    preview_fetch_for: Option<String>,
    promoted_fetch_for: Option<String>,
}

thread_local! {
    static TIMING: RefCell<Timing> = RefCell::new(Timing::default());
}

// --- canvas-event entry points (called from graph_canvas.rs handlers) ----------

/// Filter-out gate shared by hover + click picking: when the filter
/// behavior is `Filter` (non-matches discarded by the shader), raycast
/// hits on filtered-out nodes are ignored — otherwise the card + tether
/// would make an invisible node visually reappear (app.rs raw_hit gate).
/// `Focus` behavior keeps filtered nodes hoverable, which is the point.
fn pick_allowed(idx: u32) -> bool {
    if !matches!(*filter::BEHAVIOR.peek(), FilterBehavior::Filter) {
        return true;
    }
    filter::FIELD_INDEX
        .peek()
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|fi| fi.matches(&filter::QUERY.peek().active_filters))
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
/// - node hit → sticky focus + promote the anchored card (reusing the
///   hover preview's cached meta to avoid an empty-render flash, then a
///   fresh `/node/:id` fetch);
/// - empty canvas → clear sticky focus only. The promoted card and the
///   `selected` signal intentionally survive (egui: "clicking empty
///   canvas should not dismiss a promoted panel").
pub(crate) fn canvas_click(x: f32, y: f32, g: &GraphData) -> Option<u32> {
    let hit = render::pick(x, y).filter(|&i| pick_allowed(i));
    match hit {
        Some(idx) => {
            *STICKY_IDX.write() = Some(idx);
            // Sticky suppresses hover focus + rim immediately.
            if HOVER_IDX.peek().is_some() {
                *HOVER_IDX.write() = None;
            }
            render::set_hover_feedback(None, None);

            let prev = *PROMOTED.peek();
            *PROMOTED.write() = Some(idx);
            if prev != Some(idx) {
                if let Some(id) = g.ids.get(idx as usize).cloned() {
                    tracing::info!("[anchored] promote idx={idx} id={id}");
                    // Swap: seed from the hover preview's cache when the id
                    // matches, drop stale meta otherwise, kick a fetch.
                    let cached = PREVIEW.peek().meta.clone().filter(|m| m.id == id);
                    *PROMOTED_META.write() = cached;
                    kick_promoted_fetch(id);
                } else {
                    *PROMOTED_META.write() = None;
                }
            }
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

/// ✕ on the promoted card (app.rs::dismiss_promoted_node — sticky focus
/// is NOT cleared here; that's the empty-canvas click's job).
fn dismiss_promoted() {
    *PROMOTED.write() = None;
    *PROMOTED_META.write() = None;
    TIMING.with(|t| t.borrow_mut().promoted_fetch_for = None);
}

// --- async meta fetches ---------------------------------------------------------

fn kick_preview_fetch(id: String) {
    let already = TIMING.with(|t| t.borrow().preview_fetch_for == Some(id.clone()));
    if already {
        return;
    }
    TIMING.with(|t| t.borrow_mut().preview_fetch_for = Some(id.clone()));
    spawn(async move {
        match crate::api::node_meta(&id).await {
            Ok(m) => {
                // Render gates on meta.id matching the hovered node's id,
                // so a stale arrival for a previous node is harmless — it
                // just refreshes the cache.
                PREVIEW.write().meta = Some(m);
            }
            Err(e) => tracing::warn!("[anchored] preview fetch {id}: {e}"),
        }
        TIMING.with(|t| {
            let mut t = t.borrow_mut();
            if t.preview_fetch_for.as_deref() == Some(id.as_str()) {
                t.preview_fetch_for = None;
            }
        });
    });
}

fn kick_promoted_fetch(id: String) {
    TIMING.with(|t| t.borrow_mut().promoted_fetch_for = Some(id.clone()));
    spawn(async move {
        match crate::api::node_meta(&id).await {
            Ok(m) => {
                // Accept only if this fetch is still the live one (the
                // user may have clicked another node mid-flight).
                let current = TIMING.with(|t| t.borrow().promoted_fetch_for.clone());
                if current.as_deref() == Some(id.as_str()) {
                    *PROMOTED_META.write() = Some(m);
                }
            }
            Err(e) => tracing::warn!("[anchored] promoted fetch {id}: {e}"),
        }
    });
}

// --- driver loop ----------------------------------------------------------------

/// Stable signature of the active filter set + behavior, so a chip toggle
/// re-runs the focus push even when `focused` is unchanged (app.rs
/// filter_sig — BTreeMap iteration keeps the hash deterministic). It also
/// repairs the dim mask after `filter::sync_gpu` clobbers a node-focus
/// write (the egui app re-ran change-detected every frame; here the next
/// 16 ms tick catches it).
fn filter_sig() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let q = filter::QUERY.peek();
    let mut h = DefaultHasher::new();
    for (k, vs) in q.active_filters.by_field.iter() {
        k.hash(&mut h);
        for v in vs.iter() {
            v.hash(&mut h);
        }
        (q.active_filters.combinator_for(k) as u8).hash(&mut h);
    }
    (q.active_filters.cross_field_combinator as u8).hash(&mut h);
    (*filter::BEHAVIOR.peek() as u8).hash(&mut h);
    h.finish()
}

fn id_for(graph: &Signal<Option<GraphData>>, idx: u32) -> Option<String> {
    graph.peek().as_ref().and_then(|g| g.ids.get(idx as usize).cloned())
}

/// Close the hover preview, keeping the meta cache for re-entry
/// (app.rs::close_hover_preview keeps hover_preview_meta too).
fn close_preview() {
    let p = PREVIEW.peek().clone();
    if p.idx.is_some() || p.open {
        *PREVIEW.write() = Preview { idx: None, open: false, meta: p.meta };
    }
    TIMING.with(|t| t.borrow_mut().armed_at = None);
}

/// One 16 ms tick: hover-release hold → preview state machine → focus-set
/// dim push → card projection + EMA. The first three mirror the egui
/// per-frame calls (`maybe_clear_hover_after_hold`, `tick_hover_preview`,
/// `apply_focus_set_to_gpu`); the last is `render_anchored_panel`'s
/// project-and-smooth step hoisted out of paint.
fn drive(graph: Signal<Option<GraphData>>) {
    let now = js_sys::Date::now();

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

    // -- hover-preview state machine ----------------------------------------
    if STICKY_IDX.peek().is_some() {
        // Sticky-focus mode suppresses the preview — the promoted card
        // covers that node's detail surface.
        close_preview();
    } else {
        match *HOVER_IDX.peek() {
            Some(idx) => {
                let want_id = id_for(&graph, idx);
                let p = PREVIEW.peek().clone();
                if p.idx != Some(idx) {
                    // Landed on a new node: re-arm the delay; close any old
                    // card; keep the cached meta only if it's already for
                    // this id (no refetch on re-entry).
                    let keep = match (&p.meta, &want_id) {
                        (Some(m), Some(id)) => m.id == *id,
                        _ => false,
                    };
                    *PREVIEW.write() = Preview {
                        idx: Some(idx),
                        open: false,
                        meta: if keep { p.meta } else { None },
                    };
                    TIMING.with(|t| t.borrow_mut().armed_at = Some(now));
                } else if !p.open {
                    let armed_long_enough = TIMING.with(|t| {
                        t.borrow().armed_at.map(|at| now - at >= HOVER_PREVIEW_DELAY_MS)
                    })
                    .unwrap_or(false);
                    if armed_long_enough {
                        PREVIEW.write().open = true;
                        if let Some(id) = want_id {
                            let cached =
                                p.meta.as_ref().map(|m| m.id == id).unwrap_or(false);
                            if !cached {
                                kick_preview_fetch(id);
                            }
                        }
                    }
                }
            }
            None => close_preview(),
        }
    }

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

    // -- card anchors: project + EMA ------------------------------------------
    let mut places: HashMap<u32, CardPlace> = HashMap::new();
    let mut want: Vec<u32> = Vec::new();
    {
        let p = PREVIEW.peek();
        // Skip the hover card when it would double the promoted card
        // (app.rs::show_hover_preview's `Some(hidx) != promoted_idx` gate).
        if p.open {
            if let Some(idx) = p.idx {
                if Some(idx) != *PROMOTED.peek() && p.meta.is_some() {
                    want.push(idx);
                }
            }
        }
    }
    if let Some(idx) = *PROMOTED.peek() {
        if PROMOTED_META.peek().is_some() {
            want.push(idx);
        }
    }
    for idx in want {
        // Behind camera → no place → card hidden (egui BehindCamera arm).
        if let Some((sx, sy, on_screen)) = render::project_node(idx) {
            let (ex, ey) = TIMING.with(|t| {
                let mut t = t.borrow_mut();
                let e = t.ema.entry(idx).or_insert((sx, sy));
                e.0 += (sx - e.0) * EMA_ALPHA;
                e.1 += (sy - e.1) * EMA_ALPHA;
                *e
            });
            places.insert(idx, CardPlace { x: ex, y: ey, ax: sx, ay: sy, on_screen });
        }
    }
    let frame = OverlayFrame { canvas: render::canvas_rect(), places };
    if *FRAME.peek() != frame {
        *FRAME.write() = frame;
    }
}

// --- overlay component -----------------------------------------------------------

/// Hover/click card overlay, rendered unconditionally at the app root
/// (empty unless a card is anchored).
pub(crate) fn overlay(ctx: Ctx) -> Element {
    rsx! {
        AnchoredOverlay { graph: ctx.graph, selected: ctx.selected }
    }
}

#[component]
fn AnchoredOverlay(graph: Signal<Option<GraphData>>, selected: Signal<Option<String>>) -> Element {
    // The driver loop. 16 ms ≈ one vsync tick — the same cadence the egui
    // app got from running inside its per-frame update. (The renderer's
    // rAF closure can't host this: GlobalSignal writes need the Dioxus
    // runtime context.)
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(16).await;
            drive(graph);
        }
    });

    let frame = FRAME.read().clone();
    let Some(canvas) = frame.canvas else {
        return rsx! {};
    };
    let promoted = *PROMOTED.read();
    let preview = PREVIEW.read().clone();

    // Hover card: open + meta arrived for the right id + not doubling the
    // promoted card.
    let hover_card = match (preview.open, preview.idx, preview.meta) {
        (true, Some(idx), Some(meta)) if Some(idx) != promoted => {
            let id_ok = id_for(&graph, idx).map(|id| meta.id == id).unwrap_or(false);
            match (id_ok, frame.places.get(&idx)) {
                (true, Some(pl)) => Some((idx, meta, *pl)),
                _ => None,
            }
        }
        _ => None,
    };
    let promoted_card = match (promoted, PROMOTED_META.read().clone()) {
        (Some(idx), Some(meta)) => frame.places.get(&idx).map(|pl| (idx, meta, *pl)),
        _ => None,
    };

    rsx! {
        div { class: "anch-layer",
            if let Some((idx, meta, pl)) = hover_card {
                {card(idx, meta, pl, canvas, false, selected)}
            }
            if let Some((idx, meta, pl)) = promoted_card {
                {card(idx, meta, pl, canvas, true, selected)}
            }
        }
    }
}

// --- card geometry + rendering ----------------------------------------------------

/// Midpoint of the edge of `rect` closest to `target` — edges, not
/// corners, so the tether reads as "coming out of the side of the card"
/// (ui/anchored.rs::closest_edge_midpoint, verbatim math). `rect` is
/// (left, top, w, h) in viewport px.
fn closest_edge_midpoint(rect: (f32, f32, f32, f32), tx: f32, ty: f32) -> (f32, f32) {
    let (l, t, w, h) = rect;
    let cx = l + w * 0.5;
    let cy = t + h * 0.5;
    let dx = tx - cx;
    let dy = ty - cy;
    // Compare in units of half-extents so a wide-but-short panel doesn't
    // always prefer its left/right edges.
    let ax = dx.abs() / (w * 0.5).max(0.0001);
    let ay = dy.abs() / (h * 0.5).max(0.0001);
    if ax > ay {
        if dx > 0.0 { (l + w, cy) } else { (l, cy) }
    } else if dy > 0.0 {
        (cx, t + h)
    } else {
        (cx, t)
    }
}

/// Title fallback chain (app.rs::render_anchored_panel header / node_title):
/// title → path file stem → id → "Node".
fn node_title(meta: &NodeMeta) -> String {
    if !meta.title.is_empty() {
        meta.title.clone()
    } else if !meta.path.is_empty() {
        std::path::Path::new(&meta.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| meta.id.clone())
    } else if !meta.id.is_empty() {
        meta.id.clone()
    } else {
        "Node".to_string()
    }
}

/// Truncate a markdown body to a hover-preview snippet: up to `max_chars`
/// chars OR `max_lines` lines, whichever bound trips first. Appends "…"
/// when truncated; preserves line breaks (app.rs::body_snippet, verbatim).
fn body_snippet(body: &str, max_chars: usize, max_lines: usize) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut lines = 0usize;
    for line in body.lines() {
        if lines >= max_lines {
            out.push('…');
            return out;
        }
        // Skip leading blank lines so a body that starts with `\n` doesn't
        // burn a snippet "line" on emptiness.
        if out.is_empty() && line.trim().is_empty() {
            continue;
        }
        let remaining = max_chars.saturating_sub(chars);
        if remaining == 0 {
            out.push('…');
            return out;
        }
        let take = line.chars().count().min(remaining);
        out.extend(line.chars().take(take));
        if take < line.chars().count() {
            out.push('…');
            return out;
        }
        chars += take;
        lines += 1;
        if lines < max_lines && chars < max_chars {
            out.push('\n');
            chars += 1;
        }
    }
    out.trim_end().to_string()
}

/// Render one anchored card (compact hover preview or the promoted click
/// card) plus its tether.
///
/// Placement = EMA-smoothed projection + offset, pre-clamped so the
/// reserved rect stays inside `canvas.shrink(CLAMP_MARGIN)` — the
/// `reserved_size` regime of ui/anchored.rs (the hover card always passes
/// one, so the legacy anchor-only clamp arm never fires here). The tether
/// aims at the LIVE projection. On-screen anchor → straight line + dot;
/// off-screen → triangular arrow at the card edge pointing toward the
/// anchor's bearing (the projected point still encodes direction even
/// when outside the viewport).
///
/// Parity note: the egui promoted form was a FloatingPanel hosting the
/// full inspector body. This app keeps the Inspector/Document panels as
/// the deep-dive surface (selection is set on the same click), so the
/// promoted card carries the anchored summary: title, path, tags, the
/// NodeMeta metric block, body snippet, and actions (⌖ fly-to, inspect,
/// ✕ close).
fn card(
    idx: u32,
    meta: NodeMeta,
    pl: CardPlace,
    canvas: (f32, f32, f32, f32),
    promoted: bool,
    mut selected: Signal<Option<String>>,
) -> Element {
    let (cl, ct, cw, chh) = canvas;
    let (off_x, off_y) = if promoted { PROMOTED_OFFSET } else { HOVER_OFFSET };
    let (rw, rh) = if promoted { PROMOTED_RESERVED } else { HOVER_RESERVED };

    // Pre-clamp the reserved rect into the canvas (canvas-local space).
    let raw_x = pl.x + off_x;
    let raw_y = pl.y + off_y;
    let min_x = CLAMP_MARGIN;
    let max_x = (cw - CLAMP_MARGIN - rw).max(min_x);
    let min_y = CLAMP_MARGIN;
    let max_y = (chh - CLAMP_MARGIN - rh).max(min_y);
    let px = cl + raw_x.clamp(min_x, max_x);
    let py = ct + raw_y.clamp(min_y, max_y);

    // Tether geometry (viewport space). The card's true height is
    // content-driven; the reserved height is a good-enough rect estimate
    // for picking the nearest edge midpoint.
    let anchor_x = cl + pl.ax;
    let anchor_y = ct + pl.ay;
    let (ex, ey) = closest_edge_midpoint((px, py, rw, rh), anchor_x, anchor_y);
    let dx = anchor_x - ex;
    let dy = anchor_y - ey;
    let len = (dx * dx + dy * dy).sqrt();
    let ang = dy.atan2(dx);

    let title = node_title(&meta);
    let tags = meta.tags.join(", ");
    let snippet = if promoted {
        body_snippet(&meta.body, 600, 12)
    } else {
        body_snippet(&meta.body, 280, 6)
    };
    let meta_id = meta.id.clone();
    let card_class = if promoted { "anch-card promoted" } else { "anch-card" };

    rsx! {
        // Tether: straight line + anchor dot when the anchor is visible;
        // angled arrow stub toward the off-screen anchor otherwise
        // (silently hiding the card would feel like a bug — see the
        // ui/anchored.rs module docs).
        if pl.on_screen {
            div {
                class: "anch-tether",
                style: "left:{ex}px;top:{ey}px;width:{len}px;transform:rotate({ang}rad);",
            }
            div { class: "anch-dot", style: "left:{anchor_x}px;top:{anchor_y}px;" }
        } else {
            div {
                class: "anch-arrow",
                style: "left:{ex}px;top:{ey}px;transform:rotate({ang}rad);",
            }
        }
        div {
            class: card_class,
            style: "left:{px}px;top:{py}px;",
            // Keep hover focus alive while the cursor is over the card:
            // the DOM card sits outside the <canvas>, so entering it fires
            // the canvas mouseleave — without this the 250 ms hold would
            // close the card under the cursor (the egui Area lived inside
            // the canvas rect, so it never had this problem).
            onmouseenter: move |_| {
                if !promoted {
                    TIMING.with(|t| t.borrow_mut().hover_clear_at = None);
                }
            },
            onmouseleave: move |_| {
                if !promoted {
                    arm_hover_clear(js_sys::Date::now());
                }
            },
            div { class: "anch-title",
                span { class: "anch-glyph", "☰" }
                span { class: "anch-name", "{title}" }
                if promoted {
                    button {
                        class: "anch-close",
                        title: "dismiss",
                        onclick: move |_| dismiss_promoted(),
                        "✕"
                    }
                }
            }
            if !meta.path.is_empty() {
                div { class: "anch-path", "{meta.path}" }
            }
            if !tags.is_empty() {
                div { class: "anch-tags", "{tags}" }
            }
            if promoted {
                div { class: "anch-metrics",
                    {metric_row("degree", format!("{} ({}→ / →{})", meta.degree, meta.outdegree, meta.indegree))}
                    {metric_row("pagerank", format!("{:.4}", meta.pagerank))}
                    {metric_row("betweenness", format!("{:.4}", meta.betweenness))}
                    {metric_row("k-core", format!("{}", meta.kcore))}
                    {metric_row("community", format!("{}", meta.community))}
                    {metric_row("component", format!("{}", meta.wcc))}
                }
            }
            if !snippet.is_empty() {
                hr { class: "anch-sep" }
                div { class: "anch-body", "{snippet}" }
            }
            if promoted {
                div { class: "anch-actions",
                    button {
                        class: "btn",
                        title: "fly the camera to this node",
                        onclick: move |_| render::look_at_node(idx),
                        "⌖ focus"
                    }
                    button {
                        class: "btn",
                        title: "open in the Inspector / Document panels",
                        onclick: move |_| selected.set(Some(meta_id.clone())),
                        "inspect"
                    }
                }
            }
        }
    }
}

fn metric_row(k: &'static str, v: String) -> Element {
    rsx! {
        div { class: "anch-metric",
            span { class: "k", "{k}" }
            span { class: "v", "{v}" }
        }
    }
}
