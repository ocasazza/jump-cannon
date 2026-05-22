//! 3D-anchored floating panel.
//!
//! Projects a world-space anchor through the same view-projection the
//! graph canvas was painted with, then opens an `egui::Area` at the
//! resulting screen point with the project's standard floating chrome
//! (squircle backdrop + 1px border) and a tether line back to the
//! anchor.
//!
//! Diverges from `draw_inspector_leader_line` in one place: when the
//! anchor's NDC falls outside `[-1, 1]` we *clamp* the panel to the
//! viewport edge and draw an angled triangular arrow toward the
//! off-screen anchor, instead of hiding. The inspector leader line was
//! a "supplementary visual" — losing it when the node panned away was
//! fine. Anchored panels carry the actual content (hover preview /
//! promoted card); silently disappearing them would feel like a bug.
//!
//! The `BehindCamera` case (clip-w ≤ 0) still hides: there's no sane
//! viewport-edge mapping when the anchor is literally behind the eye.
//!
//! See the comment on `project_world_to_canvas` for why `aspect` is
//! sourced from `canvas_rect` rather than `camera.aspect` — same
//! one-frame-lag trap that `draw_inspector_leader_line` documents.

use eframe::egui::{self, Color32, Id, InnerResponse, LayerId, Order, Pos2, Rect, Stroke, Vec2};

use crate::camera::Camera;
use crate::ui::{squircle, theme};

/// How the projected anchor sits relative to the canvas viewport.
///
/// `Visible` and `OffScreen` both produce a rendered panel — the
/// difference is whether the panel was clamped to the viewport edge
/// (off-screen anchor → arrow tether) or sits naturally over its
/// projected point (on-screen anchor → straight line tether).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AnchoredResult {
    Visible,
    OffScreen,
    BehindCamera,
}

/// Outcome of world→canvas projection. Off-screen still carries the
/// projected screen point so callers can clamp & draw an arrow.
enum ProjectionOutcome {
    OnScreen { screen: Pos2, _depth: f32 },
    OffScreen { screen: Pos2, _depth: f32 },
    BehindCamera,
}

/// Project a world-space point into the canvas, using `canvas_rect`'s
/// aspect ratio (NOT `camera.aspect`).
///
/// Why: when this runs (typically during egui's CentralPanel pass), the
/// `GraphPaintCallback::prepare()` hook hasn't fired yet for the current
/// frame, so `camera.aspect` still reflects the *previous* frame's
/// canvas size. Mirror what `App::draw_inspector_leader_line` does and
/// pull the aspect from the fresh `canvas_rect`.
fn project_world_to_canvas(
    camera: &Camera,
    canvas_rect: Rect,
    world: glam::Vec3,
) -> ProjectionOutcome {
    let aspect = (canvas_rect.width() / canvas_rect.height().max(0.0001)).max(0.0001);
    let view = glam::Mat4::look_to_rh(camera.position, camera.forward(), glam::Vec3::Y);
    let proj = glam::Mat4::perspective_rh(camera.fov_y, aspect, camera.znear, camera.zfar);
    let clip = (proj * view) * world.extend(1.0);
    if clip.w <= 0.0 {
        return ProjectionOutcome::BehindCamera;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    // NDC y is up; egui screen y is down — flip on y.
    let screen_x = canvas_rect.left() + (ndc_x * 0.5 + 0.5) * canvas_rect.width();
    let screen_y = canvas_rect.top() + (1.0 - (ndc_y * 0.5 + 0.5)) * canvas_rect.height();
    let screen = egui::pos2(screen_x, screen_y);
    let depth = clip.z / clip.w;
    if (-1.0..=1.0).contains(&ndc_x) && (-1.0..=1.0).contains(&ndc_y) {
        ProjectionOutcome::OnScreen { screen, _depth: depth }
    } else {
        ProjectionOutcome::OffScreen { screen, _depth: depth }
    }
}

/// Builder for a 3D-anchored floating panel.
///
/// `offset` is the auto-position delta (e.g. nudge the panel below-and-
/// right of the projected anchor so the cursor / node glyph isn't
/// covered). `anchor_pixels` is reserved for a future soft-tether
/// drag UI: the user grabs the panel, drags it, and the delta lives
/// here so the panel stays put relative to the anchor across camera
/// motion. Today no UI populates it; pass `None` and ignore.
pub struct AnchoredPanel<'a> {
    pub id: Id,
    pub world_pos: glam::Vec3,
    pub canvas_rect: Rect,
    pub camera: &'a Camera,
    pub offset: Vec2,
    /// User-drag delta in screen pixels (persists per node).
    ///
    /// TODO(soft-tether): wire a drag UI on the panel header so the
    /// user can re-position the card and the arrow keeps tracking the
    /// projected anchor. Plumbed but unused for now.
    pub anchor_pixels: Option<Vec2>,
    /// Inset (px) from the viewport edge when the panel is clamped
    /// because the anchor is off-screen. 40 px keeps the squircle
    /// fully on-canvas with room for the arrow stub.
    pub clamp_margin: f32,
    /// Inner padding inside the squircle backdrop.
    pub inner_margin: f32,
    /// Squircle corner radius. Matches the standard floating chrome.
    pub corner_radius: f32,
    /// If false, the area is non-interactable (e.g. hover preview).
    pub interactable: bool,
}

impl<'a> AnchoredPanel<'a> {
    pub fn new(id: Id, world_pos: glam::Vec3, canvas_rect: Rect, camera: &'a Camera) -> Self {
        Self {
            id,
            world_pos,
            canvas_rect,
            camera,
            offset: Vec2::new(16.0, 16.0),
            anchor_pixels: None,
            clamp_margin: 40.0,
            inner_margin: 8.0,
            corner_radius: 10.0,
            interactable: true,
        }
    }

    pub fn offset(mut self, offset: Vec2) -> Self {
        self.offset = offset;
        self
    }

    pub fn interactable(mut self, interactable: bool) -> Self {
        self.interactable = interactable;
        self
    }

    /// Render the panel.
    ///
    /// On `BehindCamera`, returns `(BehindCamera, None)` without
    /// opening an Area. Otherwise opens a foreground-layer Area at
    /// the (clamped, if off-screen) anchor + offset, paints the
    /// squircle backdrop, invokes `add_contents`, and draws a tether
    /// from the panel edge back to the projected anchor (or an arrow
    /// stub at the clamped edge pointing toward the off-screen
    /// anchor).
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> (AnchoredResult, Option<InnerResponse<R>>) {
        let outcome = project_world_to_canvas(self.camera, self.canvas_rect, self.world_pos);
        let (projected_screen, on_screen) = match outcome {
            ProjectionOutcome::BehindCamera => return (AnchoredResult::BehindCamera, None),
            ProjectionOutcome::OnScreen { screen, .. } => (screen, true),
            ProjectionOutcome::OffScreen { screen, .. } => (screen, false),
        };

        let user_delta = self.anchor_pixels.unwrap_or(Vec2::ZERO);
        let raw_pos = projected_screen + self.offset + user_delta;

        // Clamp the panel into the viewport (with margin) when the
        // anchor itself is off-screen. We don't clamp on-screen
        // anchors — the panel is allowed to spill slightly past the
        // canvas edge as the user pans, since egui will auto-place
        // tooltips relative to the screen anyway.
        let clamp_rect = self.canvas_rect.shrink(self.clamp_margin);
        let panel_pos = if on_screen {
            raw_pos
        } else {
            egui::pos2(
                raw_pos.x.clamp(clamp_rect.left(), clamp_rect.right()),
                raw_pos.y.clamp(clamp_rect.top(), clamp_rect.bottom()),
            )
        };

        let area_id = self.id;
        let corner_radius = self.corner_radius;
        let inner_margin = self.inner_margin;

        let area = egui::Area::new(area_id)
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .interactable(self.interactable);

        let inner = area.show(ctx, |ui| {
            // Paint squircle backdrop first via an allocate-then-paint
            // dance: we don't yet know the inner content size, so use
            // egui::Frame to handle the allocate+paint, then overlay
            // the squircle. Simpler: use a Frame with our colors,
            // then paint the squircle ourselves on top of the
            // frame's rect after the fact via a layered painter.
            //
            // Concretely: render the body inside a transparent
            // Frame so it self-sizes; capture the rect; paint the
            // squircle backdrop *below* the body with an offset
            // shape index by inserting the shape at the start of
            // the layer's shape list.
            let backdrop_idx = ui.painter().add(egui::Shape::Noop);

            let frame = egui::Frame::none()
                .inner_margin(egui::Margin::same(inner_margin));
            let body = frame.show(ui, add_contents);

            let panel_rect = body.response.rect;
            // Replace the placeholder with the squircle backdrop.
            ui.painter().set(
                backdrop_idx,
                egui::Shape::convex_polygon(
                    squircle::squircle_path(
                        panel_rect,
                        corner_radius,
                        squircle::DEFAULT_N,
                        squircle::DEFAULT_SEGMENTS_PER_CORNER,
                    ),
                    theme::FLOATING_BACKDROP,
                    Stroke::new(1.0, Color32::WHITE),
                ),
            );

            body.inner
        });

        // Draw the tether on a dedicated foreground layer keyed off
        // the panel id. Using a per-panel layer id keeps multiple
        // anchored panels from clobbering each other's tethers.
        let painter = ctx.layer_painter(LayerId::new(
            Order::Foreground,
            Id::new(("anchor-tether", area_id)),
        ));
        let panel_rect = inner.response.rect;
        let stroke = Stroke::new(1.0, theme::palette::BORDER);

        if on_screen {
            let edge = closest_edge_midpoint(panel_rect, projected_screen);
            painter.line_segment([edge, projected_screen], stroke);
            painter.circle_filled(projected_screen, 2.5, theme::palette::ICON);
        } else {
            // Off-screen anchor: draw a triangular arrow at the
            // panel-side edge pointing toward the off-screen anchor.
            // Direction is from the panel center to the (uncloamped)
            // projected anchor — the panel is clamped but the
            // projected screen point still encodes the bearing.
            let center = panel_rect.center();
            let dir = (projected_screen - center).normalized();
            // Tip = a few pixels outside the panel rect along `dir`.
            let edge = closest_edge_midpoint(panel_rect, projected_screen);
            let tip = edge + dir * 10.0;
            // Perpendicular for the arrow base.
            let perp = Vec2::new(-dir.y, dir.x);
            let base_a = edge + perp * 5.0;
            let base_b = edge - perp * 5.0;
            painter.add(egui::Shape::convex_polygon(
                vec![tip, base_a, base_b],
                theme::palette::BORDER,
                stroke,
            ));
        }

        let result = if on_screen {
            AnchoredResult::Visible
        } else {
            AnchoredResult::OffScreen
        };
        (result, Some(inner))
    }
}

/// Midpoint of the edge of `rect` closest to `target`.
///
/// We pick edges (not corners) so the tether reads as "coming out of
/// the side of the card" rather than from a corner — corners would
/// suggest a connector tab anchored to a 90° bend, which is the wrong
/// visual metaphor for a soft anchor.
fn closest_edge_midpoint(rect: Rect, target: Pos2) -> Pos2 {
    let c = rect.center();
    let dx = target.x - c.x;
    let dy = target.y - c.y;
    // Compare in units of half-extents so a wide-but-short panel
    // doesn't always prefer its left/right edges.
    let hx = rect.width() * 0.5;
    let hy = rect.height() * 0.5;
    let ax = dx.abs() / hx.max(0.0001);
    let ay = dy.abs() / hy.max(0.0001);
    if ax > ay {
        if dx > 0.0 {
            egui::pos2(rect.right(), c.y)
        } else {
            egui::pos2(rect.left(), c.y)
        }
    } else if dy > 0.0 {
        egui::pos2(c.x, rect.bottom())
    } else {
        egui::pos2(c.x, rect.top())
    }
}
