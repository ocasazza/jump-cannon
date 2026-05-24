//! Debug console.
//!
//! Two-mode panel: an [`Events`](super::super::state::DebugViewMode::Events)
//! view that scrolls the rolling frontend-event log, and a
//! [`Stats`](super::super::state::DebugViewMode::Stats) view that holds the
//! perf charts plus the engine-status summary previously hosted by the
//! standalone Stats section.
//!
//! The Stats view borrows a `&PerfCollector` filled by `App` each frame
//! and renders a stack of compact line charts via `egui_plot`.

use eframe::egui;
use egui_plot::{Corner, Legend, Line, Plot, PlotPoints};

use crate::perf::{PerfCollector, StageId};
use crate::ui::state::{AppState, DebugViewMode, SimStatus};
use crate::ui::theme::accent;

use super::{subgroup_label, subgroup_separator};

const CHART_HEIGHT: f32 = 70.0;
const CHART_WINDOW_SECS: f64 = 10.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, perf: &PerfCollector) {
    state.snapshot_source = Some("Debug".into());

    // Mode toggle ----------------------------------------------------------
    ui.horizontal(|ui| {
        let mut mode = state.debug_view_mode;
        for option in [DebugViewMode::Events, DebugViewMode::Stats] {
            let label = match option {
                DebugViewMode::Events => "Events",
                DebugViewMode::Stats => "Stats",
            };
            if ui
                .selectable_label(mode == option, label)
                .clicked()
                && mode != option
            {
                mode = option;
                let to_label = match option {
                    DebugViewMode::Events => "events",
                    DebugViewMode::Stats => "stats",
                };
                state.frontend_events.push(
                    "debug",
                    format!("mode -> {to_label}"),
                );
            }
        }
        if mode != state.debug_view_mode {
            state.debug_view_mode = mode;
        }
    });
    ui.add_space(6.0);

    match state.debug_view_mode {
        DebugViewMode::Events => show_events(ui, state),
        DebugViewMode::Stats => show_stats(ui, state, perf),
    }
}

// ---------------------------------------------------------------------------
// Events view
// ---------------------------------------------------------------------------

fn show_events(ui: &mut egui::Ui, state: &mut AppState) {
    let total = state.frontend_events.entries.len();
    ui.label(
        egui::RichText::new(format!(
            "{total} event(s) (cap {})",
            state.frontend_events.cap
        ))
        .monospace()
        .size(11.0)
        .color(egui::Color32::from_gray(160)),
    );
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(ui.available_height() - 30.0)
        .show(ui, |ui| {
            if state.frontend_events.entries.is_empty() {
                ui.label(
                    egui::RichText::new("(no events yet — try the palette, a chip, or a section)")
                        .italics()
                        .size(11.0)
                        .color(egui::Color32::from_gray(140)),
                );
                return;
            }
            // Newest first.
            for ev in state.frontend_events.entries.iter().rev() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format_hms(ev.timestamp_ms))
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                    ui.label(
                        egui::RichText::new(&ev.source)
                            .monospace()
                            .size(11.0)
                            .color(accent::GREEN),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&ev.message)
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_gray(210)),
                        )
                        .wrap(),
                    );
                });
            }
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .small_button("Clear")
            .on_hover_text("Drop all event log entries")
            .clicked()
        {
            state.frontend_events.clear();
        }
    });
}

fn format_hms(ms: u64) -> String {
    let secs_in_day = (ms / 1000) % 86_400;
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// ---------------------------------------------------------------------------
// Stats view (previously `sections/stats.rs` + the perf charts that already
// lived here). Widgets and labels are preserved verbatim — only the host
// section has changed.
// ---------------------------------------------------------------------------

fn show_stats(ui: &mut egui::Ui, state: &mut AppState, perf: &PerfCollector) {
    // Engine status dot (was in stats.rs).
    let (sim_color, sim_label) = match state.sim_status {
        SimStatus::Running => (accent::GREEN, "running"),
        SimStatus::Settled => (accent::YELLOW, "settled"),
        SimStatus::Error => (accent::RED, "error"),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, sim_color);
        ui.label(
            egui::RichText::new(sim_label)
                .monospace()
                .size(11.0)
                .color(egui::Color32::from_gray(180)),
        );
    });

    // Halted / running badge from the perf collector (was at the top of
    // the old debug section).
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
    let n = if s.n_nodes == 0 { "—".to_string() } else { s.n_nodes.to_string() };
    let m = if s.n_edges == 0 { "—".to_string() } else { s.n_edges.to_string() };
    let c = if s.n_communities == 0 {
        "—".to_string()
    } else {
        s.n_communities.to_string()
    };
    ui.label(egui::RichText::new(format!("nodes       {n}")).monospace());
    ui.label(egui::RichText::new(format!("edges       {m}")).monospace());
    ui.label(egui::RichText::new(format!("communities {c}")).monospace());
    ui.label(
        egui::RichText::new(format!("samples     {}", perf.len()))
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
    let plot_x_max = perf.samples().last().map(|s| s.t).unwrap_or(0.0);
    let plot_x_min = (plot_x_max - CHART_WINDOW_SECS).max(0.0);
    Plot::new(plot_id)
        .width(ui.available_width())
        .height(110.0)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .show_axes([false, true])
        .legend(
            Legend::default()
                .background_alpha(0.4)
                .position(Corner::LeftTop)
                .text_style(egui::TextStyle::Small),
        )
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

    ui.add_space(4.0);
    for stage in StageId::ALL {
        let stats = PerfCollector::stats(perf.samples().map(|s| s.stages[stage.idx()]));
        let [r, g, b] = stage.color();
        ui.horizontal(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(stage.label())
                        .size(10.0)
                        .color(egui::Color32::from_gray(180)),
                )
                .truncate(),
            );
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(ms_stats_label(stats))
                    .monospace()
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
            )
            .wrap(),
        );
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

    // Cheatsheet (relocated from sections/stats.rs).
    subgroup_separator(ui);
    subgroup_label(ui, "Cheatsheet");
    ui.add_space(4.0);
    let lines = [
        "WASD   pan",
        "Q/E    up / down",
        "F+drag focal plane",
        "LMB    attract",
        "RMB    repel",
        "Space  pause sim",
    ];
    for l in lines {
        ui.label(egui::RichText::new(l).monospace().size(11.0));
    }

    // Danger zone (relocated from sections/stats.rs).
    ui.add_space(20.0);
    ui.separator();
    ui.label(egui::RichText::new("Danger zone").color(accent::RED).small());
    ui.horizontal(|ui| {
        if ui
            .button(egui::RichText::new("Reset everything").color(accent::RED))
            .clicked()
        {
            *state = AppState::default();
        }
    });
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
        .width(ui.available_width())
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
    format!("avg {:.1} | p99 {:.1} | max {:.1}", avg, p99, max)
}

fn ms_stats_label((avg, p99, max): (f32, f32, f32)) -> String {
    format!("avg {:.2}ms | p99 {:.2}ms | max {:.2}ms", avg, p99, max)
}
