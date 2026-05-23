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
/// covered). `anchor_pixels` is the soft-tether user-drag delta in
/// screen pixels (persists per node). `screen_pos_override`, when
/// supplied, replaces the internal re-projection for *panel placement*
/// (e.g. an EMA-smoothed value) while the tether arrow is always drawn
/// to the freshly re-projected anchor — so the arrow tracks the true
/// node while the panel sits at the smoothed point.
pub struct AnchoredPanel<'a> {
    pub id: Id,
    pub world_pos: glam::Vec3,
    pub canvas_rect: Rect,
    pub camera: &'a Camera,
    pub offset: Vec2,
    /// User-drag delta in screen pixels (persists per node).
    pub anchor_pixels: Option<Vec2>,
    /// EMA-smoothed projected screen position. When `Some`, used in
    /// place of the freshly re-projected anchor for *panel placement*
    /// (the tether arrow still uses the live projection). When `None`,
    /// the panel falls back to the internal projection.
    pub screen_pos_override: Option<Pos2>,
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
    /// When `true`, the panel is in "expanded" mode: the caller is
    /// expected to render the full inspector body (metrics + neighbours
    /// + frontmatter + page editor) and the panel auto-sizes to a
    /// larger default. AnchoredPanel itself only forwards this flag
    /// through `AnchoredOutput::expanded` for caller-side branching —
    /// the squircle chrome, tether, drag handling, and EMA-smoothed
    /// placement are identical in both modes. (The panel growing
    /// physically larger is a side effect of the larger body the
    /// caller renders, not a size override at this layer.)
    pub expanded: bool,
    /// Expected panel size in pixels (width, height). When `Some`, the
    /// panel position is pre-clamped so the rect from `panel_pos` to
    /// `panel_pos + reserved_size` stays within
    /// `canvas_rect.shrink(clamp_margin)` — prevents the expanded body
    /// from running off the right/bottom edge when the anchor sits near
    /// the viewport corner. `None` falls back to anchor-based clamping
    /// only (the legacy behavior for hover-preview-sized panels that
    /// don't risk overflow).
    pub reserved_size: Option<Vec2>,
}

/// Outcome of [`AnchoredPanel::show`]. `drag_delta` carries the
/// per-frame drag delta from the *caller-supplied header response*
/// (NOT the outer area), so the caller can accumulate it into a
/// persistent per-node soft-tether offset without scroll-drag inside
/// the body bubbling up and moving the panel. `header_double_clicked`
/// mirrors the header response's `double_clicked()` so the caller can
/// wire re-snap on header double-click without re-querying the
/// response after `show` returns.
pub struct AnchoredOutput<R> {
    pub result: AnchoredResult,
    pub inner: Option<InnerResponse<R>>,
    pub drag_delta: Vec2,
    pub header_double_clicked: bool,
    /// Echoes the panel's `expanded` flag. Useful for callers that
    /// dispatch on the same flag for post-show bookkeeping (e.g.
    /// "if expanded, request a larger frame" — currently unused, but
    /// kept symmetrical with the input so future state writes don't
    /// need to re-thread the bool through a separate channel).
    pub expanded: bool,
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
            screen_pos_override: None,
            clamp_margin: 40.0,
            inner_margin: 8.0,
            corner_radius: 10.0,
            interactable: true,
            expanded: false,
            reserved_size: None,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Tell the panel its expected pixel size so its position can be
    /// pre-clamped to keep the whole rect inside the viewport. Required
    /// for the expanded body (large) so a node near the right/bottom
    /// edge doesn't push the panel off-screen.
    pub fn reserved_size(mut self, size: Vec2) -> Self {
        self.reserved_size = Some(size);
        self
    }

    pub fn offset(mut self, offset: Vec2) -> Self {
        self.offset = offset;
        self
    }

    pub fn interactable(mut self, interactable: bool) -> Self {
        self.interactable = interactable;
        self
    }

    pub fn anchor_pixels(mut self, anchor_pixels: Option<Vec2>) -> Self {
        self.anchor_pixels = anchor_pixels;
        self
    }

    pub fn screen_pos_override(mut self, screen_pos: Option<Pos2>) -> Self {
        self.screen_pos_override = screen_pos;
        self
    }

    /// Render the panel.
    ///
    /// On `BehindCamera`, returns a no-op output. Otherwise opens a
    /// foreground-layer Area at the (clamped, if off-screen) anchor +
    /// offset, paints the squircle backdrop, invokes `add_contents`,
    /// and draws a tether from the panel edge back to the projected
    /// anchor (or an arrow stub at the clamped edge pointing toward
    /// the off-screen anchor).
    ///
    /// When `screen_pos_override` is set, *panel placement* uses the
    /// override; the *tether* still aims at the live re-projected
    /// anchor, so the visible string between panel and node stays
    /// truthful while the panel position is allowed to lead/lag.
    ///
    /// `add_contents` returns `(R, egui::Response)` — the second tuple
    /// element is the *drag-sensing header response*, allocated by the
    /// caller with `Sense::click_and_drag()`. AnchoredPanel reads
    /// `drag_delta()` and `double_clicked()` from that response and
    /// exposes them on `AnchoredOutput`. This prevents drag-vs-scroll
    /// conflicts: body widgets (e.g. a ScrollArea over markdown) sense
    /// their own drag without it bubbling up to move the panel. If a
    /// caller has no header (transient hover-preview style), pass a
    /// non-draggable dummy response (e.g. allocate a zero-size rect
    /// with `Sense::hover()`); the caller can also simply ignore
    /// `drag_delta`.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add_contents: impl FnOnce(&mut egui::Ui) -> (R, egui::Response),
    ) -> AnchoredOutput<R> {
        let outcome = project_world_to_canvas(self.camera, self.canvas_rect, self.world_pos);
        let (projected_screen, on_screen) = match outcome {
            ProjectionOutcome::BehindCamera => {
                return AnchoredOutput {
                    result: AnchoredResult::BehindCamera,
                    inner: None,
                    drag_delta: Vec2::ZERO,
                    header_double_clicked: false,
                    expanded: self.expanded,
                };
            }
            ProjectionOutcome::OnScreen { screen, .. } => (screen, true),
            ProjectionOutcome::OffScreen { screen, .. } => (screen, false),
        };

        // Panel placement source: smoothed override if the caller
        // supplied one, otherwise the live re-projection. The tether
        // below always uses `projected_screen` so it tracks the real
        // node position.
        let placement_anchor = self.screen_pos_override.unwrap_or(projected_screen);

        let user_delta = self.anchor_pixels.unwrap_or(Vec2::ZERO);
        let raw_pos = placement_anchor + self.offset + user_delta;

        // Clamp the panel into the viewport (with margin). Two regimes:
        //
        // 1. `reserved_size` is Some (expanded body): clamp so the full
        //    expected rect — `panel_pos .. panel_pos + reserved_size` —
        //    fits inside `canvas.shrink(clamp_margin)`. Without this,
        //    expanding a panel anchored near the right/bottom edge sends
        //    the body off-screen.
        // 2. `reserved_size` is None (hover preview / compact): clamp
        //    only when the anchor itself is off-screen AND no explicit
        //    override is set. With an override, trust the caller; with
        //    an on-screen anchor and a small panel, the chance of
        //    overflow is negligible and clamping fights soft-tether
        //    drag intent.
        let clamp_rect = self.canvas_rect.shrink(self.clamp_margin);
        let panel_pos = if let Some(size) = self.reserved_size {
            // The right/bottom edge of the panel must stay inside
            // clamp_rect. Push the top-left in by however much overflow
            // we'd otherwise have. min after subtraction guards against
            // a panel larger than the viewport (rare; we leave it
            // top-left aligned in that case rather than clipping the
            // header off the top).
            let max_x = (clamp_rect.right() - size.x).max(clamp_rect.left());
            let max_y = (clamp_rect.bottom() - size.y).max(clamp_rect.top());
            egui::pos2(
                raw_pos.x.clamp(clamp_rect.left(), max_x),
                raw_pos.y.clamp(clamp_rect.top(), max_y),
            )
        } else if on_screen || self.screen_pos_override.is_some() {
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

            // body.inner is (R, header_response). Return both so we
            // can read header drag/double-click outside the closure.
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
        // Split the inner tuple: we expose `InnerResponse<R>` to the
        // caller, but the drag/double-click come from the
        // *caller-supplied header response* — NOT `inner.response`
        // (which is the outer Area's response, and would pick up
        // scroll-drag inside any body widget).
        let InnerResponse {
            inner: (user_value, header_resp),
            response: area_resp,
        } = inner;
        let drag_delta = header_resp.drag_delta();
        let header_double_clicked = header_resp.double_clicked();
        AnchoredOutput {
            result,
            inner: Some(InnerResponse::new(user_value, area_resp)),
            drag_delta,
            header_double_clicked,
            expanded: self.expanded,
        }
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
