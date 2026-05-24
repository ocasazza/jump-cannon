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

use eframe::egui::{self, Color32, Id, Rect, Stroke};

use crate::ui::squircle;
use crate::ui::state::{FocusedPanel, PanelId};
use crate::ui::theme::{self, palette};
use crate::ui::tiles::Placement;

/// Builder for a squircle-backed floating panel whose visibility is
/// driven by a caller-owned `&mut bool`.
pub struct FloatingPanel<'p> {
    id: PanelId,
    title: &'static str,
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
}

impl<'p> FloatingPanel<'p> {
    pub fn new(id: PanelId, title: &'static str) -> Self {
        Self {
            id,
            title,
            default_pos: None,
            default_size: None,
            placement: None,
            focus: None,
        }
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

        let mut window = egui::Window::new(self.title)
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
            Stroke::new(1.0, palette::BORDER)
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

            ui.horizontal(|ui| {
                // Panel toggles — left. The drag glyph is a passive
                // affordance; the actual drag is handled by the outer
                // egui::Window. Showing it tells the user where to
                // grab the panel.
                ui.label(
                    egui::RichText::new("\u{2261}")
                        .font(theme::mono(theme::font_size::HEADING))
                        .color(palette::GREY),
                );
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::mono(theme::font_size::HEADING))
                        .color(palette::TEXT),
                );
                // View controls — right.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.small_button("X").clicked() {
                            *open = false;
                        }
                        if let Some(p) = placement.as_deref_mut() {
                            // ⊟ when currently floating → click to tile;
                            // ⤢ when currently tiled → click to float.
                            // (When tiled, this branch usually isn't
                            // rendered because the panel goes through
                            // the tile chrome instead, but we keep the
                            // toggle symmetrical for completeness.)
                            let (glyph, tip) = match p {
                                Placement::Floating => ("\u{229F}", "Snap into tile workspace"),
                                Placement::Tiled => ("\u{2922}", "Float (un-tile)"),
                            };
                            if ui.small_button(glyph).on_hover_text(tip).clicked() {
                                *p = match *p {
                                    Placement::Floating => Placement::Tiled,
                                    Placement::Tiled => Placement::Floating,
                                };
                            }
                        }
                    },
                );
            });
            ui.separator();

            body(ui)
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

        response.and_then(|r| r.inner)
    }
}
