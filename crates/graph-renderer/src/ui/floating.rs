//! Reusable floating-panel wrapper.
//!
//! Wraps an `egui::Window` with the project's standard floating chrome:
//! squircle background painted in `FLOATING_BACKDROP` with a 1px
//! `palette::BORDER` stroke, a custom header row (label + close `X`
//! button) replacing the default title bar, and an `&mut bool`
//! open/closed flag driven by the X button and the tray launcher row.
//!
//! All floating panels in the app go through this type — adding a new
//! one should be a 5-line builder call, not a copy of `egui::Window`
//! plumbing.

use std::borrow::Cow;

use eframe::egui::{self, Color32, Id, Rect, Stroke};

use crate::ui::squircle;
use crate::ui::state::{FocusedPanel, PanelId};
use crate::ui::theme::{self, palette};
use crate::ui::tiles::Placement;
use crate::ui::traffic_lights;

/// Vertical padding above the header row. Lifts the traffic-light cluster
/// out of the window's top-edge resize zone and gives the header room.
const HEADER_TOP_PAD: f32 = 6.0;
/// Horizontal padding left of the header row. Lifts the (left-aligned)
/// traffic lights out of the top-LEFT corner resize handle, which would
/// otherwise intercept their clicks.
const HEADER_LEFT_PAD: f32 = 6.0;

/// Builder for a squircle-backed floating panel whose visibility is
/// driven by a caller-owned `&mut bool`.
pub struct FloatingPanel<'p> {
    id: PanelId,
    title: Cow<'static, str>,
    default_pos: Option<[f32; 2]>,
    default_size: Option<[f32; 2]>,
    /// Optional placement toggle plumbed through the header. When
    /// `Some`, a small `⊟` button appears between the title and the
    /// close X; clicking it flips the placement to `Tiled` (the caller
    /// is expected to react by snap-inserting the panel into the
    /// workspace tree on the next frame via
    /// `ui::tiles::sync_tree_with_open_state`).
    placement: Option<&'p mut Placement>,
    /// Optional focus channel. When `Some`, the panel renders its
    /// outer chrome with a `palette::PRIMARY` 3px stroke when
    /// `*focused == Some(my_id)`, and writes `my_id` into the channel
    /// when the user interacts with the panel (header click or drag,
    /// or a body click). This drives the "currently focused window"
    /// concept that gates canvas-scroll-zoom while panel scroll is
    /// active.
    focus: Option<(&'p mut Option<FocusedPanel>, FocusedPanel)>,
    /// Optional collapse channel driven by the yellow "minimize" traffic
    /// light. When `Some`, clicking minimize toggles `*collapsed`; while
    /// `*collapsed` the panel renders only its header chrome (title +
    /// traffic lights) and suppresses `body`. When `None`, the minimize
    /// dot is hidden — a panel with no collapse target shouldn't claim
    /// to have one.
    collapsed: Option<&'p mut bool>,
}

impl<'p> FloatingPanel<'p> {
    pub fn new(id: PanelId, title: impl Into<Cow<'static, str>>) -> Self {
        Self {
            id,
            title: title.into(),
            default_pos: None,
            default_size: None,
            placement: None,
            focus: None,
            collapsed: None,
        }
    }

    /// Plumb a collapse flag through the header. Enables the yellow
    /// "minimize" traffic light, which collapses the panel to just its
    /// header row.
    pub fn with_collapsed(mut self, collapsed: &'p mut bool) -> Self {
        self.collapsed = Some(collapsed);
        self
    }

    /// Plumb a focus channel through the panel. See [`Self::focus`].
    pub fn with_focus(
        mut self,
        focused: &'p mut Option<FocusedPanel>,
        my_id: FocusedPanel,
    ) -> Self {
        self.focus = Some((focused, my_id));
        self
    }

    pub fn default_pos(mut self, pos: [f32; 2]) -> Self {
        self.default_pos = Some(pos);
        self
    }

    pub fn default_size(mut self, size: [f32; 2]) -> Self {
        self.default_size = Some(size);
        self
    }

    /// Plumb a `Placement` reference through the header. Adds a tile-
    /// snap glyph next to the X.
    pub fn with_placement(mut self, p: &'p mut Placement) -> Self {
        self.placement = Some(p);
        self
    }

    /// Render the panel. Skips entirely when `!*open`. The X button in
    /// the custom header sets `*open = false`.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        open: &mut bool,
        body: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<R> {
        if !*open {
            return None;
        }

        let frame = theme::floating_frame()
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE);

        // The window NAME is decoupled from the displayed title: the title
        // bar is hidden (`title_bar(false)`) and the egui id is set
        // explicitly below, so `Window::new`'s string is otherwise unused.
        // Feeding it the display title made egui emit a SECOND accessible
        // title node (window-name + the chrome label), reading as a
        // duplicate title to screen readers / the test harness. A stable
        // per-id name keeps the visible chrome label the sole title.
        let window_name = format!("floating-panel-{:?}", self.id);
        let mut window = egui::Window::new(window_name)
            .id(Id::new(("floating", self.id)))
            .title_bar(false)
            .frame(frame)
            .resizable(true)
            .movable(true)
            .collapsible(false);

        if let Some(pos) = self.default_pos {
            window = window.default_pos(pos);
        }
        if let Some(size) = self.default_size {
            window = window.default_size(size);
        }

        let title = self.title;
        let mut placement = self.placement;
        let mut collapsed = self.collapsed;
        // Snapshot collapse state for the body gate below. The traffic-
        // light helper toggles `*collapsed` in place during the header
        // draw, so capture the pre-draw value to decide whether to run
        // `body` this frame (toggling collapse takes effect next frame —
        // standard egui single-pass behavior).
        let is_collapsed = collapsed.as_deref().copied().unwrap_or(false);

        // Decide focus stroke up front so the squircle paint inside the
        // closure can read it without re-borrowing self.focus. The
        // write-back (when the user clicks the panel) happens after
        // the window closure returns.
        let is_focused = match &self.focus {
            Some((focused, my_id)) => focused.as_ref() == Some(my_id),
            None => false,
        };
        let outer_stroke = if is_focused {
            Stroke::new(3.0, palette::PRIMARY)
        } else {
            // New window implementation uses 2.0px borders
            Stroke::new(2.0, palette::BORDER)
        };

        let response = window.show(ctx, |ui| {
            let rect: Rect = ui.max_rect().expand(theme::spacing::SECTION_GAP);
            let mut painter = ui.painter().clone();
            painter.set_layer_id(egui::LayerId::new(
                egui::Order::Background,
                ui.layer_id().id,
            ));
            squircle::paint_squircle(
                &painter,
                rect,
                10.0,
                theme::FLOATING_BACKDROP,
                outer_stroke,
            );

            // Inset the header off the window's top-left corner. egui's
            // `resizable(true)` window reserves an interactive resize zone
            // along every edge/corner; the top-left corner handle sits
            // exactly over the traffic-light cluster and steals its clicks.
            // A few px of top + left padding pushes the dots out of that
            // corner zone so they stay hit-testable, and the vertical
            // breathing room stops the header from cramming against the
            // body's first row.
            ui.add_space(HEADER_TOP_PAD);
            ui.horizontal(|ui| {
                ui.add_space(HEADER_LEFT_PAD);
                // Traffic light cluster (left-aligned), via the shared
                // helper so every panel's chrome matches. Minimize is
                // only offered when a collapse channel is wired; the
                // green maximize dot doubles as the tile/float toggle.
                let lights = traffic_lights::TrafficLights::all()
                    .show_minimize(collapsed.is_some())
                    .show_maximize(placement.is_some())
                    .maximize_tip("Toggle Tile/Float");
                let action = traffic_lights::show(ui, lights);

                if action.close {
                    *open = false;
                }
                if action.minimize {
                    if let Some(c) = collapsed.as_deref_mut() {
                        *c = !*c;
                    }
                }
                if action.maximize {
                    if let Some(p) = placement.as_deref_mut() {
                        *p = match *p {
                            Placement::Floating => Placement::Tiled,
                            Placement::Tiled => Placement::Floating,
                        };
                    }
                }

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(title.as_ref())
                        .font(theme::mono(theme::font_size::HEADING))
                        .color(palette::TEXT),
                );
            });

            // Collapsed: render header chrome only, suppress the body.
            if is_collapsed {
                return None;
            }
            ui.separator();

            Some(body(ui))
        });

        // Write-back focus on user interaction with this panel. The
        // outer `egui::Window` response surfaces clicks and drags
        // anywhere over the window's area (header, body, chrome). We
        // also probe `ctx.input` for a pointer-down inside the panel's
        // area rect as a belt-and-suspenders: some body widgets
        // (TextEdit, ScrollArea) consume the click before the area
        // response sees it.
        if let (Some(r), Some((focused, my_id))) = (response.as_ref(), self.focus) {
            let outer = &r.response;
            let area_rect = outer.rect;
            let pointer_down_inside = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .map(|p| area_rect.contains(p))
                        .unwrap_or(false)
            });
            let acquired = outer.clicked()
                || outer.drag_started()
                || pointer_down_inside;
            if acquired && *focused != Some(my_id) {
                *focused = Some(my_id);
            }
        }

        // Window `show` returns `Option<InnerResponse<Option<R>>>` —
        // the inner `Option` is `None` when the panel was collapsed
        // (body suppressed). Flatten both layers.
        response.and_then(|r| r.inner).flatten()
    }
}
