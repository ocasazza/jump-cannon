//! Tabbed / dockable workspace surface for the central panel.
//!
//! Replaces a single `CentralPanel { wgpu callback }` with an
//! `egui_dock::DockArea` so the graph view becomes one tab among many.
//! Splits, drag-to-reorder, and a per-pane "+" add-tab button are
//! provided by `egui_dock`.
//!
//! The graph tab itself reproduces the wgpu callback flow exactly the
//! way the pre-dock central panel did. All mutable bits the graph tab
//! needs (frame, app input trackers, click capture) live on
//! `WorkspaceCtx` which the TabViewer borrows for one frame.

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::data::LoadState;
use crate::graph_callback::GraphCallback;
use crate::graph_pipelines::GraphPipelines;
use crate::ui::input::AppAction;

/// What lives in a tab. New tab kinds (Stats, NodeInspector, QueryEditor,
/// Logs, …) plug in here and pick up the tab strip / split UI for free.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TabKind {
    Graph,
    Welcome,
}

impl TabKind {
    pub const ALL: &'static [TabKind] = &[TabKind::Graph, TabKind::Welcome];

    pub fn default_title(&self) -> &'static str {
        match self {
            TabKind::Graph => "Graph",
            TabKind::Welcome => "Welcome",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub kind: TabKind,
    pub title: String,
}

impl Tab {
    pub fn new(kind: TabKind) -> Self {
        let title = kind.default_title().to_string();
        Self { kind, title }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub dock_state: egui_dock::DockState<Tab>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            dock_state: egui_dock::DockState::new(vec![Tab::new(TabKind::Graph)]),
        }
    }

    /// Append a tab of the given kind into the focused (or main) leaf.
    pub fn push_tab(&mut self, kind: TabKind) {
        self.dock_state.push_to_focused_leaf(Tab::new(kind));
    }

    /// True when the dock has more than one tab open. Used by
    /// `AppState::pop_canvas_out` to snapshot whether the dock tab
    /// strip was visible at the moment of pop-out (the renderer
    /// collapses the strip to zero height when only one tab exists).
    pub fn has_multiple_tabs(&self) -> bool {
        self.dock_state.iter_all_tabs().count() > 1
    }
}

/// Per-frame context passed into the TabViewer. Carries everything the
/// graph tab needs to reproduce the original `CentralPanel` flow:
/// the loaded-into-gpu flag, a load-status snapshot for the placeholder
/// label, and out-params for the input handlers in `App::update`.
pub struct WorkspaceCtx<'a> {
    pub frame: &'a mut eframe::Frame,
    pub loaded_into_gpu: bool,
    pub load_msg: Option<&'a str>,
    pub invert_mouse_x: bool,
    pub invert_mouse_y: bool,

    /// Persistent WASD ease-in timer (seconds of continuous pan input).
    /// Owned by `App` so it survives across frames; threaded through the
    /// per-frame ctx so the tab handler can ramp pan speed.
    pub pan_accel_t: &'a mut f32,

    /// Semantic input events (jump-io) for camera pan / rotate / zoom.
    /// Pulses (palette / cancel / fit) are already consumed in App;
    /// this slice carries only `Axis1` / `Axis2` events relevant to
    /// the canvas. Pointer-in-canvas gating happens in the consumer.
    pub input_events: &'a [jump_io::Event<crate::ui::input::AppAction>],

    /// When `Some`, a non-canvas panel logically owns scroll input —
    /// wheel-zoom events are not consumed by the canvas. Plumbed from
    /// `AppState::focused_panel` so the gating logic in `draw_graph_tab`
    /// matches the rest of the UI's notion of focus. `None` means the
    /// canvas is the focused surface and zoom routes here as normal.
    pub focused_panel: Option<crate::ui::state::FocusedPanel>,

    // Out: filled by the graph tab when it runs.
    pub canvas_rect: Option<egui::Rect>,
    pub pointer_in_canvas: Option<egui::Pos2>,
    pub click: Option<(egui::Rect, egui::Pos2)>,
    pub lmb_held: bool,
    pub rmb_held: bool,

    // Out: requests from the Welcome tab and "+ menu" / context menu.
    pub add_tab_requests: Vec<TabKind>,
    pub split_requests: Vec<SplitRequest>,
}

pub struct SplitRequest {
    pub surface: egui_dock::SurfaceIndex,
    pub node: egui_dock::NodeIndex,
    pub split: egui_dock::Split,
    pub new_tab: TabKind,
}

pub struct WorkspaceViewer<'a, 'ctx> {
    pub ctx: &'a mut WorkspaceCtx<'ctx>,
}

impl<'a, 'ctx> egui_dock::TabViewer for WorkspaceViewer<'a, 'ctx> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title.clone().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab.kind {
            TabKind::Graph => self.draw_graph_tab(ui),
            TabKind::Welcome => self.draw_welcome_tab(ui),
        }
    }

    fn on_add(&mut self, _surface: egui_dock::SurfaceIndex, _node: egui_dock::NodeIndex) {
        // The popup below already fires `add_tab_requests`; on_add alone
        // (without the popup) defaults to Graph for ergonomics.
        self.ctx.add_tab_requests.push(TabKind::Graph);
    }

    fn add_popup(
        &mut self,
        ui: &mut egui::Ui,
        _surface: egui_dock::SurfaceIndex,
        _node: egui_dock::NodeIndex,
    ) {
        ui.set_min_width(120.0);
        for kind in TabKind::ALL {
            if ui.button(kind.default_title()).clicked() {
                self.ctx.add_tab_requests.push(kind.clone());
                ui.close_menu();
            }
        }
    }

    fn context_menu(
        &mut self,
        ui: &mut egui::Ui,
        tab: &mut Tab,
        surface: egui_dock::SurfaceIndex,
        node: egui_dock::NodeIndex,
    ) {
        ui.set_min_width(140.0);
        if ui.button("Split Right").clicked() {
            self.ctx.split_requests.push(SplitRequest {
                surface,
                node,
                split: egui_dock::Split::Right,
                new_tab: tab.kind.clone(),
            });
            ui.close_menu();
        }
        if ui.button("Split Down").clicked() {
            self.ctx.split_requests.push(SplitRequest {
                surface,
                node,
                split: egui_dock::Split::Below,
                new_tab: tab.kind.clone(),
            });
            ui.close_menu();
        }
    }
}

/// Sign-preserving response curve for mouse-rotate deltas. Goal: keep
/// the linear floor for sub-2px nudges (so 1-pixel corrections stay 1-pixel)
/// while ramping hard past ~10px so a real hand-sweep produces a full
/// rotation without the user dragging across the entire screen.
///
/// Shape: `dx + sign(dx) * |dx|^2 / 12 + sign(dx) * |dx|^3 / 900`.
/// - At `|dx|=1`: ≈ 1 + 0.083 + 0.001 ≈ 1.08 (essentially linear)
/// - At `|dx|=2`: ≈ 2 + 0.33 + 0.009 ≈ 2.34 (still close to linear)
/// - At `|dx|=10`: ≈ 10 + 8.33 + 1.11 ≈ 19.4 (~2× boost)
/// - At `|dx|=25`: ≈ 25 + 52 + 17.4 ≈ 94 (~3.8× boost — sweeps fly)
/// The cubic term provides the "hand sweep = full rotation" lift past
/// the ~10px knee without touching small-delta precision.
pub fn apply_rotate_curve(dx: f32) -> f32 {
    let a = dx.abs();
    dx + dx.signum() * a * a / 12.0 + dx.signum() * a * a * a / 900.0
}

impl<'a, 'ctx> WorkspaceViewer<'a, 'ctx> {
    pub(crate) fn draw_graph_tab(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

        if self.ctx.loaded_into_gpu {
            let cb = GraphCallback {
                screen_px: [rect.width().max(1.0), rect.height().max(1.0)],
            };
            ui.painter()
                .add(egui_wgpu::Callback::new_paint_callback(rect, cb));
        } else if let Some(msg) = self.ctx.load_msg {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                crate::ui::theme::mono(crate::ui::theme::font_size::DISPLAY),
                egui::Color32::WHITE,
            );
        }

        self.ctx.canvas_rect = Some(rect);

        if let Some(pos) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
            if rect.contains(pos) {
                self.ctx.pointer_in_canvas = Some(pos);
            }
        }

        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.ctx.click = Some((rect, pos));
            }
        }

        // LMB held = cursor "attract" force. RMB held with Shift = cursor
        // "repel" force. Both stay as direct egui reads — they're
        // held-tool toggles, not action events; modeling them as
        // jump-io triggers would just push the same booleans through
        // a thicker pipe.
        let pointer_in_canvas_for_btn = ui
            .input(|i| i.pointer.hover_pos())
            .map(|p| rect.contains(p))
            .unwrap_or(false);
        let shift_held = ui.input(|i| i.modifiers.shift);
        self.ctx.lmb_held = resp.dragged_by(egui::PointerButton::Primary)
            || ui.input(|i| {
                i.pointer.button_down(egui::PointerButton::Primary)
                    && pointer_in_canvas_for_btn
            });
        self.ctx.rmb_held = shift_held
            && ui.input(|i| {
                i.pointer.button_down(egui::PointerButton::Secondary)
                    && pointer_in_canvas_for_btn
            });

        // Camera deltas accumulated from jump-io semantic events.
        let mut yaw_d = 0.0_f32;
        let mut pitch_d = 0.0_f32;
        let mut pan_x = 0.0_f32;
        let mut pan_y = 0.0_f32;
        let mut pan_z = 0.0_f32;
        let mut zoom = 0.0_f32;

        // Read pointer position straight from input rather than from the
        // response — egui_dock's nested layout doesn't always propagate
        // hover state to the inner allocate_exact_size response, which
        // killed scroll-wheel zoom in the dockable workspace.
        let pointer_in_canvas = ui
            .input(|i| i.pointer.hover_pos())
            .map(|p| rect.contains(p))
            .unwrap_or(false);

        // WASDQE pan ramp. Starts at BASE units/s, ramps to MAX over
        // RAMP seconds of continuous input. Shift multiplies on top.
        // Resets on the first frame with no pan event so a quick tap
        // stays a tap.
        const PAN_BASE: f32 = 2400.0;
        const PAN_MAX: f32 = 24000.0;
        const PAN_RAMP: f32 = 0.32;
        const SHIFT_MUL: f32 = 4.0;
        let dt = ui.input(|i| i.unstable_dt.min(0.05));

        // Pre-scan events to drive the pan-accel timer and the
        // pinch-vs-wheel arbitration before consuming each event.
        let pan_event_active = self.ctx.input_events.iter().any(|ev| {
            matches!(
                ev,
                jump_io::Event::Axis1(
                    AppAction::PanX | AppAction::PanY | AppAction::PanZ,
                    _,
                )
            )
        });
        let pinch_active = self.ctx.input_events.iter().any(|ev| {
            matches!(ev, jump_io::Event::Axis1(AppAction::CameraZoomPinch, v) if v.abs() > 1e-3)
        });

        if pointer_in_canvas && pan_event_active {
            *self.ctx.pan_accel_t = (*self.ctx.pan_accel_t + dt).min(PAN_RAMP);
        } else {
            *self.ctx.pan_accel_t = 0.0;
        }
        // Ease-out cubic: gentle start, steeper finish — feels like
        // the camera "spools up" rather than ramping linearly.
        let pan_t = (*self.ctx.pan_accel_t / PAN_RAMP).clamp(0.0, 1.0);
        let pan_eased = 1.0 - (1.0 - pan_t).powi(3);
        let pan_base_speed = PAN_BASE + (PAN_MAX - PAN_BASE) * pan_eased;
        let pan_speed = pan_base_speed * if shift_held { SHIFT_MUL } else { 1.0 };

        // Walk the events. Pointer-in-canvas gates camera moves so a
        // user dragging out of a sidebar text field can't fly the
        // view. Shift-held suppresses CameraRotate so the cursor-repel
        // tool above keeps RMB+Shift to itself.
        //
        // "Currently focused window" gate: when any non-canvas panel
        // (inspector / section / filter strip / debug) is focused, the
        // scroll wheel belongs to its inner ScrollArea — the canvas
        // must NOT also consume the wheel as a zoom event. Without
        // this, scrolling inside an expanded inspector body both
        // scrolled the body AND zoomed the camera. The gate applies to
        // wheel + pinch zoom AND to scroll-driven PanZ (same input
        // stream). Camera-rotate (RMB drag) and WASDQE pan stay live
        // — they don't conflict with panel scroll.
        let panel_owns_scroll = self.ctx.focused_panel.is_some();
        let mut zoom_consumed = false;
        for ev in self.ctx.input_events {
            match ev {
                jump_io::Event::Axis2(AppAction::CameraRotate, [dx, dy]) => {
                    if shift_held || !pointer_in_canvas {
                        continue;
                    }
                    let mut dx = *dx;
                    let mut dy = *dy;
                    if self.ctx.invert_mouse_x {
                        dx = -dx;
                    }
                    if self.ctx.invert_mouse_y {
                        dy = -dy;
                    }
                    // Sensitivity 0.0085 → 0.011 paired with the
                    // mixed quadratic+cubic curve (see
                    // `apply_rotate_curve`) so 1px corrections stay
                    // precise while hand-sweeps actually fly.
                    yaw_d += apply_rotate_curve(dx) * 0.011;
                    pitch_d -= apply_rotate_curve(dy) * 0.011;
                }
                jump_io::Event::Axis1(AppAction::PanX, v) => {
                    if pointer_in_canvas {
                        // KeyHeld emits Axis1(action, dt * gain). Sign
                        // is in `v`; `pan_speed` carries the eased
                        // base speed + Shift multiplier.
                        pan_x += v * pan_speed;
                    }
                }
                jump_io::Event::Axis1(AppAction::PanY, v) => {
                    if pointer_in_canvas {
                        pan_y += v * pan_speed;
                    }
                }
                jump_io::Event::Axis1(AppAction::PanZ, v) => {
                    // PanZ also rides the scroll stream on some
                    // bindings — skip when a panel owns scroll.
                    if pointer_in_canvas && !panel_owns_scroll {
                        pan_z += v * pan_speed;
                    }
                }
                jump_io::Event::Axis1(AppAction::CameraZoomPinch, v) => {
                    if !pointer_in_canvas || panel_owns_scroll || v.abs() <= 1e-3 {
                        continue;
                    }
                    // Trigger::Pinch emits the *log* of pinch_delta —
                    // for small gestures ln(1+x) ≈ x so this matches
                    // the prior `(pinch - 1.0) * 320.0` to within
                    // sub-percent of the original feel. Coefficient
                    // 320 was chosen so trackpad pinch feels close to
                    // ctrl+wheel intensity.
                    zoom += v * 320.0;
                    zoom_consumed = true;
                }
                jump_io::Event::Axis1(AppAction::CameraZoomWheel, v) => {
                    if !pointer_in_canvas || panel_owns_scroll || pinch_active || v.abs() <= 0.5 {
                        // De-double-count: many laptops emit the
                        // same two-finger gesture as both pinch AND
                        // smooth scroll in the same frame. Pinch
                        // wins (more intentional "zoom" signal).
                        continue;
                    }
                    // Mixed curve: linear floor (so 1-tick wheel
                    // notches and tiny trackpad swipes feel
                    // proportional) plus a sqrt tail for hard flicks.
                    zoom += v.signum() * (v.abs() * 0.6 + v.abs().sqrt() * 26.0);
                    zoom_consumed = true;
                }
                _ => {}
            }
        }

        // Drain egui's scroll/zoom buffers when we consumed a zoom event
        // so the next frame doesn't see a phantom carry-over.
        if zoom_consumed {
            ui.ctx().input_mut(|i| {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                i.raw_scroll_delta = egui::Vec2::ZERO;
            });
        }

        if yaw_d != 0.0 || pitch_d != 0.0 || pan_x != 0.0 || pan_y != 0.0
            || pan_z != 0.0 || zoom != 0.0
        {
            // One-shot debug ping per non-zero camera-delta frame. Used by
            // the headless `tests/browser/regression.mjs` regression suite
            // to assert scroll-zoom + WASD pan stay wired up. Gated by the
            // existing RUST_LOG=info default — no spam unless the user
            // actually moves the camera.
            log::info!(
                "[graph-renderer] camera input: yaw={:.4} pitch={:.4} pan_xyz=[{:.2},{:.2},{:.2}] zoom={:.2}",
                yaw_d, pitch_d, pan_x, pan_y, pan_z, zoom
            );
            if let Some(wgpu_state) = self.ctx.frame.wgpu_render_state() {
                let mut renderer = wgpu_state.renderer.write();
                if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                    if yaw_d != 0.0 { pipes.camera.rotate_yaw(yaw_d); }
                    if pitch_d != 0.0 { pipes.camera.rotate_pitch(pitch_d); }
                    if pan_x != 0.0 || pan_y != 0.0 || pan_z != 0.0 {
                        pipes.camera.pan(pan_x, pan_y, pan_z);
                    }
                    if zoom != 0.0 {
                        // Distance-aware zoom: when the camera is far
                        // from the cluster a fixed `zoom += 50` barely
                        // moves the view; when close, the same units
                        // overshoot. Scale by `|position| / 1000`
                        // clamped to [0.2, 5.0] so close-in stays
                        // precise and far-out stays responsive. We use
                        // `position.length()` as a stand-in for
                        // distance-to-target since the camera currently
                        // looks at the origin; the clamp keeps the
                        // formula stable when the camera sits at or
                        // very near the origin (lower bound 0.2 means
                        // zoom never collapses to zero).
                        let dist = pipes.camera.position.length();
                        let dist_scale = (dist / 1000.0).clamp(0.2, 5.0);
                        pipes.camera.zoom(zoom * dist_scale);
                    }
                }
            }
        }
    }

    fn draw_welcome_tab(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("jump-cannon");
            ui.label("Rust + wgpu graph renderer");
            ui.add_space(12.0);
            ui.label("Drag tabs to split. Right-click a tab for split / close.");
            ui.add_space(20.0);
            if ui.button("Open Graph tab").clicked() {
                self.ctx.add_tab_requests.push(TabKind::Graph);
            }
        });
    }
}

/// Snapshot the current load state into a short string suitable for the
/// graph tab's placeholder label. Mirrors the pre-dock central-panel logic.
pub fn load_status_message(load: &LoadState) -> Option<String> {
    match load {
        LoadState::Pending => Some("loading…".to_string()),
        LoadState::Loading(m) => Some(m.clone()),
        _ => None,
    }
}
