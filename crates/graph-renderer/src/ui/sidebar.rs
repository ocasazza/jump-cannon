//! 44px activity bar + 280px section panel.
//!
//! Icons are drawn as simple geometric primitives via the painter so we
//! aren't at the mercy of font emoji coverage in headless WebGPU.

use eframe::egui;

use super::actions::ActionRegistry;
use super::layout::registry::LayoutRegistry;
use super::floating::FloatingPanel;
use super::sections;
use super::state::{AppState, PanelId, Section};
use crate::perf::PerfCollector;

// Section launchers live in `status_footer::show_tray` now; this
// module owns icon rendering (`draw_icon`) + the floating section
// panel (`show_floating`).

pub(crate) fn draw_icon(painter: &egui::Painter, rect: egui::Rect, section: Section, color: egui::Color32) {
    // Icons are drawn in an 18×18 cell centered in the button rect.
    let center = rect.center();
    let s = egui::Stroke::new(1.5, color);
    let r = 9.0; // half of 18px icon size
    match section {
        // Magnifier: circle + handle.
        Section::Filter => {
            painter.circle_stroke(center - egui::vec2(2.0, 2.0), r * 0.7, s);
            let p1 = center + egui::vec2(2.0, 2.0);
            let p2 = center + egui::vec2(7.0, 7.0);
            painter.line_segment([p1, p2], s);
        }
        // Style: three horizontal sliders.
        Section::Style => {
            for i in 0..3 {
                let y = center.y - 6.0 + i as f32 * 6.0;
                painter.line_segment(
                    [egui::pos2(center.x - 8.0, y), egui::pos2(center.x + 8.0, y)],
                    s,
                );
                let knob_x = center.x + (-4.0 + i as f32 * 4.0);
                painter.rect_filled(
                    egui::Rect::from_center_size(egui::pos2(knob_x, y), egui::vec2(3.0, 5.0)),
                    0.0,
                    color,
                );
            }
        }
        // Layout: a 3-node triangle graph.
        Section::Layout => {
            let a = center + egui::vec2(0.0, -8.0);
            let b = center + egui::vec2(-8.0, 6.0);
            let c = center + egui::vec2(8.0, 6.0);
            painter.line_segment([a, b], s);
            painter.line_segment([b, c], s);
            painter.line_segment([c, a], s);
            for p in [a, b, c] {
                painter.circle_filled(p, 2.5, color);
            }
        }
        // Camera: rect with circle lens.
        Section::Camera => {
            let body = egui::Rect::from_center_size(center, egui::vec2(20.0, 14.0));
            painter.rect_stroke(body, 0.0, s);
            painter.circle_stroke(center, 4.0, s);
            let bump = egui::Rect::from_min_size(
                egui::pos2(center.x - 4.0, body.top() - 3.0),
                egui::vec2(8.0, 3.0),
            );
            painter.rect_stroke(bump, 0.0, s);
        }
        // Focus: concentric circles.
        Section::Focus => {
            painter.circle_stroke(center, 9.0, s);
            painter.circle_stroke(center, 5.0, s);
            painter.circle_filled(center, 1.5, color);
        }
        // Cursor: crosshair.
        Section::Cursor => {
            painter.circle_stroke(center, 8.0, s);
            painter.line_segment(
                [egui::pos2(center.x - 11.0, center.y), egui::pos2(center.x - 4.0, center.y)],
                s,
            );
            painter.line_segment(
                [egui::pos2(center.x + 4.0, center.y), egui::pos2(center.x + 11.0, center.y)],
                s,
            );
            painter.line_segment(
                [egui::pos2(center.x, center.y - 11.0), egui::pos2(center.x, center.y - 4.0)],
                s,
            );
            painter.line_segment(
                [egui::pos2(center.x, center.y + 4.0), egui::pos2(center.x, center.y + 11.0)],
                s,
            );
        }
        // Stats: lowercase i in a circle.
        Section::Stats => {
            painter.circle_stroke(center, 9.0, s);
            painter.circle_filled(center + egui::vec2(0.0, -4.0), 1.5, color);
            painter.line_segment(
                [center + egui::vec2(0.0, -1.0), center + egui::vec2(0.0, 5.0)],
                s,
            );
        }
        // Debug: a tiny line chart (sparkline).
        Section::Debug => {
            let pts = [
                center + egui::vec2(-9.0, 4.0),
                center + egui::vec2(-5.0, -2.0),
                center + egui::vec2(-1.0, 1.0),
                center + egui::vec2(3.0, -5.0),
                center + egui::vec2(7.0, 0.0),
            ];
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], s);
            }
            // baseline
            painter.line_segment(
                [center + egui::vec2(-9.0, 7.0), center + egui::vec2(8.0, 7.0)],
                egui::Stroke::new(1.0, color),
            );
        }
        // Instances: stacked rectangles (cards).
        Section::Instances => {
            for i in 0..3 {
                let off = -6.0 + i as f32 * 4.0;
                let rect = egui::Rect::from_center_size(
                    egui::pos2(center.x, center.y + off),
                    egui::vec2(16.0, 3.0),
                );
                painter.rect_stroke(rect, 0.0, s);
            }
        }
    }
}

fn render_section_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    active: Section,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        sections::show(ui, active, state, registry, layout_registry, perf);
    });
}

/// Floating variant of the section panel. Renders the same inner body
/// inside a `FloatingPanel` keyed by `PanelId::Sidebar`. The activity
/// bar stays docked and is not touched here.
pub fn show_floating(
    ctx: &egui::Context,
    state: &mut AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    let Some(active) = state.active_section else {
        return;
    };
    let mut open = true;
    FloatingPanel::new(PanelId::Sidebar, active.title())
        .default_pos([16.0, 64.0])
        .default_size([280.0, 520.0])
        .show(ctx, &mut open, |ui| {
            render_section_body(ui, state, active, registry, layout_registry, perf);
        });
    if !open {
        state.active_section = None;
    }
}
