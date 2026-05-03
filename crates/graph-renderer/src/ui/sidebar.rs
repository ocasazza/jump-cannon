//! 44px activity bar + 280px section panel.
//!
//! Icons are drawn as simple geometric primitives via the painter so we
//! aren't at the mercy of font emoji coverage in headless WebGPU.

use eframe::egui;

use super::actions::ActionRegistry;
use super::layout::registry::LayoutRegistry;
use super::sections;
use super::state::{AppState, Section};
use crate::perf::PerfCollector;

const ACTIVITY_W: f32 = 44.0;
const ACTIVITY_BTN: f32 = 40.0;
const SECTION_W: f32 = 280.0;

pub fn show(
    ctx: &egui::Context,
    state: &mut AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    show_activity_bar(ctx, state);
    if let Some(active) = state.active_section {
        show_section_panel(ctx, state, active, registry, layout_registry, perf);
    }
}

fn show_activity_bar(ctx: &egui::Context, state: &mut AppState) {
    egui::SidePanel::left("activity-bar")
        .exact_width(ACTIVITY_W)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::BLACK)
                .stroke(egui::Stroke::NONE)
                .inner_margin(egui::Margin::ZERO),
        )
        .show(ctx, |ui| {
            // Right border separating from section panel / central panel.
            let rect = ui.max_rect();
            ui.painter().vline(
                rect.right() - 0.5,
                rect.y_range(),
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            );

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);

            // 2px top padding before first button.
            ui.add_space(2.0);

            // Wrap in a vertical ScrollArea so the bottom buttons remain
            // reachable on short viewports (9 sections × 42px overflows
            // many browser window heights). Hide the scrollbar — users
            // can flick with the mouse wheel.
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    for &section in Section::ALL {
                        let active = state.active_section == Some(section);
                        if activity_button(ui, section, active).clicked() {
                            state.active_section = if active { None } else { Some(section) };
                        }
                    }
                });
        });
}

fn activity_button(ui: &mut egui::Ui, section: Section, active: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ACTIVITY_BTN, ACTIVITY_BTN),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    let painter = ui.painter();

    let (bg, fg) = if active {
        (egui::Color32::WHITE, egui::Color32::BLACK)
    } else if hovered {
        (egui::Color32::from_gray(40), egui::Color32::WHITE)
    } else {
        (egui::Color32::BLACK, egui::Color32::WHITE)
    };
    painter.rect_filled(rect, 0.0, bg);

    draw_icon(painter, rect, section, fg);

    if active {
        // 2px white stripe on the right edge as VSCode-style "active" indicator
        let mut stripe = rect;
        stripe.set_left(rect.right() - 2.0);
        painter.rect_filled(stripe, 0.0, fg);
    }

    response.on_hover_text(section.title())
}

fn draw_icon(painter: &egui::Painter, rect: egui::Rect, section: Section, color: egui::Color32) {
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

fn show_section_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    active: Section,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    egui::SidePanel::left("section-panel")
        .exact_width(SECTION_W)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::BLACK)
                .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
                .inner_margin(egui::Margin {
                    left: 16.0,
                    right: 16.0,
                    top: 14.0,
                    bottom: 14.0,
                }),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                sections::show(ui, active, state, registry, layout_registry, perf);
            });
        });
}
