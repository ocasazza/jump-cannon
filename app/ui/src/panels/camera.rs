//! Camera panel — Dioxus port of crates/graph-renderer/src/ui/sections/camera.rs.
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained. Renderer access goes
//! through `crate::render::with_host`.
//!
//! The egui app applies `follow_centroid` / `fit_to_window` once per frame
//! (`app.rs::apply_camera_to_gpu`); the Dioxus renderer's rAF tick lives in
//! `render/mod.rs` (read-only for this panel), so an equivalent 30 Hz loop
//! is spawned here on first render. The same loop re-stages the DoF effects
//! uniforms after a host rebuild (graph panel minimize → restore), which
//! `render::reapply_ctl_state` does not cover.
//!
//! Camera keybindings live at the workspace root (`main.rs::onkeydown`):
//! `F` fits to graph bounds, `C` toggles follow-centroid, `⇧C` snaps to the
//! centroid once at the current distance.

use std::cell::Cell;

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::render;
use crate::Ctx;

const STORE_KEY: &str = "jc_camera_v1";

// --- state (mirrors ui/state.rs::{CameraState, FocusState}) -------------------

// `CameraState` / `FocusState` are `pub(crate)` so `crate::appstate` can
// carry them as the round-trip `camera` / `focus` fields (the same two
// top-level fields the egui AppState persists).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CameraState {
    invert_mouse_x: bool,
    invert_mouse_y: bool,
    invert_ad: bool,
    invert_qe: bool,
    follow_centroid: bool,
    fit_to_window: bool,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            invert_mouse_x: false,
            invert_mouse_y: false,
            invert_ad: false,
            invert_qe: false,
            follow_centroid: true,
            fit_to_window: true,
        }
    }
}

/// Depth-of-field parameterization as one grouped struct — the first half
/// of the panel's camera-model plan (a future enum of camera types can then
/// carry a `DofParams` variant payload and swap parameter sets as a unit).
/// `#[serde(flatten)]` inside [`FocusState`] keeps the persisted wire shape
/// identical to the pre-grouping flat fields.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DofParams {
    /// Master DoF toggle. When false, the shader runs the sharp path
    /// for every node (no bokeh halo, no fragment-area inflation) —
    /// this is the cosmograph baseline. When true, the configured
    /// distance / thickness / blur / max_coc band engages.
    ///
    /// `rename` preserves the flat `dof_enabled` key written by every
    /// prior build's localStorage payload and AppState snapshot.
    #[serde(rename = "dof_enabled", default)]
    enabled: bool,
    distance: f32,
    thickness: f32,
    blur: f32,
    max_coc: f32,
}

impl Default for DofParams {
    fn default() -> Self {
        Self {
            enabled: false,
            distance: 100.0,
            thickness: 50.0,
            blur: 0.5,
            max_coc: 8.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FocusState {
    #[serde(flatten)]
    dof: DofParams,
    /// Membership criterion for hover/click focus dimming. See
    /// the egui app's `ui/focus_set.rs::FocusMode`.
    #[serde(default)]
    focus_mode: FocusMode,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            dof: DofParams::default(),
            focus_mode: FocusMode::default(),
        }
    }
}

/// Membership criterion for the focused community — verbatim port of the
/// egui app's `ui/focus_set.rs::FocusMode` (labels, order, enablement).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
enum FocusMode {
    None,
    #[default]
    SameCommunityId,
    SharedEdge,
    SharedTag,
    Filter,
}

impl FocusMode {
    const ALL: &'static [FocusMode] = &[
        FocusMode::None,
        FocusMode::SameCommunityId,
        FocusMode::SharedEdge,
        FocusMode::SharedTag,
        FocusMode::Filter,
    ];

    fn label(self) -> &'static str {
        match self {
            FocusMode::None => "None (single node)",
            FocusMode::SameCommunityId => "Same community id",
            FocusMode::SharedEdge => "Shared edge",
            FocusMode::SharedTag => "Shared tag",
            FocusMode::Filter => "Active filter",
        }
    }

    /// All modes enabled in the egui app since the field_index
    /// plumb-through landed; kept so the disabled-option + tooltip path
    /// stays in place if a mode regresses to stub.
    fn enabled(self) -> bool {
        true
    }
}

#[derive(Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    camera: CameraState,
    #[serde(default)]
    focus: FocusState,
}

static STATE: GlobalSignal<Persisted> =
    Signal::global(|| LocalStorage::get(STORE_KEY).unwrap_or_default());

// Plain mirrors for the spawn_local loop below — `GlobalSignal` reads
// require a Dioxus runtime context, which a detached future doesn't have.
thread_local! {
    static INIT: Cell<bool> = const { Cell::new(false) };
    static FOLLOW_FIT: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static FOCUS_MIRROR: Cell<FocusState> = Cell::new(FocusState::default());
    /// Last (plane_z, thickness, blur, max_coc) staged to the GPU. The
    /// plane is captured at settings-change time (absolute world-z focal
    /// plane — egui's `apply_focus_to_gpu` semantics), so re-staging it
    /// every tick is idempotent and only exists to survive host rebuilds.
    static FOCUS_GPU: Cell<Option<[f32; 4]>> = const { Cell::new(None) };
}

fn update(mutate: impl FnOnce(&mut Persisted)) {
    // Attribute the auto-snapshot like the egui section, which stamps
    // `snapshot_source = Some("Camera")` every frame it renders.
    crate::appstate::note_source("Camera");
    let snap = {
        let mut s = STATE.write();
        mutate(&mut s);
        *s
    };
    sync(&snap);
}

/// AppState round-trip seam (`crate::appstate`): the live camera + focus
/// states (egui's two top-level AppState fields).
pub(crate) fn state_snapshot() -> (CameraState, FocusState) {
    let s = *STATE.read();
    (s.camera, s.focus)
}

/// AppState round-trip seam: write imported camera + focus straight to
/// localStorage; the apply path's reload re-seeds [`STATE`].
pub(crate) fn state_restore(camera: &CameraState, focus: &FocusState) {
    let _ = LocalStorage::set(
        STORE_KEY,
        &Persisted {
            camera: *camera,
            focus: *focus,
        },
    );
}

fn sync(s: &Persisted) {
    let _ = LocalStorage::set(STORE_KEY, s);
    FOLLOW_FIT.with(|c| c.set((s.camera.follow_centroid, s.camera.fit_to_window)));
    FOCUS_MIRROR.with(|c| c.set(s.focus));
    push_focus(&s.focus.dof);
}

/// Mirror of the egui app's `apply_focus_to_gpu`: plane_z is derived from
/// the camera's z at change time; DoF off pushes a sentinel thickness so
/// node.wgsl's `focus_thickness < 1e6` gate stays false for every node
/// (sharp fragment path, no bokeh quad inflation).
fn push_focus(f: &DofParams) {
    let f = *f;
    render::with_host(|h| {
        let plane_z = h.pipes.camera.position.z - f.distance;
        let thickness = if f.enabled { f.thickness } else { 1.0e9 };
        h.pipes.set_focus_plane(plane_z, thickness);
        h.pipes.set_dof_params(f.blur, f.max_coc);
        FOCUS_GPU.with(|c| c.set(Some([plane_z, thickness, f.blur, f.max_coc])));
    });
}

/// Look-toward the centroid: keep the camera's current distance along
/// forward, retarget (`app.rs::apply_camera_to_gpu`). `look_at_point`
/// floors at `znear * 2` instead of the egui port's original 50-unit
/// clamp — that clamp blocked zooming in past 50 units of the centroid
/// for as long as follow-centroid stayed on.
fn retarget_to_centroid() {
    render::with_host(|h| {
        if let Some(c) = h.pipes.centroid() {
            let dist = (c - h.pipes.camera.position).length();
            h.pipes.camera.look_at_point(c, dist);
        }
    });
}

/// `C` — toggle follow-centroid, the persistent keep-centered mode.
/// Routed from the workspace root key handler (main.rs).
pub(crate) fn toggle_follow_centroid() {
    let next = !STATE.read().camera.follow_centroid;
    update(|s| s.camera.follow_centroid = next);
}

/// `⇧C` — one-shot recenter on the graph centroid at the camera's
/// current distance; does NOT engage follow mode. Routed from the
/// workspace root key handler (main.rs).
pub(crate) fn snap_to_center() {
    retarget_to_centroid();
}

/// `pub(crate)`: `appstate::ensure_init` arms this loop from the FIRST
/// panel that renders (Nodes is open in the default layout), so
/// follow-centroid / fit-to-window run from effective app start like the
/// egui update loop — not only once the Camera panel itself first opens.
pub(crate) fn ensure_init() {
    if INIT.with(|c| c.replace(true)) {
        return;
    }
    let s = *STATE.read();
    sync(&s);
    // Push the persisted focus mode into the hover/click focus engine
    // (anchored.rs) — variant order is identical by construction.
    if let Some(i) = FocusMode::ALL.iter().position(|m| *m == s.focus.focus_mode) {
        crate::anchored::set_focus_mode(crate::anchored::FocusMode::ALL[i]);
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut last_fit_screen: Option<(f64, f64)> = None;
        loop {
            let (follow, fit) = FOLLOW_FIT.with(Cell::get);
            if follow {
                retarget_to_centroid();
            }
            if fit {
                // Auto-refit ONLY on actual window resize — the egui app
                // watches its full screen_rect (not the canvas rect) so a
                // panel open/close can't bounce the camera. window inner
                // size is the webview analog.
                let size = web_sys::window().map(|w| {
                    (
                        w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(0.0),
                        w.inner_height()
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                    )
                });
                if let Some(size) = size {
                    let changed = match last_fit_screen {
                        None => false, // initial fit handled at graph load; skip first tick
                        Some(prev) => (prev.0 - size.0).abs().max((prev.1 - size.1).abs()) > 1.0,
                    };
                    if changed {
                        render::with_host(|h| h.pipes.fit_camera());
                    }
                    last_fit_screen = Some(size);
                }
            } else {
                last_fit_screen = None;
            }
            match FOCUS_GPU.with(Cell::get) {
                // Re-stage cached DoF uniforms — survives host rebuilds.
                Some([z, t, b, m]) => {
                    render::with_host(|h| {
                        h.pipes.set_focus_plane(z, t);
                        h.pipes.set_dof_params(b, m);
                    });
                }
                // Settings restored before the host existed — compute the
                // plane as soon as a host shows up.
                None => push_focus(&FOCUS_MIRROR.with(Cell::get).dof),
            }
            gloo_timers::future::TimeoutFuture::new(33).await;
        }
    });
}

// --- row widgets (HTML analogs of ui/widgets.rs::{row, subgroup_label, …}) ----

fn check_row(
    label: &'static str,
    accent_on: bool,
    checked: bool,
    on: impl FnMut(bool) + 'static,
) -> Element {
    let mut on = on;
    rsx! {
        div { class: "cam-row",
            span { class: if accent_on { "cam-label accent" } else { "cam-label" }, "{label}" }
            input {
                r#type: "checkbox",
                checked,
                onchange: move |e| on(e.checked()),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn slider_row(
    label: &'static str,
    min: f64,
    max: f64,
    step: f64,
    decimals: usize,
    value: f32,
    disabled: bool,
    on: impl FnMut(f32) + 'static,
) -> Element {
    let mut on = on;
    rsx! {
        div { class: "cam-row",
            span { class: "cam-label", "{label}" }
            input {
                r#type: "range",
                min: "{min}",
                max: "{max}",
                step: "{step}",
                value: "{value}",
                disabled,
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on(v);
                    }
                },
            }
            span { class: "cam-val", { format!("{:.*}", decimals, value) } }
        }
    }
}

// --- panel ---------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    ensure_init();
    crate::appstate::ensure_init();
    let s = *STATE.read();
    let c = s.camera;
    let f = s.focus;
    let mode = f.focus_mode;

    rsx! {
        div { class: "cam",
            div { class: "cam-reset-row",
                button { class: "btn cam-small",
                    onclick: move |_| update(|s| s.camera = CameraState::default()),
                    "↺ Reset"
                }
            }
            div { class: "cam-actions",
                // Inert placeholders in the egui source (`let _ = ui.button(…)`);
                // wired here per the migration contract's renderer port.
                button { class: "btn", onclick: move |_| render::fit_camera(), "Fit" }
                button { class: "btn",
                    onclick: move |_| { render::with_host(|h| h.pipes.camera.reset()); },
                    "Reset"
                }
            }

            hr { class: "cam-sep" }

            // PARITY GAP: invert mouse X/Y are consumed by the egui input
            // path (workspace.rs flips the rotate deltas); the Dioxus
            // rotate path (render::pointer_rotate) has fixed signs and
            // render/ is read-only here — state + control only.
            {check_row("Invert mouse X", false, c.invert_mouse_x,
                move |v| update(|s| s.camera.invert_mouse_x = v))}
            {check_row("Invert mouse Y", false, c.invert_mouse_y,
                move |v| update(|s| s.camera.invert_mouse_y = v))}
            // Invert A/D and Q/E are stored-but-unconsumed in the egui app
            // too (no input-path reader) — identical fidelity here.
            {check_row("Invert A/D", false, c.invert_ad,
                move |v| update(|s| s.camera.invert_ad = v))}
            {check_row("Invert Q/E", false, c.invert_qe,
                move |v| update(|s| s.camera.invert_qe = v))}

            div { class: "cam-space" }

            // Follow centroid: blue tint on the row label when active.
            {check_row("Follow centroid", c.follow_centroid, c.follow_centroid,
                move |v| update(|s| s.camera.follow_centroid = v))}
            {check_row("Fit to window", false, c.fit_to_window,
                move |v| update(|s| s.camera.fit_to_window = v))}
            div { class: "cam-hint",
                "Keys: F fit to bounds · C toggle follow centroid · ⇧C snap to center."
            }

            // ---- Focus subgroup (merged from former Section::Focus) ---------
            hr { class: "cam-sep" }
            div { class: "cam-sub", "Focus" }
            div { class: "cam-row",
                span { class: "cam-label", "Focus mode" }
                select { class: "cam-select",
                    onchange: move |e| {
                        if let Ok(i) = e.value().parse::<usize>() {
                            if let Some(&m) = FocusMode::ALL.get(i) {
                                if m.enabled() {
                                    update(|s| s.focus.focus_mode = m);
                                    // Same variant order on both enums.
                                    crate::anchored::set_focus_mode(
                                        crate::anchored::FocusMode::ALL[i],
                                    );
                                }
                            }
                        }
                    },
                    for (i, m) in FocusMode::ALL.iter().enumerate() {
                        option {
                            value: "{i}",
                            selected: *m == mode,
                            disabled: !m.enabled(),
                            title: if m.enabled() { "" } else { "(needs vault meta cache)" },
                            "{m.label()}"
                        }
                    }
                }
            }
            div { class: "cam-hint",
                "Hover or click a node → that node + its community light up; \
                 everything else dims. Click empty canvas to clear."
            }

            hr { class: "cam-sep" }

            // ---- DoF subgroup -----------------------------------------------
            // Parameterized as the grouped `DofParams` struct; the longer-
            // term camera-modes plan (typed camera models, computational
            // cameras beyond simple DoF, known bokeh-quality defects) lives
            // in docs/research/computational-cameras.md.
            div { class: "cam-sub", "Depth of field" }
            {check_row("Enabled", false, f.dof.enabled,
                move |v| update(|s| s.focus.dof.enabled = v))}
            {slider_row("distance", 0.0, 1000.0, 1.0, 0, f.dof.distance, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.distance = v))}
            {slider_row("thickness", 1.0, 500.0, 1.0, 0, f.dof.thickness, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.thickness = v))}
            {slider_row("blur", 0.0, 4.0, 0.01, 2, f.dof.blur, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.blur = v))}
            {slider_row("max CoC", 0.0, 32.0, 0.1, 1, f.dof.max_coc, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.max_coc = v))}
            div { class: "cam-hint",
                "The sharp band sits `distance` ahead of the camera with \
                 `thickness` depth; `blur` and `max CoC` shape the \
                 out-of-focus halo on nodes outside the band."
            }
        }
    }
}
