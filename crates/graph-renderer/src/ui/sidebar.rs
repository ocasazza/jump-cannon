//! 44px activity bar + 280px section panel.
//!
//! Icons are drawn as simple geometric primitives via the painter so we
//! aren't at the mercy of font emoji coverage in headless WebGPU.

use eframe::egui;

use super::actions::ActionRegistry;
use super::layout::registry::LayoutRegistry;
use super::floating::FloatingPanel;
use super::sections;
use super::state::{AppState, FocusedPanel, PanelId, Section};
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
        // Metrics: a bar chart (three vertical bars + baseline).
        Section::Metrics => {
            let base = center.y + 7.0;
            let bar = egui::Stroke::new(2.5, color);
            for (i, h) in [6.0f32, 12.0, 9.0].iter().enumerate() {
                let x = center.x - 6.0 + i as f32 * 6.0;
                painter.line_segment([egui::pos2(x, base), egui::pos2(x, base - h)], bar);
            }
            painter.line_segment(
                [egui::pos2(center.x - 9.0, base), egui::pos2(center.x + 8.0, base)],
                egui::Stroke::new(1.0, color),
            );
        }
        // Generate: a star/hub — central node with radiating spokes to a
        // ring of leaves (the prefilled starGen demo, visually).
        Section::Generate => {
            let leaves = 6;
            for i in 0..leaves {
                let theta = std::f32::consts::TAU * (i as f32) / (leaves as f32);
                let leaf = center + egui::vec2(theta.cos(), theta.sin()) * r * 0.85;
                painter.line_segment([center, leaf], s);
                painter.circle_filled(leaf, 1.8, color);
            }
            painter.circle_filled(center, 2.5, color);
        }
        // Timeline: a scrub track (horizontal line) with a playhead knob and
        // a play triangle — the transport metaphor.
        Section::Timeline => {
            let track_y = center.y + 4.0;
            painter.line_segment(
                [egui::pos2(center.x - 8.0, track_y), egui::pos2(center.x + 8.0, track_y)],
                s,
            );
            painter.circle_filled(egui::pos2(center.x + 1.0, track_y), 2.2, color);
            let tip = center + egui::vec2(4.0, -5.0);
            let a = center + egui::vec2(-3.0, -8.0);
            let b = center + egui::vec2(-3.0, -2.0);
            painter.line_segment([a, b], s);
            painter.line_segment([b, tip], s);
            painter.line_segment([tip, a], s);
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

/// Floating section panels. Each `Section` whose `section_open[s]`
/// flag is true renders as an independent `FloatingPanel` keyed by
/// `PanelId::Section(s)` — egui persists each panel's position by id, so
/// once the user drags a section it stays where they put it.
///
/// Default positions cascade from `[16, 64]` (24px down + 12px right per
/// index in `Section::ALL`) so that several panels opened in quick
/// succession don't stack atop one another on first reveal.
pub fn show_floating(
    ctx: &egui::Context,
    state: &mut AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    for (idx, &section) in Section::ALL.iter().enumerate() {
        if !state.is_section_open(section) {
            continue;
        }
        // Tiled panels live in the workspace tree; skip the floating
        // chrome for them. The workspace SidePanel renders them.
        if crate::ui::tiles::section_placement(state, section)
            == crate::ui::tiles::Placement::Tiled
        {
            continue;
        }
        let mut open = true;
        // Debug is right-justified on first open (the user wants the
        // console hugging the right edge of the canvas so it doesn't
        // overlap the section panels cascading from the left). Other
        // panels keep the cascading default.
        let default_size = match section {
            Section::Instances => [320.0, 680.0],
            Section::Debug => [360.0, 520.0],
            _ => [280.0, 520.0],
        };
        let pos = if matches!(section, Section::Debug) {
            let screen = ctx.screen_rect();
            let panel_w = default_size[0];
            let margin = 16.0;
            if screen.width() > panel_w + margin {
                [screen.right() - panel_w - margin, 64.0]
            } else {
                [16.0 + idx as f32 * 12.0, 64.0 + idx as f32 * 24.0]
            }
        } else {
            [16.0 + idx as f32 * 12.0, 64.0 + idx as f32 * 24.0]
        };
        let mut placement = crate::ui::tiles::section_placement(state, section);
        let placement_before = placement;
        // The focus channel uses an indirection: we temporarily take
        // `focused_panel` out of `state` so the closure body can hold
        // `&mut state` exclusively, then put it back after `show`.
        let mut focused = std::mem::take(&mut state.focused_panel);
        let my_focus_id = match section {
            Section::Debug => FocusedPanel::Debug,
            other => FocusedPanel::Section(other),
        };
        let panel_id = PanelId::Section(section);
        let mut collapsed = state.collapsed_panels.contains(&panel_id);
        FloatingPanel::new(panel_id, section.title())
            .default_pos(pos)
            .default_size(default_size)
            .with_placement(&mut placement)
            .with_focus(&mut focused, my_focus_id)
            .with_collapsed(&mut collapsed)
            .show(ctx, &mut open, |ui| {
                render_section_body(ui, state, section, registry, layout_registry, perf);
            });
        state.focused_panel = focused;
        if collapsed {
            state.collapsed_panels.insert(panel_id);
        } else {
            state.collapsed_panels.remove(&panel_id);
        }
        if placement != placement_before {
            state.section_placement.insert(section, placement);
            if placement == crate::ui::tiles::Placement::Tiled {
                let mut ws = std::mem::take(&mut state.tiles);
                ws.snap_insert(crate::ui::tiles::PaneKind::Section(section));
                state.tiles = ws;
            }
        }
        if !open {
            state.set_section_open(section, false);
            // Closing the focused panel drops focus back to the canvas.
            if state.focused_panel == Some(my_focus_id) {
                state.focused_panel = None;
            }
        }
    }
}
