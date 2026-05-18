//! Squircle (superellipse) shape helper.
//!
//! egui's built-in `Rounding` produces circular corners. For iOS-style
//! "squircle" floating panels we want true superellipse corners
//! (|x/r|^n + |y/r|^n = 1, with n in the 4..=5 range).
//!
//! This module samples the superellipse around each of the four corners,
//! connects them with straight edges, and returns a closed polygon that
//! egui can render with `Shape::convex_polygon` (fill + stroke in one go).

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};

/// Default superellipse exponent. n=4 is the canonical "Apple squircle".
pub const DEFAULT_N: f32 = 4.0;
/// Default sample count per corner. 12 is smooth enough for typical panels.
pub const DEFAULT_SEGMENTS_PER_CORNER: usize = 12;

/// Returns the closed polygon ring approximating a squircle inscribed in `rect`.
///
/// `radius` is the corner radius; it is clamped to `min(width, height) / 2`.
/// `n` is the superellipse exponent (use 4.0 for a soft iOS-like squircle,
/// higher = closer to a rounded rectangle's straight edges).
/// `segments_per_corner` controls smoothness; each corner gets that many
/// sampled points (inclusive of the corner endpoints), so the returned
/// ring has roughly `4 * segments_per_corner` vertices.
pub fn squircle_path(
    rect: Rect,
    radius: f32,
    n: f32,
    segments_per_corner: usize,
) -> Vec<Pos2> {
    let segments = segments_per_corner.max(2);
    let half_min = (rect.width().min(rect.height()) * 0.5).max(0.0);
    let r = radius.max(0.0).min(half_min);
    let inv_n = 2.0 / n.max(0.0001);

    let mut out = Vec::with_capacity(segments * 4);

    // Corner centers.
    let tr = Pos2::new(rect.right() - r, rect.top() + r);
    let br = Pos2::new(rect.right() - r, rect.bottom() - r);
    let bl = Pos2::new(rect.left() + r, rect.bottom() - r);
    let tl = Pos2::new(rect.left() + r, rect.top() + r);

    // Walk clockwise starting from the top edge, going TR -> BR -> BL -> TL.
    // For each corner we sweep t in [0, PI/2] and map to the local quadrant.
    //
    // Local superellipse point at parameter t (t in [0, PI/2]):
    //   sx = |cos t|^(2/n)
    //   sy = |sin t|^(2/n)
    // Both nonneg in this range; sign handled per-corner.
    let sample = |t: f32| -> (f32, f32) {
        let c = t.cos().abs().powf(inv_n);
        let s = t.sin().abs().powf(inv_n);
        (c, s)
    };

    use std::f32::consts::FRAC_PI_2;

    // Top-right corner: starts at (cx, cy - r) (top edge) and ends at (cx + r, cy) (right edge).
    // x = cx + r * sx, y = cy - r * sy.
    for i in 0..segments {
        let t = FRAC_PI_2 * (i as f32) / ((segments - 1) as f32);
        // At i=0: t=0 -> sx=1, sy=0 -> (cx+r, cy-r) ... wait that's wrong for "start at top edge".
        // Re-derive: start with t=PI/2 -> sx=0, sy=1 -> (cx, cy - r) which IS the top-edge meeting point.
        // So we sweep t from PI/2 down to 0 around TR.
        let t = FRAC_PI_2 - t;
        let (sx, sy) = sample(t);
        out.push(Pos2::new(tr.x + r * sx, tr.y - r * sy));
    }

    // Bottom-right corner: starts at (cx + r, cy) and ends at (cx, cy + r).
    // x = cx + r * sx, y = cy + r * sy. Sweep t from 0 to PI/2.
    for i in 0..segments {
        let t = FRAC_PI_2 * (i as f32) / ((segments - 1) as f32);
        let (sx, sy) = sample(t);
        out.push(Pos2::new(br.x + r * sx, br.y + r * sy));
    }

    // Bottom-left corner: starts at (cx, cy + r) and ends at (cx - r, cy).
    // x = cx - r * sx, y = cy + r * sy. Sweep t from PI/2 down to 0.
    for i in 0..segments {
        let t = FRAC_PI_2 - FRAC_PI_2 * (i as f32) / ((segments - 1) as f32);
        let (sx, sy) = sample(t);
        out.push(Pos2::new(bl.x - r * sx, bl.y + r * sy));
    }

    // Top-left corner: starts at (cx - r, cy) and ends at (cx, cy - r).
    // x = cx - r * sx, y = cy - r * sy. Sweep t from 0 to PI/2.
    for i in 0..segments {
        let t = FRAC_PI_2 * (i as f32) / ((segments - 1) as f32);
        let (sx, sy) = sample(t);
        out.push(Pos2::new(tl.x - r * sx, tl.y - r * sy));
    }

    out
}

/// Paints a filled + stroked squircle into `painter` using the defaults.
///
/// For full control over `n` / segment count, call [`squircle_path`] and
/// build the `Shape` yourself.
pub fn paint_squircle(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    fill: Color32,
    stroke: Stroke,
) {
    let pts = squircle_path(rect, radius, DEFAULT_N, DEFAULT_SEGMENTS_PER_CORNER);
    painter.add(Shape::convex_polygon(pts, fill, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_count_matches_segments_times_four() {
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 120.0));
        let pts = squircle_path(rect, 24.0, 4.0, 12);
        assert_eq!(pts.len(), 12 * 4);

        let pts = squircle_path(rect, 24.0, 5.0, 8);
        assert_eq!(pts.len(), 8 * 4);
    }

    #[test]
    fn radius_is_clamped_for_small_rects() {
        // A 10x4 rect can't have radius 50 — should clamp to min(w,h)/2 = 2.
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(10.0, 4.0));
        let pts = squircle_path(rect, 50.0, 4.0, 6);
        assert_eq!(pts.len(), 24);
        // All points must lie within the rect bounds (with small float epsilon).
        for p in &pts {
            assert!(p.x >= rect.left() - 1e-3 && p.x <= rect.right() + 1e-3);
            assert!(p.y >= rect.top() - 1e-3 && p.y <= rect.bottom() + 1e-3);
        }
    }

    #[test]
    fn zero_radius_collapses_to_rect_corners() {
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 50.0));
        let pts = squircle_path(rect, 0.0, 4.0, 5);
        // With r=0 every sample collapses to the corresponding rect corner.
        for p in &pts {
            let on_corner = (p.x == rect.left() || p.x == rect.right())
                && (p.y == rect.top() || p.y == rect.bottom());
            assert!(on_corner, "point {:?} not at a rect corner", p);
        }
    }
}
