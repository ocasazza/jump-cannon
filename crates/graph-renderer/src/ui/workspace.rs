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

        self.ctx.lmb_held = resp.dragged_by(egui::PointerButton::Primary)
            || ui.input(|i| {
                i.pointer.button_down(egui::PointerButton::Primary) && resp.hovered()
            });
        self.ctx.rmb_held = ui.input(|i| {
            i.pointer.button_down(egui::PointerButton::Secondary) && resp.hovered()
        });

        // Aggregate per-frame camera deltas so we open the wgpu callback
        // resources at most once.
        let mut yaw_d = 0.0_f32;
        let mut pitch_d = 0.0_f32;
        let mut pan_x = 0.0_f32;
        let mut pan_y = 0.0_f32;
        let mut pan_z = 0.0_f32;
        let mut zoom = 0.0_f32;

        if resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            let mut dx = d.x;
            let mut dy = d.y;
            if self.ctx.invert_mouse_x { dx = -dx; }
            if self.ctx.invert_mouse_y { dy = -dy; }
            yaw_d += dx * 0.005;
            pitch_d -= dy * 0.005;
        }

        // Scroll-wheel zoom (only when the canvas is hovered so it doesn't
        // fight scrollable sidebars). raw_scroll_delta gives px on most
        // mice / touchpads; scale to a useful camera-units amount.
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                zoom += scroll * 2.0;
            }
        }

        // WASDQE keyboard pan / vertical (only while hovered to avoid
        // hijacking text inputs in sidebars). Speed scales with frame dt.
        if resp.hovered() {
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
            let speed = if shift { 5.0 } else { 1.0 } * 400.0 * dt;
            if w { pan_z += speed; }
            if s { pan_z -= speed; }
            if d { pan_x += speed; }
            if a { pan_x -= speed; }
            if q { pan_y += speed; }
            if e { pan_y -= speed; }
        }

        if yaw_d != 0.0 || pitch_d != 0.0 || pan_x != 0.0 || pan_y != 0.0
            || pan_z != 0.0 || zoom != 0.0
        {
            if let Some(wgpu_state) = self.ctx.frame.wgpu_render_state() {
                let mut renderer = wgpu_state.renderer.write();
                if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                    if yaw_d != 0.0 { pipes.camera.rotate_yaw(yaw_d); }
                    if pitch_d != 0.0 { pipes.camera.rotate_pitch(pitch_d); }
                    if pan_x != 0.0 || pan_y != 0.0 || pan_z != 0.0 {
                        pipes.camera.pan(pan_x, pan_y, pan_z);
                    }
                    if zoom != 0.0 { pipes.camera.zoom(zoom); }
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
