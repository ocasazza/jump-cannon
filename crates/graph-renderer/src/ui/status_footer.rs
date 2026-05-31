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
/// Renders the launcher row: one icon per `Section` (matches the old
/// activity bar), then a divider, then a dedicated Filters icon.
/// Clicking an icon toggles the corresponding floating panel's open
/// state. Right edge carries the running-task indicator. The right-side
/// Inspector tray icon used to live next to Filters; it was removed
/// when the inspector collapsed into the unified click-promoted
/// anchored panel (expand/contract toggle in the panel header is the
/// new "open inspector" affordance).
pub fn show_tray(ctx: &egui::Context, state: &mut AppState, progress: &Progress) {
    use crate::ui::sidebar::draw_icon;
    use crate::ui::state::Section;

    let mut frame = theme::floating_frame();
    frame.inner_margin = egui::Margin::symmetric(8.0, 2.0);
    egui::TopBottomPanel::bottom("tray-strip")
        .resizable(false)
        .show_separator_line(false)
        .exact_height(28.0)
        .frame(frame)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for &section in Section::ALL {
                    let active = state.is_section_open(section);
                    if tray_icon_button(ui, |painter, rect, color| {
                        draw_icon(painter, rect, section, color);
                    }, active, section.title())
                    .clicked()
                    {
                        // Toggle + (if opening a Tiled panel) auto-snap.
                        // Closing a tiled panel here flips section_open
                        // to false; `sync_tree_with_open_state` next
                        // frame yanks it out of the tree.
                        let new_open = !active;
                        crate::ui::tiles::toggle_panel_with_snap(
                            state,
                            crate::ui::tiles::PaneKind::Section(section),
                            new_open,
                        );
                        // Tray-driven open moves focus to the new
                        // panel; tray-driven close drops focus back
                        // to the canvas (if it pointed at this panel).
                        let my_focus = if matches!(section, Section::Debug) {
                            crate::ui::state::FocusedPanel::Debug
                        } else {
                            crate::ui::state::FocusedPanel::Section(section)
                        };
                        if new_open {
                            state.focused_panel = Some(my_focus);
                        } else if state.focused_panel == Some(my_focus) {
                            state.focused_panel = None;
                        }
                    }
                }

                ui.add_space(8.0);

                if tray_icon_button(ui, draw_filter_icon, state.filter_strip_open, "Filters")
                    .clicked()
                {
                    let new_open = !state.filter_strip_open;
                    crate::ui::tiles::toggle_panel_with_snap(
                        state,
                        crate::ui::tiles::PaneKind::FilterStrip,
                        new_open,
                    );
                    let my_focus = crate::ui::state::FocusedPanel::FilterStrip;
                    if new_open {
                        state.focused_panel = Some(my_focus);
                    } else if state.focused_panel == Some(my_focus) {
                        state.focused_panel = None;
                    }
                }

                // View controls — right side. Within `right_to_left`,
                // widgets render rightmost-first, so the running
                // indicator comes first in the closure, then the
                // separator, then the canvas pop-out icon.
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

                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);

                        if tray_icon_button(
                            ui,
                            draw_popout_icon,
                            state.canvas_mount.is_floating(),
                            "Pop out graph canvas",
                        )
                        .clicked()
                        {
                            state.toggle_canvas_mount();
                        }

                        // Pinned-metrics HUD — always visible. Shows the user's
                        // pinned layout-quality metrics from the last computed
                        // snapshot (right_to_left: add separator first so it sits
                        // just left of the view-controls, then labels in reverse
                        // so they read left-to-right in pin order).
                        if !state.metrics.pinned.is_empty() {
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);
                            // Shared formatting on MetricKind (see state.rs);
                            // "—" when no snapshot or value yet.
                            let snap = state.metrics.last;
                            for m in state.metrics.pinned.iter().rev() {
                                let val =
                                    snap.map_or_else(|| "—".to_string(), |s| m.format_value(&s));
                                ui.label(
                                    egui::RichText::new(format!("{}: {}", m.label(), val))
                                        .size(10.0)
                                        .color(palette::TEXT),
                                );
                            }
                        }
                    },
                );
            });
        });
}

/// Compact 18×18 tray icon button (≈3/4 the previous 22 px footprint).
/// Caller supplies a paint closure that draws the icon glyph into the
/// **inner** rect (the button rect shrunk by 2 px so the glyph never
/// touches the border).
///
/// Color scheme:
/// - active = `palette::PRIMARY` background (the red highlight) +
///   white glyph — toggled panels POP against the tray.
/// - hovered = grey-40 background + white glyph.
/// - idle = transparent background + `palette::ICON` glyph.
///
/// Every state renders a 1 px `palette::BORDER` stroke around the
/// rounded button so the icon footprint reads as a discrete control
/// even when idle.
fn tray_icon_button(
    ui: &mut egui::Ui,
    paint: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
    active: bool,
    tooltip: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let hovered = response.hovered();
    let (bg, fg) = if active {
        (palette::PRIMARY, egui::Color32::WHITE)
    } else if hovered {
        (egui::Color32::from_gray(40), egui::Color32::WHITE)
    } else {
        (egui::Color32::TRANSPARENT, palette::ICON)
    };
    let painter = ui.painter();
    let corner = 3.0;
    painter.rect_filled(rect, corner, bg);
    painter.rect_stroke(rect, corner, egui::Stroke::new(1.0, palette::BORDER));
    // Inner glyph rect: 2 px inset on every side so the icon never
    // touches the border. draw_icon's offsets are absolute from the
    // rect center, so this just nudges the glyph inward.
    paint(painter, rect.shrink(2.0), fg);
    response.on_hover_text(tooltip)
}

fn draw_popout_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    // Window-pop-out glyph: a larger rect with a smaller rect overlapping
    // its top-right corner, suggesting "this becomes a window".
    let center = rect.center();
    let s = egui::Stroke::new(1.2, color);
    // Larger (background) rect, offset slightly down-left.
    let big = egui::Rect::from_center_size(
        egui::pos2(center.x - 2.0, center.y + 2.0),
        egui::vec2(11.0, 9.0),
    );
    painter.rect_stroke(big, 1.5, s);
    // Smaller (foreground) rect, offset up-right; punch a small notch
    // out of the big rect by overdrawing its bg first.
    let small = egui::Rect::from_center_size(
        egui::pos2(center.x + 3.0, center.y - 3.0),
        egui::vec2(9.0, 7.0),
    );
    // Clear under the small rect with the button bg so it reads as
    // "on top". Use a transparent overdraw — the button background is
    // already painted before this fn runs, so a simple stroke suffices.
    painter.rect_stroke(small, 1.5, s);
}

fn draw_filter_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    // Funnel glyph.
    let center = rect.center();
    let s = egui::Stroke::new(1.2, color);
    let tl = egui::pos2(center.x - 7.0, center.y - 6.0);
    let tr = egui::pos2(center.x + 7.0, center.y - 6.0);
    let ml = egui::pos2(center.x - 2.0, center.y + 1.0);
    let mr = egui::pos2(center.x + 2.0, center.y + 1.0);
    let bl = egui::pos2(center.x - 2.0, center.y + 7.0);
    let br = egui::pos2(center.x + 2.0, center.y + 7.0);
    painter.line_segment([tl, tr], s);
    painter.line_segment([tl, ml], s);
    painter.line_segment([tr, mr], s);
    painter.line_segment([ml, bl], s);
    painter.line_segment([mr, br], s);
    painter.line_segment([bl, br], s);
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
