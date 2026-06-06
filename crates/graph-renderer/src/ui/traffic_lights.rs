//! macOS-style "traffic-light" window controls.
//!
//! A single shared helper that draws the three top-LEFT circle buttons
//! (red = close, yellow = minimize, green = maximize/zoom) and reports
//! which one was clicked. Every panel with a title-bar header — the
//! `FloatingPanel`, the tiled panes, and the anchored node card — routes
//! its window chrome through this so the cluster looks and behaves the
//! same everywhere.
//!
//! Colors match the macOS convention already used inline in
//! `floating.rs`:
//!   * close    → `rgb(255, 96, 92)`
//!   * minimize → `rgb(255, 189, 68)`
//!   * maximize → `rgb(0, 202, 78)`
//!
//! The caller decides which lights to show (e.g. a pane with no collapse
//! target can hide the yellow dot) and wires the returned action to its
//! own state — `close` hides, `minimize` collapses-to-header, `maximize`
//! toggles the panel's expand/tile state.

use eframe::egui::{self, Color32};

/// Which traffic-light button the user clicked this frame, if any.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TrafficAction {
    pub close: bool,
    pub minimize: bool,
    pub maximize: bool,
}

impl TrafficAction {
    pub fn any(self) -> bool {
        self.close || self.minimize || self.maximize
    }
}

/// Which lights to render. A panel that can't collapse omits
/// `minimize`; one with no maximize/tile target omits `maximize`.
#[derive(Copy, Clone, Debug)]
pub struct TrafficLights {
    pub close: bool,
    pub minimize: bool,
    pub maximize: bool,
    /// Tooltip for the green dot — panels differ ("Maximize" vs
    /// "Restore" vs "Toggle Tile/Float").
    pub maximize_tip: &'static str,
}

impl Default for TrafficLights {
    fn default() -> Self {
        Self {
            close: true,
            minimize: true,
            maximize: true,
            maximize_tip: "Maximize",
        }
    }
}

impl TrafficLights {
    /// All three dots with the default maximize tooltip.
    pub fn all() -> Self {
        Self::default()
    }

    pub fn maximize_tip(mut self, tip: &'static str) -> Self {
        self.maximize_tip = tip;
        self
    }

    pub fn show_minimize(mut self, on: bool) -> Self {
        self.minimize = on;
        self
    }

    pub fn show_maximize(mut self, on: bool) -> Self {
        self.maximize = on;
        self
    }
}

const CLOSE_COLOR: Color32 = Color32::from_rgb(255, 96, 92);
const MINIMIZE_COLOR: Color32 = Color32::from_rgb(255, 189, 68);
const MAXIMIZE_COLOR: Color32 = Color32::from_rgb(0, 202, 78);
const DOT_DIAMETER: f32 = 10.0;
const DOT_RADIUS: f32 = 5.0;

/// Draw the traffic-light cluster left-aligned at the current cursor and
/// return which dot was clicked. Lights render in macOS order
/// (close, minimize, maximize) left-to-right. Hover dims the dot
/// slightly so it reads as interactive even without glyphs.
///
/// Call this as the first thing inside a header `ui.horizontal(...)`,
/// then add `ui.add_space(...)` + the title label after it.
pub fn show(ui: &mut egui::Ui, lights: TrafficLights) -> TrafficAction {
    let prev_spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 6.0;

    let mut action = TrafficAction::default();

    let dot = |ui: &mut egui::Ui, color: Color32, tip: &str| -> bool {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(DOT_DIAMETER, DOT_DIAMETER), egui::Sense::click());
        let draw = if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            color.gamma_multiply(0.8)
        } else {
            color
        };
        ui.painter().circle_filled(rect.center(), DOT_RADIUS, draw);
        // Name the dot as a labeled button in the accessibility tree. The
        // glyph-less circle carries no text, so without this it's invisible
        // to AccessKit (and to the headless egui_kittest harness, which
        // hit-tests the cluster by label). The label matches the tooltip.
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip)
        });
        response.on_hover_text(tip).clicked()
    };

    if lights.close && dot(ui, CLOSE_COLOR, "Close") {
        action.close = true;
    }
    if lights.minimize && dot(ui, MINIMIZE_COLOR, "Minimize") {
        action.minimize = true;
    }
    if lights.maximize && dot(ui, MAXIMIZE_COLOR, lights.maximize_tip) {
        action.maximize = true;
    }

    ui.spacing_mut().item_spacing.x = prev_spacing;
    action
}
