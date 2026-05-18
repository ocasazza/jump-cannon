//! Collapsible status footer at the bottom of the egui app.
//!
//! - Collapsed: ~24px strip with the latest active task + a "running N"
//!   badge on the right and a chevron to expand.
//! - Expanded: list of active tasks (each with label + spinner /
//!   determinate bar) above a scrollable log buffer.
//!
//! Driven by [`crate::ui::progress::Progress`]. The footer never mutates
//! progress data except via `clear_log` triggered from its own button.

use eframe::egui;

use crate::ui::progress::{LogLevel, Progress, TaskStatus};
use crate::ui::state::AppState;
use crate::ui::theme::{self, palette};

/// Per-line height for the collapsed strip and each active-task row.
const ROW_H: f32 = 18.0;
/// Default expanded panel height (resizable by the user).
const EXPANDED_DEFAULT_H: f32 = 240.0;
/// Collapsed panel height — one row plus a hair of vertical padding.
const COLLAPSED_H: f32 = 24.0;

/// Render the status footer panel.
///
/// Mutates `state.status_footer_open` (toggled by the chevron) and
/// `progress` (only via `clear_log` from the explicit button).
pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    progress: &mut Progress,
) {
    // One-shot first-paint ping. The headless regression suite reads
    // this to assert the footer mounts on boot — guards against the
    // panel being hidden under another panel via stacking-order bugs.
    static FOOTER_LOGGED: std::sync::Once = std::sync::Once::new();
    FOOTER_LOGGED.call_once(|| {
        log::info!("[graph-renderer] status footer mounted");
    });
    let panel = egui::TopBottomPanel::bottom("status-footer")
        .resizable(*open)
        .show_separator_line(true);
    let panel = if *open {
        panel
            .min_height(60.0)
            .default_height(EXPANDED_DEFAULT_H)
            .max_height(480.0)
    } else {
        panel.exact_height(COLLAPSED_H)
    };
    panel.show(ctx, |ui| {
        if *open {
            draw_expanded(ui, open, progress);
        } else {
            draw_collapsed(ui, open, progress);
        }
    });
}

/// Sticky Windows-taskbar-style tray strip across the bottom of the window.
///
/// Renders a small chip per collapsed `PanelId` (left-aligned, preserving
/// `state.tray.collapsed` insertion order). Clicking a chip restores that
/// panel. The right edge carries a compact running-task indicator: a
/// spinner + "running N" when any task is in progress, otherwise a tiny
/// grey dot.
pub fn show_tray(ctx: &egui::Context, state: &mut AppState, progress: &Progress) {
    let mut frame = theme::floating_frame();
    frame.inner_margin = egui::Margin::symmetric(8.0, 4.0);
    egui::TopBottomPanel::bottom("tray-strip")
        .resizable(false)
        .show_separator_line(false)
        .exact_height(26.0)
        .frame(frame)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Left: one chip per collapsed panel. Snapshot the list so
                // the loop body can mutate `state.tray` on click without
                // aliasing.
                let chips: Vec<_> = state.tray.collapsed.clone();
                for id in chips {
                    let label = egui::RichText::new(id.label())
                        .size(10.0)
                        .color(palette::TEXT);
                    let btn = egui::Button::new(label)
                        .small()
                        .stroke(egui::Stroke::new(1.0, palette::BORDER))
                        .fill(egui::Color32::TRANSPARENT);
                    if ui.add(btn).clicked() {
                        state.tray.restore(id);
                    }
                }

                // Right: running indicator.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let n_running = progress.in_progress().count();
                        if n_running > 0 {
                            ui.label(
                                egui::RichText::new(format!("running {n_running}"))
                                    .size(10.0)
                                    .color(palette::TEXT),
                            );
                            ui.add(egui::Spinner::new().size(10.0));
                        } else {
                            ui.label(
                                egui::RichText::new("●")
                                    .size(10.0)
                                    .color(palette::GREY),
                            );
                        }
                    },
                );
            });
        });
}

fn draw_collapsed(ui: &mut egui::Ui, open: &mut bool, progress: &Progress) {
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        // Left: latest active task or idle marker.
        let latest = progress.in_progress().last();
        if let Some(task) = latest {
            ui.add(egui::Spinner::new().size(10.0));
            ui.label(
                egui::RichText::new(format!("{} › {}", task.group, task.label))
                    .small()
                    .color(palette::TEXT),
            );
        } else {
            ui.label(
                egui::RichText::new("●")
                    .small()
                    .color(palette::GREY),
            );
            ui.label(
                egui::RichText::new(format!("idle • {} entries", progress.log_len()))
                    .small()
                    .color(palette::GREY),
            );
        }

        // Right: running badge + expand chevron.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("▴")
                .on_hover_text("Expand status panel")
                .clicked()
            {
                *open = true;
            }
            let n_running = progress.in_progress().count();
            if n_running > 0 {
                ui.label(
                    egui::RichText::new(format!("{n_running} running"))
                        .small()
                        .color(palette::INFO),
                );
            }
        });
    });
}

fn draw_expanded(ui: &mut egui::Ui, open: &mut bool, progress: &mut Progress) {
    // Header: title + clear-log + collapse chevron.
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Status")
                .strong()
                .color(palette::WHITE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("▾")
                .on_hover_text("Collapse status panel")
                .clicked()
            {
                *open = false;
            }
            if ui.small_button("Clear log").clicked() {
                progress.clear_log();
            }
        });
    });

    ui.separator();

    // Active tasks.
    let active: Vec<_> = progress.active().cloned().collect();
    if active.is_empty() {
        ui.label(
            egui::RichText::new("no active tasks")
                .small()
                .color(palette::GREY),
        );
    } else {
        for task in &active {
            ui.horizontal(|ui| {
                ui.set_min_height(ROW_H);
                let (glyph, color) = match &task.status {
                    TaskStatus::InProgress => ("⠿", palette::INFO),
                    TaskStatus::Done => ("✓", palette::GOOD),
                    TaskStatus::Failed(_) => ("✗", palette::BAD),
                };
                ui.label(egui::RichText::new(glyph).color(color).small());
                let label = format!("{} › {}", task.group, task.label);
                ui.label(egui::RichText::new(label).small().color(palette::WHITE));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let elapsed = task.elapsed();
                    let elapsed_txt = if elapsed.as_millis() < 1000 {
                        format!("{}ms", elapsed.as_millis())
                    } else {
                        format!("{:.2}s", elapsed.as_secs_f32())
                    };
                    ui.label(
                        egui::RichText::new(elapsed_txt)
                            .small()
                            .color(palette::GREY),
                    );
                    let bar_w = (ui.available_width() - 8.0).max(60.0);
                    if let Some(p) = task.progress {
                        let bar = egui::ProgressBar::new(p)
                            .desired_width(bar_w)
                            .show_percentage();
                        ui.add(bar);
                    } else if matches!(task.status, TaskStatus::InProgress) {
                        // Indeterminate: use an unfilled bar + spinner so
                        // the row aligns with determinate ones below it.
                        ui.add(egui::Spinner::new().size(12.0));
                    }
                });
            });
        }
    }

    ui.separator();

    // Scrollable log — newest at the bottom (console-style).
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for line in progress.log() {
                let color = match line.level {
                    LogLevel::Info => palette::GREY,
                    LogLevel::Warn => palette::WARNING,
                    LogLevel::Error => palette::BAD,
                };
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("[{}]", line.group))
                            .small()
                            .monospace()
                            .color(palette::INFO),
                    );
                    ui.label(
                        egui::RichText::new(&line.message)
                            .small()
                            .monospace()
                            .color(color),
                    );
                });
            }
        });
}
