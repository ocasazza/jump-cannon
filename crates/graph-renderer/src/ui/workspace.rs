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
fn apply_rotate_curve(dx: f32) -> f32 {
    let a = dx.abs();
    dx + dx.signum() * a * a / 12.0 + dx.signum() * a * a * a / 900.0
}

impl<'a, 'ctx> WorkspaceViewer<'a, 'ctx> {
    fn draw_graph_tab(&mut self, ui: &mut egui::Ui) {
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
                egui::FontId::proportional(14.0),
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
        // "repel" force. Plain RMB-drag rotates the camera (3D-editor
        // convention) — see the rotation block below.
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

        // Aggregate per-frame camera deltas so we open the wgpu callback
        // resources at most once.
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

        // Camera rotation — RMB-drag is the standard convention in 3D
        // editors (Unity / Blender fly-mode / Unreal). Middle-drag also
        // rotates as a fallback for trackpad users without a real RMB.
        // RMB+Shift is reserved for the cursor "repel" tool above, so
        // we only rotate on RMB *without* shift.
        let rmb_drag_rotate = !shift_held
            && pointer_in_canvas
            && ui.input(|i| {
                i.pointer.button_down(egui::PointerButton::Secondary)
            });
        let mid_dragging = resp.dragged_by(egui::PointerButton::Middle)
            || (pointer_in_canvas
                && ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle)));
        if rmb_drag_rotate || mid_dragging {
            let d = ui.input(|i| i.pointer.delta());
            let mut dx = d.x;
            let mut dy = d.y;
            if self.ctx.invert_mouse_x { dx = -dx; }
            if self.ctx.invert_mouse_y { dy = -dy; }
            // Sensitivity 0.0085 → 0.011 paired with a steeper
            // quadratic+cubic curve (see `apply_rotate_curve`) so 1px
            // corrections stay precise while hand-sweeps actually fly.
            yaw_d   += apply_rotate_curve(dx) * 0.011;
            pitch_d -= apply_rotate_curve(dy) * 0.011;
        }

        // Wheel + pinch zoom. `smooth_scroll_delta` is the egui-recommended
        // accumulator for vertical scroll (mouse wheel + most trackpad
        // two-finger gestures). `zoom_delta` is egui's normalised
        // pinch-gesture multiplier (`1.0` = no pinch; >1 zooms in,
        // <1 out) which trackpads + ctrl+wheel both produce in the
        // browser. We feed both through the same camera.zoom path so
        // the device-of-the-day picks whichever it has.
        if pointer_in_canvas {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            // De-double-count: many laptops emit the same two-finger
            // gesture as both `smooth_scroll_delta` AND `zoom_delta` in
            // the same frame. Pinch wins when present — it's the more
            // intentional "zoom" signal — and we then drop scroll for
            // this frame so the camera doesn't double up.
            let pinch_active = (pinch - 1.0).abs() > 1e-3;
            if pinch_active {
                // `(pinch - 1.0)` is a small signed multiplier. Bumped
                // from ×240 → ×320 so trackpad pinch feels closer to
                // ctrl+wheel intensity. Still drained by egui itself.
                zoom += (pinch - 1.0) * 320.0;
                // Drain the wheel signal so it can't sneak through on
                // a later frame as a phantom second contribution.
                ui.ctx().input_mut(|i| {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    i.raw_scroll_delta = egui::Vec2::ZERO;
                });
            } else if scroll.abs() > 0.5 {
                // Mixed curve: a linear floor (so 1-tick wheel notches
                // and tiny trackpad swipes still feel proportional)
                // plus a sqrt tail for hard flicks. Coefficient bumped
                // from 18 → 26 so trackpad two-finger swipes don't
                // feel under-driven.
                zoom += scroll.signum() * (scroll.abs() * 0.6 + scroll.abs().sqrt() * 26.0);
                ui.ctx().input_mut(|i| {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    i.raw_scroll_delta = egui::Vec2::ZERO;
                });
            }
        }

        // WASDQE keyboard pan / vertical. Same pointer-over-canvas guard
        // so typing into a sidebar text field doesn't fly the camera.
        // Speed eases in: starts at BASE units/s, ramps to MAX over RAMP
        // seconds of continuous input. Shift multiplies by SHIFT_MUL on
        // top of the eased value. Ramp resets on the first frame with no
        // pan input so a quick tap stays a tap.
        const PAN_BASE: f32   = 2400.0;   // units/s at start of hold
        const PAN_MAX: f32    = 24000.0;  // units/s after full ramp
        const PAN_RAMP: f32   = 0.32;     // seconds to reach PAN_MAX
        const SHIFT_MUL: f32  = 4.0;
        if pointer_in_canvas {
            let (dt, w, a, s, d, q, e, shift) = ui.input(|i| (
                i.unstable_dt.min(0.05),
                i.key_down(egui::Key::W),
                i.key_down(egui::Key::A),
                i.key_down(egui::Key::S),
                i.key_down(egui::Key::D),
                i.key_down(egui::Key::Q),
                i.key_down(egui::Key::E),
                i.modifiers.shift,
            ));
            let any = w || a || s || d || q || e;
            if any {
                *self.ctx.pan_accel_t = (*self.ctx.pan_accel_t + dt).min(PAN_RAMP);
            } else {
                *self.ctx.pan_accel_t = 0.0;
            }
            // Ease-out cubic: gentle start, steeper finish — feels like
            // the camera "spools up" rather than ramping linearly.
            let t = (*self.ctx.pan_accel_t / PAN_RAMP).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            let base_speed = PAN_BASE + (PAN_MAX - PAN_BASE) * eased;
            let speed = base_speed * if shift { SHIFT_MUL } else { 1.0 } * dt;
            // W/S = vertical (up/down), Q/E = forward/back, A/D = strafe
            // — swapped from the conventional FPS layout per user
            // preference (Minecraft-creative-style). Up on W matches
            // mouse-rotate pitch direction so it feels coherent.
            if w { pan_y += speed; }
            if s { pan_y -= speed; }
            if d { pan_x += speed; }
            if a { pan_x -= speed; }
            if q { pan_z += speed; }
            if e { pan_z -= speed; }
        } else {
            *self.ctx.pan_accel_t = 0.0;
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
