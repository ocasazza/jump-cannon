//! Debug / perf section.
//!
//! Reads from the `PerfCollector` ring buffer the App fills each frame
//! and renders a stack of compact line charts via `egui_plot`. The
//! collector is owned by `App`; we only borrow `&PerfCollector` here.

use eframe::egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::perf::{PerfCollector, StageId};
use crate::ui::state::AppState;
use crate::ui::theme::accent;

use super::{subgroup_label, subgroup_separator};

const CHART_HEIGHT: f32 = 70.0;
const CHART_WINDOW_SECS: f64 = 10.0;

pub fn show(ui: &mut egui::Ui, state: &AppState, perf: &PerfCollector) {
    // Halted / running badge ------------------------------------------------
    let (dot_color, label) = if perf.last_halted {
        (accent::YELLOW, "halted")
    } else {
        (accent::GREEN, "running")
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, dot_color);
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .size(11.0)
                .color(egui::Color32::from_gray(200)),
        );
        ui.label(
            egui::RichText::new(format!("backend: {}", &perf.last_layout_id))
                .monospace()
                .size(11.0)
                .color(egui::Color32::from_gray(160)),
        );
    });

    ui.add_space(4.0);
    let s = &state.stats;
    ui.label(
        egui::RichText::new(format!(
            "nodes {}  edges {}  samples {}",
            s.n_nodes,
            s.n_edges,
            perf.len()
        ))
        .monospace()
        .size(11.0)
        .color(egui::Color32::from_gray(180)),
    );

    subgroup_separator(ui);

    // FPS ------------------------------------------------------------------
    let fps_pts = perf.fps_history();
    let fps_stats = PerfCollector::stats(fps_pts.iter().map(|p| p[1] as f32));
    chart_block(
        ui,
        "FPS",
        &fps_stats_label(fps_stats),
        &fps_pts,
        egui::Color32::from_rgb(120, 220, 120),
        "perf-chart-fps",
    );

    // Frame time ms --------------------------------------------------------
    let frame_pts = perf.frame_ms_history();
    let frame_stats = PerfCollector::stats(frame_pts.iter().map(|p| p[1] as f32));
    chart_block(
        ui,
        "Frame ms",
        &ms_stats_label(frame_stats),
        &frame_pts,
        egui::Color32::from_rgb(255, 220, 120),
        "perf-chart-frame",
    );

    // Per-stage timings (overlaid) -----------------------------------------
    subgroup_label(ui, "Stage timings (ms)");
    ui.add_space(2.0);
    let plot_id = ui.id().with("perf-chart-stages");
    let plot_x_max = perf
        .samples()
        .last()
        .map(|s| s.t)
        .unwrap_or(0.0);
    let plot_x_min = (plot_x_max - CHART_WINDOW_SECS).max(0.0);
    Plot::new(plot_id)
        .height(110.0)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .show_axes([false, true])
        .legend(Legend::default().background_alpha(0.4))
        .include_x(plot_x_min)
        .include_x(plot_x_max)
        .include_y(0.0)
        .show(ui, |plot_ui| {
            for stage in StageId::ALL {
                let pts = perf.stage_ms_history(stage);
                let [r, g, b] = stage.color();
                plot_ui.line(
                    Line::new(PlotPoints::from(pts))
                        .color(egui::Color32::from_rgb(r, g, b))
                        .name(stage.label()),
                );
            }
        });

    // Per-stage avg/p99/max readout ----------------------------------------
    ui.add_space(4.0);
    for stage in StageId::ALL {
        let stats = PerfCollector::stats(
            perf.samples().map(|s| s.stages[stage.idx()]),
        );
        let [r, g, b] = stage.color();
        ui.horizontal(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
            ui.label(
                egui::RichText::new(format!(
                    "{:<22} {}",
                    stage.label(),
                    ms_stats_label(stats)
                ))
                .monospace()
                .size(10.0)
                .color(egui::Color32::from_gray(180)),
            );
        });
    }

    subgroup_separator(ui);

    // KE history -----------------------------------------------------------
    let ke_pts = perf.ke_history();
    let ke_stats = PerfCollector::stats(ke_pts.iter().map(|p| p[1] as f32));
    chart_block(
        ui,
        "max KE",
        &format!(
            "avg {:.2} | p99 {:.2} | max {:.2}",
            ke_stats.0, ke_stats.1, ke_stats.2
        ),
        &ke_pts,
        egui::Color32::from_rgb(255, 120, 200),
        "perf-chart-ke",
    );
}

fn chart_block(
    ui: &mut egui::Ui,
    title: &str,
    sub: &str,
    points: &[[f64; 2]],
    color: egui::Color32,
    id: &str,
) {
    subgroup_label(ui, title);
    ui.label(
        egui::RichText::new(sub)
            .monospace()
            .size(10.0)
            .color(egui::Color32::from_gray(160)),
    );
    let plot_id = ui.id().with(id);
    let x_max = points.last().map(|p| p[0]).unwrap_or(0.0);
    let x_min = (x_max - CHART_WINDOW_SECS).max(0.0);
    Plot::new(plot_id)
        .height(CHART_HEIGHT)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .show_axes([false, true])
        .include_x(x_min)
        .include_x(x_max)
        .include_y(0.0)
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(PlotPoints::from(points.to_vec())).color(color),
            );
        });
    ui.add_space(6.0);
}

fn fps_stats_label((avg, p99, max): (f32, f32, f32)) -> String {
    // For FPS the "p99" we want is actually the *low* end (worst frame
    // pacing) — but we keep symmetry with the ms label and just report
    // avg / p99 / max as computed.
    format!("avg {:.1} | p99 {:.1} | max {:.1}", avg, p99, max)
}

fn ms_stats_label((avg, p99, max): (f32, f32, f32)) -> String {
    format!("avg {:.2}ms | p99 {:.2}ms | max {:.2}ms", avg, p99, max)
}
