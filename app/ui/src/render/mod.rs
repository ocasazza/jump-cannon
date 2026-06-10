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

fn with_host<R>(f: impl FnOnce(&mut RenderHost) -> R) -> Option<R> {
    HOST.with(|h| h.borrow_mut().as_mut().map(f))
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
        h.frame();
    });
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

/// Hover feedback: node hover wins; otherwise edge hover; both flow into
/// the effects uniform (white rim / brightened edge in the shaders).
pub fn update_hover(x: f32, y: f32) {
    with_host(|h| {
        let (w, hgt) = h.logical_size();
        let screen = [w.max(1.0), hgt.max(1.0)];
        let ndc = to_ndc(x, y, w, hgt);
        let node = h.pipes.raycast(ndc[0], ndc[1], screen);
        h.pipes.set_hovered_node(node);
        let edge = if node.is_none() {
            // 1.5 = the default `edge_width` in EffectsUniform.
            h.pipes.raycast_edge(ndc, screen, 1.5)
        } else {
            None
        };
        h.pipes.set_hovered_edge(edge);
    });
}

pub fn clear_hover() {
    with_host(|h| {
        h.pipes.set_hovered_node(None);
        h.pipes.set_hovered_edge(None);
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
