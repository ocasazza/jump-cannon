//! Camera panel — projection model, navigation, focus, and the camera
//! effects stack (depth of field, attribute focus, depth cueing, clipping
//! slab, saved views).
//!
//! Panel-local state lives in `GlobalSignal`s inside this module (not on
//! `crate::Ctx`) so each panel file is self-contained. Renderer access goes
//! through `crate::render::with_host`.
//!
//! A 30 Hz loop (spawned by [`ensure_init`]) applies follow-centroid /
//! fit-to-window like the egui app's per-frame `apply_camera_to_gpu`, and
//! re-stages every camera-effects setting from thread-local mirrors each
//! tick — uniform writes are idempotent, and the re-stage is what survives
//! a host rebuild (graph panel minimize → restore), which
//! `render::reapply_ctl_state` does not cover.
//!
//! Camera keybindings live at the workspace root (`main.rs::onkeydown`):
//! `F` fits to graph bounds, `C` toggles follow-centroid, `⇧C` snaps to the
//! centroid once at the current distance.

use std::cell::{Cell, RefCell};

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::render;
use crate::render::camera::Projection;
use crate::Ctx;

const STORE_KEY: &str = "jc_camera_v1";
const VIEWS_KEY: &str = "jc_camera_views_v1";

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
    /// Projection model toggle. `fov_y` applies in perspective mode;
    /// switching to orthographic fits the graph to seed `half_height`.
    #[serde(default)]
    orthographic: bool,
    #[serde(default = "default_fov_y")]
    fov_y: f32,
    #[serde(default)]
    fog: FogParams,
    #[serde(default)]
    clip: ClipParams,
}

fn default_fov_y() -> f32 {
    Projection::DEFAULT_FOV_Y
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
            orthographic: false,
            fov_y: default_fov_y(),
            fog: FogParams::default(),
            clip: ClipParams::default(),
        }
    }
}

/// Depth cueing: contrast + alpha attenuate with view-space distance.
/// PyMOL-style fog — the cheap depth-legibility channel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FogParams {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_fog_start")]
    start: f32,
    #[serde(default = "default_fog_end")]
    end: f32,
    #[serde(default = "default_fog_strength")]
    strength: f32,
}

fn default_fog_start() -> f32 {
    1500.0
}
fn default_fog_end() -> f32 {
    6000.0
}
fn default_fog_strength() -> f32 {
    0.7
}

impl Default for FogParams {
    fn default() -> Self {
        Self {
            enabled: false,
            start: default_fog_start(),
            end: default_fog_end(),
            strength: default_fog_strength(),
        }
    }
}

/// Clipping slab — a section view: only nodes whose view-space depth
/// falls inside [near, far] rasterize.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClipParams {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_clip_near")]
    near: f32,
    #[serde(default = "default_clip_far")]
    far: f32,
}

fn default_clip_near() -> f32 {
    1.0
}
fn default_clip_far() -> f32 {
    50_000.0
}

impl Default for ClipParams {
    fn default() -> Self {
        Self {
            enabled: false,
            near: default_clip_near(),
            far: default_clip_far(),
        }
    }
}

/// Depth-of-field parameterization, grouped per the panel's camera-model
/// plan. All distances are VIEW-SPACE (v2: the v1 focal plane was an
/// absolute world Z — broken for any rotated/panned camera).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DofParams {
    /// Master DoF toggle (drives FLAG_DOF — the v1 sentinel-thickness
    /// path is gone).
    ///
    /// `rename` preserves the flat `dof_enabled` key written by every
    /// prior build's localStorage payload and AppState snapshot.
    #[serde(rename = "dof_enabled", default)]
    enabled: bool,
    /// Focal distance from the camera (view-space).
    distance: f32,
    /// Full width of the sharp band (view-space units).
    thickness: f32,
    /// CoC scale — pixels of blur per unit of relative depth error.
    /// `rename` preserves the flat `blur` key from v1 payloads.
    #[serde(rename = "blur", default = "default_aperture")]
    aperture: f32,
    max_coc: f32,
}

fn default_aperture() -> f32 {
    0.5
}

impl Default for DofParams {
    fn default() -> Self {
        Self {
            enabled: false,
            distance: 100.0,
            thickness: 50.0,
            aperture: default_aperture(),
            max_coc: 8.0,
        }
    }
}

/// Attribute source for the defocus-as-data channel: the focal band
/// reads a per-node attribute instead of view depth, so blur encodes
/// distance from the chosen attribute center.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) enum AttrSource {
    #[default]
    None,
    Degree,
    Size,
}

impl AttrSource {
    const ALL: &'static [AttrSource] = &[AttrSource::None, AttrSource::Degree, AttrSource::Size];

    fn label(self) -> &'static str {
        match self {
            AttrSource::None => "Off (depth-driven)",
            AttrSource::Degree => "Node degree",
            AttrSource::Size => "Node size",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct AttrFocus {
    #[serde(default)]
    source: AttrSource,
    #[serde(default = "default_attr_center")]
    center: f32,
}

fn default_attr_center() -> f32 {
    0.5
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FocusState {
    #[serde(flatten)]
    dof: DofParams,
    /// Membership criterion for hover/click focus dimming. See
    /// the egui app's `ui/focus_set.rs::FocusMode`.
    #[serde(default)]
    focus_mode: FocusMode,
    #[serde(default)]
    attr: AttrFocus,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            dof: DofParams::default(),
            focus_mode: FocusMode::default(),
            attr: AttrFocus::default(),
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

/// One named camera view — the `get_view`/`set_view` analog: position,
/// orientation, and projection as restorable data.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct SavedView {
    name: String,
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    /// `Some(half_height)` = orthographic view; `None` = perspective.
    ortho_half_height: Option<f32>,
    fov_y: f32,
}

static VIEWS: GlobalSignal<Vec<SavedView>> =
    Signal::global(|| LocalStorage::get(VIEWS_KEY).unwrap_or_default());

/// Full camera-effects mirror for the re-stage loop — everything the
/// panel pushes to the GPU, in one Copy struct.
#[derive(Clone, Copy)]
struct EffectsMirror {
    dof: DofParams,
    fog: FogParams,
    clip: ClipParams,
    attr: AttrFocus,
}

// Plain mirrors for the spawn_local loop below — `GlobalSignal` reads
// require a Dioxus runtime context, which a detached future doesn't have.
thread_local! {
    static INIT: Cell<bool> = const { Cell::new(false) };
    static FOLLOW_FIT: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static EFFECTS: RefCell<EffectsMirror> = RefCell::new(EffectsMirror {
        dof: DofParams::default(),
        fog: FogParams::default(),
        clip: ClipParams::default(),
        attr: AttrFocus::default(),
    });
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
    EFFECTS.with(|c| {
        c.replace(EffectsMirror {
            dof: s.focus.dof,
            fog: s.camera.fog,
            clip: s.camera.clip,
            attr: s.focus.attr,
        })
    });
    push_projection(&s.camera);
    push_effects(&EFFECTS.with(|c| *c.borrow()));
}

/// Apply the persisted projection model to the live camera. Switching to
/// orthographic fits the graph to seed a sane `half_height`; perspective
/// takes the persisted fov.
fn push_projection(c: &CameraState) {
    let c = *c;
    render::with_host(|h| {
        let cur = h.pipes.camera.projection;
        let next = if c.orthographic {
            match cur {
                // Already ortho — keep the user's zoomed half_height.
                Projection::Orthographic { .. } => cur,
                Projection::Perspective { .. } => Projection::Orthographic { half_height: 1000.0 },
            }
        } else {
            Projection::Perspective { fov_y: c.fov_y }
        };
        if next != cur {
            let switching_to_ortho = matches!(next, Projection::Orthographic { .. })
                && !matches!(cur, Projection::Orthographic { .. });
            h.pipes.camera.projection = next;
            if switching_to_ortho {
                h.pipes.fit_camera();
            }
        }
    });
}

/// Push the full camera-effects mirror to the GPU. Idempotent — the 30 Hz
/// loop calls this every tick so settings survive host rebuilds.
fn push_effects(m: &EffectsMirror) {
    let m = *m;
    render::with_host(|h| {
        let (pipes, queue) = h.pipes_and_queue();
        pipes.set_focus_band(m.dof.distance, m.dof.thickness);
        pipes.set_dof(m.dof.aperture, m.dof.max_coc);
        pipes.set_dof_enabled(m.dof.enabled);
        pipes.set_fog(m.fog.start, m.fog.end, m.fog.strength, m.fog.enabled);
        pipes.set_clip(m.clip.near, m.clip.far, m.clip.enabled);
        let attr_on = m.attr.source != AttrSource::None;
        pipes.set_attr_focus(m.attr.center, attr_on);
        if attr_on {
            // Stream the per-node attribute, min-max normalized to [0,1].
            // Re-streamed every tick: the buffer dies with the host on a
            // rebuild, and a queue write of a few hundred KB is cheap
            // against the per-frame uniform uploads.
            let raw = match m.attr.source {
                AttrSource::Degree => pipes.degrees(),
                AttrSource::Size => pipes.node_sizes(),
                AttrSource::None => None,
            };
            if let Some(raw) = raw {
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                for v in &raw {
                    lo = lo.min(*v);
                    hi = hi.max(*v);
                }
                let span = (hi - lo).max(1e-6);
                let norm: Vec<f32> = raw.iter().map(|v| (v - lo) / span).collect();
                pipes.set_focus_attr(queue, norm);
            }
        }
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
            // Re-stage the full camera-effects mirror every tick —
            // idempotent, and survives host rebuilds.
            push_effects(&EFFECTS.with(|c| *c.borrow()));
            gloo_timers::future::TimeoutFuture::new(33).await;
        }
    });
}

// --- saved views (get_view / set_view analog) --------------------------------

fn persist_views(views: &[SavedView]) {
    let _ = LocalStorage::set(VIEWS_KEY, views);
}

fn save_current_view(name: String) {
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    let view = render::with_host(|h| {
        let c = &h.pipes.camera;
        let (ortho_half_height, fov_y) = match c.projection {
            Projection::Orthographic { half_height } => (Some(half_height), c.fov_y()),
            Projection::Perspective { fov_y } => (None, fov_y),
        };
        SavedView {
            name: name.clone(),
            position: c.position.to_array(),
            yaw: c.yaw,
            pitch: c.pitch,
            ortho_half_height,
            fov_y,
        }
    });
    let mut views = VIEWS.read().clone();
    match &view {
        Some(v) => {
            // Same-name views replace — a named view is a stable bookmark.
            views.retain(|x| x.name != v.name);
            views.push(v.clone());
        }
        None => return,
    }
    persist_views(&views);
    *VIEWS.write() = views;
}

fn apply_view(v: &SavedView) {
    let v = v.clone();
    render::with_host(|h| {
        h.pipes.camera.position = glam::Vec3::from_array(v.position);
        h.pipes.camera.yaw = v.yaw;
        h.pipes.camera.pitch = v.pitch;
        h.pipes.camera.projection = match v.ortho_half_height {
            Some(half_height) => Projection::Orthographic { half_height },
            None => Projection::Perspective { fov_y: v.fov_y },
        };
    });
    // Keep the panel's persisted projection in step with the applied view.
    update(|s| {
        s.camera.orthographic = v.ortho_half_height.is_some();
        if v.ortho_half_height.is_none() {
            s.camera.fov_y = v.fov_y;
        }
    });
}

fn delete_view(index: usize) {
    let mut views = VIEWS.read().clone();
    if index < views.len() {
        views.remove(index);
        persist_views(&views);
        *VIEWS.write() = views;
    }
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
    let mut view_name = use_signal(String::new);
    let views = VIEWS.read().clone();

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

            // ---- Projection subgroup ----------------------------------------
            div { class: "cam-sub", "Projection" }
            div { class: "cam-row",
                span { class: "cam-label", "Model" }
                select { class: "cam-select",
                    onchange: move |e| {
                        update(|s| s.camera.orthographic = e.value() == "1");
                    },
                    option { value: "0", selected: !c.orthographic, "Perspective" }
                    option { value: "1", selected: c.orthographic, "Orthographic" }
                }
            }
            if !c.orthographic {
                {slider_row("field of view°", 20.0, 120.0, 1.0, 0,
                    c.fov_y.to_degrees(), false,
                    move |v| update(|s| s.camera.fov_y = v.to_radians()))}
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
            // View-space focal band + perspective-correct CoC. The longer-
            // term camera-modes plan lives in
            // docs/research/computational-cameras.md.
            div { class: "cam-sub", "Depth of field" }
            {check_row("Enabled", false, f.dof.enabled,
                move |v| update(|s| s.focus.dof.enabled = v))}
            {slider_row("distance", 0.0, 2000.0, 1.0, 0, f.dof.distance, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.distance = v))}
            {slider_row("thickness", 1.0, 500.0, 1.0, 0, f.dof.thickness, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.thickness = v))}
            {slider_row("aperture", 0.0, 4.0, 0.01, 2, f.dof.aperture, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.aperture = v))}
            {slider_row("max CoC", 0.0, 32.0, 0.1, 1, f.dof.max_coc, !f.dof.enabled,
                move |v| update(|s| s.focus.dof.max_coc = v))}
            div { class: "cam-hint",
                "The sharp band sits `distance` ahead of the camera with \
                 `thickness` depth; `aperture` and `max CoC` shape the \
                 out-of-focus halo on nodes outside the band."
            }

            // ---- Attribute focus subgroup ------------------------------------
            hr { class: "cam-sep" }
            div { class: "cam-sub", "Attribute focus" }
            div { class: "cam-row",
                span { class: "cam-label", "Source" }
                select { class: "cam-select",
                    onchange: move |e| {
                        if let Ok(i) = e.value().parse::<usize>() {
                            if let Some(&a) = AttrSource::ALL.get(i) {
                                update(|s| s.focus.attr.source = a);
                            }
                        }
                    },
                    for (i, a) in AttrSource::ALL.iter().enumerate() {
                        option {
                            value: "{i}",
                            selected: *a == f.attr.source,
                            "{a.label()}"
                        }
                    }
                }
            }
            {slider_row("center", 0.0, 1.0, 0.01, 2, f.attr.center,
                f.attr.source == AttrSource::None,
                move |v| update(|s| s.focus.attr.center = v))}
            div { class: "cam-hint",
                "Blur encodes distance from the attribute center instead of \
                 depth — defocus as a data channel. Requires DoF enabled."
            }

            // ---- Depth cueing subgroup ---------------------------------------
            hr { class: "cam-sep" }
            div { class: "cam-sub", "Depth cueing" }
            {check_row("Enabled", false, c.fog.enabled,
                move |v| update(|s| s.camera.fog.enabled = v))}
            {slider_row("start", 0.0, 10000.0, 10.0, 0, c.fog.start, !c.fog.enabled,
                move |v| update(|s| s.camera.fog.start = v))}
            {slider_row("end", 100.0, 20000.0, 10.0, 0, c.fog.end, !c.fog.enabled,
                move |v| update(|s| s.camera.fog.end = v))}
            {slider_row("strength", 0.0, 1.0, 0.01, 2, c.fog.strength, !c.fog.enabled,
                move |v| update(|s| s.camera.fog.strength = v))}

            // ---- Clipping slab subgroup --------------------------------------
            hr { class: "cam-sep" }
            div { class: "cam-sub", "Clipping slab" }
            {check_row("Enabled", false, c.clip.enabled,
                move |v| update(|s| s.camera.clip.enabled = v))}
            {slider_row("near", 0.0, 10000.0, 1.0, 0, c.clip.near, !c.clip.enabled,
                move |v| update(|s| s.camera.clip.near = v))}
            {slider_row("far", 100.0, 50000.0, 10.0, 0, c.clip.far, !c.clip.enabled,
                move |v| update(|s| s.camera.clip.far = v))}
            div { class: "cam-hint",
                "Section view: only nodes between `near` and `far` (view-space \
                 depth) rasterize; edges are cut exactly at the slab."
            }

            // ---- Saved views subgroup ----------------------------------------
            hr { class: "cam-sep" }
            div { class: "cam-sub", "Saved views" }
            div { class: "cam-row",
                input {
                    class: "cam-select",
                    r#type: "text",
                    placeholder: "view name",
                    value: "{view_name}",
                    oninput: move |e| view_name.set(e.value()),
                }
                button { class: "btn cam-small",
                    onclick: move |_| {
                        save_current_view(view_name.read().clone());
                        view_name.set(String::new());
                    },
                    "Save"
                }
            }
            for (i, v) in views.clone().into_iter().enumerate() {
                div { class: "cam-row",
                    span { class: "cam-label", "{v.name}" }
                    button { class: "btn cam-small",
                        onclick: move |_| apply_view(&v),
                        "Apply"
                    }
                    button { class: "btn cam-small",
                        onclick: move |_| delete_view(i),
                        "✕"
                    }
                }
            }
            if views.is_empty() {
                div { class: "cam-hint",
                    "Bookmark the current camera (position, angle, projection) \
                     and restore it later — views persist across sessions."
                }
            }
        }
    }
}
