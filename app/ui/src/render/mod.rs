//! wgpu graph renderer, ported from `crates/graph-renderer` (egui app) to
//! the Dioxus shell. Same WGSL shaders, same 6DoF camera, same in-process
//! GPU force layout — only the windowing/UI glue differs.
//!
//! Architecture: wgpu objects are not `Send` on wasm and Dioxus signals
//! want `'static + PartialEq` data, so the live [`RenderHost`] sits in a
//! thread-local (wasm is single-threaded) and the UI talks to it through
//! the free functions below. A `requestAnimationFrame` loop drives one
//! `RenderHost::frame()` per vsync tick while the canvas is mounted; the
//! Dioxus event handlers feed camera/selection state in between ticks.
//!
//! Lifecycle: panel-kit unmounts the Graph panel body when the panel is
//! minimized (the canvas element is destroyed), so the host is rebuilt on
//! every `onmounted`. Live sim positions are carried across the rebuild so
//! minimize/restore doesn't reset the layout.

// camera.rs is a verbatim copy of crates/graph-renderer/src/camera.rs —
// keep it byte-identical (incl. currently-unwired helpers like `reset` /
// `look_at_point`) so diffs against the source of truth stay trivial.
#[allow(dead_code)]
pub mod camera;
pub mod data;
pub mod pipelines;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

pub use pipelines::{GraphData as Scene, RenderHost};

/// DOM id of the graph canvas — shared with `graph_canvas.rs`.
pub const CANVAS_ID: &str = "graph-canvas";

/// Size multiplier applied to the selected node so a click/list selection
/// is visible on the canvas (per-node size emphasis — one of the channels
/// `graph_pipelines` supports; the egui app's white hover rim covers the
/// pointer-feedback case via the effects uniform instead).
const SELECT_EMPHASIS: f32 = 1.8;

// WASDQE pan ramp (mirrors ui/workspace.rs in the egui app). Starts at
// BASE units/s, ramps to MAX over RAMP seconds of continuous input.
// Shift multiplies on top. Resets on the first frame with no pan input
// so a quick tap stays a tap.
const PAN_BASE: f32 = 2400.0;
const PAN_MAX: f32 = 24000.0;
const PAN_RAMP: f32 = 0.32;
const SHIFT_MUL: f32 = 4.0;

/// Sign-preserving response curve for mouse-rotate deltas (verbatim from
/// the egui app's workspace.rs). Linear floor for sub-2px nudges, hard
/// ramp past ~10px so a hand-sweep produces a full rotation.
fn apply_rotate_curve(dx: f32) -> f32 {
    let a = dx.abs();
    dx + dx.signum() * a * a / 12.0 + dx.signum() * a * a * a / 900.0
}

/// UI-facing state that survives host rebuilds. Selection / highlights /
/// play-pause are re-applied to the fresh GPU buffers after a remount.
#[derive(Default)]
struct Ctl {
    /// Held WASDQE keys (lowercase). Consumed by the rAF tick.
    keys: HashSet<char>,
    shift: bool,
    /// True while the pointer is over the canvas — gates keyboard pan,
    /// matching the egui app's pointer_in_canvas gate.
    pointer_over: bool,
    pan_accel_t: f32,
    last_tick_ms: f64,
    selected: Option<u32>,
    highlights: Option<HashSet<u32>>,
    sim_running: bool,
    /// Monotonic mount generation — a stale async init must not clobber
    /// a newer mount's host.
    generation: u64,
}

thread_local! {
    static HOST: RefCell<Option<RenderHost>> = const { RefCell::new(None) };
    static CTL: RefCell<Ctl> = RefCell::new(Ctl { sim_running: true, ..Default::default() });
    static RAF_STARTED: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` against the live render host (None while the canvas is unmounted
/// or the async init is still in flight). This is the panels' doorway to the
/// renderer: `render::with_host(|h| h.pipes.set_…(…))` — the full setter
/// surface ported from graph_pipelines.rs lives on `h.pipes`, the 6DoF
/// camera on `h.pipes.camera`.
pub(crate) fn with_host<R>(f: impl FnOnce(&mut RenderHost) -> R) -> Option<R> {
    HOST.with(|h| h.borrow_mut().as_mut().map(f))
}

/// Monotonic canvas-mount generation — bumps every time the Graph panel
/// remounts and the host is rebuilt (back to the boot gpu-force layout).
/// The Layout panel keys its "already applied" detector on this so a
/// persisted engine re-applies onto a fresh host instead of being skipped.
pub(crate) fn mount_generation() -> u64 {
    CTL.with(|c| c.borrow().generation)
}

fn canvas_el() -> Option<web_sys::HtmlCanvasElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(CANVAS_ID)?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .ok()
}

/// (Re)build the render host against the currently mounted canvas.
/// Called from the canvas `onmounted` handler. If a previous host exists
/// with the same node count, its live (sim-evolved) positions seed the new
/// one so minimize/restore doesn't restart the layout from scratch.
pub fn mount_canvas(mut scene: Scene) {
    let generation = CTL.with(|c| {
        let mut c = c.borrow_mut();
        c.generation += 1;
        c.generation
    });

    let carried = HOST.with(|h| {
        h.borrow().as_ref().and_then(|host| {
            let live = host.pipes.positions_cpu();
            (live.len() == scene.positions.len()).then(|| live.to_vec())
        })
    });
    if let Some(p) = carried {
        scene.positions = p;
    }

    wasm_bindgen_futures::spawn_local(async move {
        let Some(canvas) = canvas_el() else {
            tracing::warn!("[render] mount: canvas #{CANVAS_ID} not in DOM");
            return;
        };
        match RenderHost::new(canvas, scene).await {
            Ok(host) => {
                let still_current = CTL.with(|c| c.borrow().generation == generation);
                if !still_current {
                    return; // a newer mount superseded this init
                }
                HOST.with(|h| *h.borrow_mut() = Some(host));
                reapply_ctl_state();
                start_raf_loop();
            }
            Err(e) => tracing::error!("[render] wgpu init failed: {e}"),
        }
    });
}

/// Push selection / highlight / play-pause state into (possibly fresh)
/// GPU buffers.
fn reapply_ctl_state() {
    let (selected, highlights, sim_running) = CTL.with(|c| {
        let c = c.borrow();
        (c.selected, c.highlights.clone(), c.sim_running)
    });
    with_host(|h| {
        let (pipes, queue) = h.pipes_and_queue();
        pipes.set_selected_emphasis(queue, selected, SELECT_EMPHASIS);
        pipes.set_selected(queue, highlights.as_ref());
        pipes.set_sim_running(sim_running);
    });
}

/// The continuous frame loop. Started once; runs for the lifetime of the
/// page (a no-op tick while no host exists or the canvas is unmounted is
/// far cheaper than tearing the closure down and re-arming it).
fn start_raf_loop() {
    if RAF_STARTED.with(|s| s.replace(true)) {
        return;
    }
    // Classic self-rescheduling rAF closure: the Rc<RefCell<Option<…>>>
    // knot lets the closure reference itself for the next request.
    let f: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move || {
        tick();
        if let Some(w) = web_sys::window() {
            if let Some(cb) = f.borrow().as_ref() {
                let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
            }
        }
    }));
    if let Some(w) = web_sys::window() {
        if let Some(cb) = g.borrow().as_ref() {
            let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }
}

/// One animation tick: consume held-key pan, then render a frame.
fn tick() {
    let now = js_sys::Date::now();
    let (pan, dt_valid) = CTL.with(|c| {
        let mut c = c.borrow_mut();
        let dt = (((now - c.last_tick_ms) / 1000.0) as f32).clamp(0.0, 0.05);
        c.last_tick_ms = now;

        // Axis sums use the project convention (see the egui app's
        // ui/input.rs): A/D strafe (x), W/S vertical (y), Q/E
        // forward/back (z) — swapped from FPS, Minecraft-creative style.
        let axis = |neg: char, pos: char| -> f32 {
            (c.keys.contains(&pos) as i32 - c.keys.contains(&neg) as i32) as f32
        };
        let ax = axis('a', 'd');
        let ay = axis('s', 'w');
        let az = axis('e', 'q');
        let active = c.pointer_over && (ax != 0.0 || ay != 0.0 || az != 0.0);
        if active {
            c.pan_accel_t = (c.pan_accel_t + dt).min(PAN_RAMP);
        } else {
            c.pan_accel_t = 0.0;
        }
        // Ease-out cubic: gentle start, steeper finish — the camera
        // "spools up" rather than ramping linearly.
        let pan_t = (c.pan_accel_t / PAN_RAMP).clamp(0.0, 1.0);
        let pan_eased = 1.0 - (1.0 - pan_t).powi(3);
        let speed = (PAN_BASE + (PAN_MAX - PAN_BASE) * pan_eased)
            * if c.shift { SHIFT_MUL } else { 1.0 };
        (
            [ax * dt * speed, ay * dt * speed, az * dt * speed],
            active,
        )
    });

    with_host(|h| {
        if dt_valid {
            h.pipes.camera.pan(pan[0], pan[1], pan[2]);
        }
        // Defensive: a NaN that sneaks into the camera basis (bad input
        // delta, degenerate zoom) would render as a permanently black
        // canvas with no error anywhere. Reset instead of going dark.
        let p = h.pipes.camera.position;
        if !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
            || !h.pipes.camera.yaw.is_finite()
            || !h.pipes.camera.pitch.is_finite()
        {
            tracing::warn!("[render] camera state went non-finite — resetting");
            let aspect = h.pipes.camera.aspect;
            h.pipes.camera = crate::render::camera::Camera::new(aspect);
        }
        let t0 = js_sys::Date::now();
        h.frame();
        perf_record(now, js_sys::Date::now() - t0);
    });
}

// --- frame-time / FPS instrumentation ------------------------------------------
//
// Port of the egui PerfCollector's role for the Debug panel's time-series
// charts: per-frame wall cost (encode+submit ms) and frame spacing (dt →
// FPS). True per-GPU-stage timing needs timestamp queries (not in the egui
// app either); its StageId instrumentation measured CPU-side phases, which
// `frame()` wall cost approximates for this single-pass renderer.

const PERF_CAP: usize = 240;

#[derive(Default)]
struct Perf {
    /// ms between rAF ticks (1000/dt = instantaneous FPS).
    frame_dt: std::collections::VecDeque<f32>,
    /// ms spent inside RenderHost::frame() (CPU encode + submit + present).
    frame_cost: std::collections::VecDeque<f32>,
    last_tick: f64,
}

thread_local! {
    static PERF: RefCell<Perf> = RefCell::new(Perf::default());
}

fn perf_record(tick_start_ms: f64, cost_ms: f64) {
    PERF.with(|p| {
        let mut p = p.borrow_mut();
        if p.last_tick > 0.0 {
            let dt = (tick_start_ms - p.last_tick) as f32;
            // Skip tab-suspend gaps; they'd flatten the chart's scale.
            if dt > 0.0 && dt < 500.0 {
                p.frame_dt.push_back(dt);
                if p.frame_dt.len() > PERF_CAP {
                    p.frame_dt.pop_front();
                }
            }
        }
        p.last_tick = tick_start_ms;
        p.frame_cost.push_back(cost_ms as f32);
        if p.frame_cost.len() > PERF_CAP {
            p.frame_cost.pop_front();
        }
    });
}

/// (frame_dt_ms, frame_cost_ms) histories, oldest first — Debug panel charts.
pub(crate) fn perf_series() -> (Vec<f32>, Vec<f32>) {
    PERF.with(|p| {
        let p = p.borrow();
        (p.frame_dt.iter().copied().collect(), p.frame_cost.iter().copied().collect())
    })
}

// --- input entry points (called from Dioxus event handlers) -------------------

/// Mouse-drag camera rotate. `dx`/`dy` are CSS-pixel deltas since the
/// previous mousemove. Sensitivity + response curve match the egui app.
pub fn pointer_rotate(dx: f32, dy: f32) {
    with_host(|h| {
        h.pipes.camera.rotate_yaw(apply_rotate_curve(dx) * 0.011);
        h.pipes.camera.rotate_pitch(-apply_rotate_curve(dy) * 0.011);
    });
}

/// Wheel zoom along the camera forward axis. `v` is the scroll amount in
/// the egui sign convention (positive = zoom in), i.e. `-deltaY` pixels.
/// Mixed linear+sqrt curve and distance-aware scaling match workspace.rs.
pub fn wheel_zoom(v: f32) {
    if v.abs() <= 0.5 {
        return;
    }
    let zoom = v.signum() * (v.abs() * 0.6 + v.abs().sqrt() * 26.0);
    with_host(|h| {
        // Distance-aware zoom: far out a fixed step barely moves the
        // view; close in it overshoots. |position| stands in for
        // distance-to-target (the camera orbits the origin-centred
        // cluster); the clamp keeps the formula stable near the origin.
        let dist = h.pipes.camera.position.length();
        let dist_scale = (dist / 1000.0).clamp(0.2, 5.0);
        h.pipes.camera.zoom(zoom * dist_scale);
    });
}

/// Convert canvas-local CSS-pixel coordinates to NDC ([-1,1], y-up).
fn to_ndc(x: f32, y: f32, w: f32, h: f32) -> [f32; 2] {
    let w = w.max(1.0);
    let h = h.max(1.0);
    [(x / w) * 2.0 - 1.0, -((y / h) * 2.0 - 1.0)]
}

/// Click pick: nearest node in screen space (same projection math the
/// shaders use). Returns the node index in buffer order.
pub fn pick(x: f32, y: f32) -> Option<u32> {
    with_host(|h| {
        let (w, hgt) = h.logical_size();
        let ndc = to_ndc(x, y, w, hgt);
        h.pipes.raycast(ndc[0], ndc[1], [w.max(1.0), hgt.max(1.0)])
    })
    .flatten()
}

/// Edge pick at canvas-local CSS-pixel coords. Mirrors the egui app's
/// `raycast_edge_idx` feed; 1.5 px is the default `edge_width` in
/// EffectsUniform (the same constant the old `update_hover` used).
pub fn pick_edge(x: f32, y: f32) -> Option<u32> {
    with_host(|h| {
        let (w, hgt) = h.logical_size();
        let ndc = to_ndc(x, y, w, hgt);
        h.pipes.raycast_edge(ndc, [w.max(1.0), hgt.max(1.0)], 1.5)
    })
    .flatten()
}

/// Hover feedback to the shaders (white node rim / brightened edge).
/// Pure GPU-state write — the picking, 50 ms throttle, and 250 ms
/// release-hold policy live in `crate::anchored`, which owns the hover
/// pipeline (port of the egui app's `update_hover_focus`).
pub fn set_hover_feedback(node: Option<u32>, edge: Option<u32>) {
    with_host(|h| {
        h.pipes.set_hovered_node(node);
        h.pipes.set_hovered_edge(edge);
    });
}

// --- anchored-card projection / focus-set accessors (phase 4) -----------------

/// Project node `idx`'s world position through the same proj*view the
/// renderer draws with (port of the egui `project_world_to_canvas`,
/// crates/graph-renderer/src/ui/anchored.rs at 723af10). Returns
/// canvas-local CSS-pixel coords plus whether the NDC point sits inside
/// the viewport; `None` = behind camera (`clip.w <= 0`), no host, or no
/// such node. The aspect comes from the live canvas size — the rAF tick
/// keeps `camera.aspect` in sync here (no egui one-frame-lag trap), but
/// deriving it from the same `logical_size` the raycast uses keeps the
/// two code paths trivially identical.
pub fn project_node(idx: u32) -> Option<(f32, f32, bool)> {
    with_host(|h| {
        let world = h.pipes.position_of(idx)?;
        if !world.is_finite() {
            return None;
        }
        let (w, hgt) = h.logical_size();
        let (w, hgt) = (w.max(1.0), hgt.max(1.0));
        let aspect = (w / hgt).max(0.0001);
        let cam = &h.pipes.camera;
        let view = glam::Mat4::look_to_rh(cam.position, cam.forward(), glam::Vec3::Y);
        let proj = glam::Mat4::perspective_rh(cam.fov_y, aspect, cam.znear, cam.zfar);
        let clip = (proj * view) * world.extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        // NDC y is up; CSS pixel y is down — flip on y.
        let sx = (ndc_x * 0.5 + 0.5) * w;
        let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * hgt;
        let on_screen = (-1.0..=1.0).contains(&ndc_x) && (-1.0..=1.0).contains(&ndc_y);
        Some((sx, sy, on_screen))
    })
    .flatten()
}

/// Canvas bounding rect in *viewport* CSS pixels: (left, top, width,
/// height). The anchored-card overlay renders `position:fixed` at the app
/// root, so canvas-local projected coords need this offset.
///
/// web-sys's typed `get_bounding_client_rect` needs the `DomRect` cargo
/// feature, which the workspace web-sys doesn't enable (and Cargo.toml is
/// frozen for this phase) — go through `js_sys::Reflect` instead.
pub fn canvas_rect() -> Option<(f32, f32, f32, f32)> {
    let el = canvas_el()?;
    let func = js_sys::Reflect::get(el.as_ref(), &JsValue::from_str("getBoundingClientRect")).ok()?;
    let func: js_sys::Function = func.dyn_into().ok()?;
    let rect = func.call0(el.as_ref()).ok()?;
    let get = |k: &str| {
        js_sys::Reflect::get(&rect, &JsValue::from_str(k))
            .ok()
            .and_then(|v| v.as_f64())
    };
    Some((
        get("left")? as f32,
        get("top")? as f32,
        get("width")? as f32,
        get("height")? as f32,
    ))
}

/// Push the per-node focus dim mask (hover/click community focus — the
/// `anchored` module's `apply_focus_set_to_gpu` arm). Coexists with the
/// filter panel's `sync_gpu` writes: both go through `set_focus_set`, and
/// `anchored` defers to `panels::filter::sync_gpu()` whenever no node is
/// focused, matching the egui dispatch.
pub fn push_focus_set(focused: Option<u32>, members: &HashSet<u32>) {
    with_host(|h| {
        let (pipes, queue) = h.pipes_and_queue();
        pipes.set_focus_set(queue, focused, members);
    });
}

/// Fly the camera to look at node `idx` (port of the egui
/// `focus_node_by_id` camera move, app.rs:794 at 723af10). Distance
/// scales with graph radius so a small graph doesn't fly way back and a
/// huge one still lets the node fill ~25% of the viewport.
pub fn look_at_node(idx: u32) {
    with_host(|h| {
        let Some(pos) = h.pipes.position_of(idx) else {
            return;
        };
        let distance = h
            .pipes
            .bounds()
            .map(|(mn, mx)| ((mx - mn) * 0.5).length().max(50.0) * 0.6)
            .unwrap_or(500.0);
        h.pipes.camera.look_at_point(pos, distance);
    });
}

pub fn set_pointer_over(over: bool) {
    CTL.with(|c| c.borrow_mut().pointer_over = over);
}

/// Selected node (from canvas click or the Nodes/Search lists) — drawn at
/// `SELECT_EMPHASIS ×` its base radius.
pub fn set_selected_node(idx: Option<u32>) {
    CTL.with(|c| c.borrow_mut().selected = idx);
    reapply_ctl_state();
}

/// Search-result highlight set: non-matching nodes dim to 0.18 alpha
/// (the same `set_selected` query-path visual the egui app uses).
/// `None` or an empty set restores full alpha everywhere.
pub fn set_search_highlights(set: Option<HashSet<u32>>) {
    let set = set.filter(|s| !s.is_empty());
    CTL.with(|c| c.borrow_mut().highlights = set);
    reapply_ctl_state();
}

/// Keyboard state from the workspace root. `key` is the browser's
/// `KeyboardEvent.key` value.
pub fn key_event(key: &str, down: bool) {
    CTL.with(|c| {
        let mut c = c.borrow_mut();
        match key {
            "Shift" => c.shift = down,
            k => {
                let mut chars = k.chars();
                if let (Some(ch), None) = (chars.next(), chars.next()) {
                    let ch = ch.to_ascii_lowercase();
                    if matches!(ch, 'w' | 'a' | 's' | 'd' | 'q' | 'e') {
                        if down {
                            c.keys.insert(ch);
                        } else {
                            c.keys.remove(&ch);
                        }
                    }
                }
            }
        }
    });
}

/// Drop all held keys — called when focus moves into a text field so a
/// missed keyup can't leave the camera flying.
pub fn clear_keys() {
    CTL.with(|c| {
        let mut c = c.borrow_mut();
        c.keys.clear();
        c.shift = false;
    });
}

/// F — fit camera to the live graph bounds.
pub fn fit_camera() {
    with_host(|h| h.pipes.fit_camera());
}

/// Play/pause for the in-process GPU force layout. Resuming also wakes a
/// halted sim so cooling restarts.
pub fn set_sim_running(running: bool) {
    CTL.with(|c| c.borrow_mut().sim_running = running);
    with_host(|h| h.pipes.set_sim_running(running));
}
