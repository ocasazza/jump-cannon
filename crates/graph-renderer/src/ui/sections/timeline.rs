//! Timeline section — buffer + scrub the running simulation's position history.
//!
//! The live sim's node positions are captured into a bounded delta-compressed
//! ring (`crate::timeline::FrameRing`) owned by `App`. This panel is the scrub
//! surface over that buffer:
//!
//! * a PLAY / PAUSE toggle (Live ↔ Paused),
//! * STEP-BACK / STEP-FORWARD buttons (one buffered frame at a time, paused),
//! * a scrub SLIDER over the buffered frame range,
//! * a frame-index + buffer-depth + memory readout,
//! * the capture knobs (ring depth, capture stride).
//!
//! The panel owns NO GPU access. Like the metrics/seed sections it mutates only
//! `state.timeline`; `App::update` reads `scrub` + the knobs each frame, fills
//! the ring, and — while paused — writes the selected buffered frame to the GPU
//! via `GraphPipelines::set_positions`. The `buffered_len` / `buffered_bytes`
//! fields are mirrored back from the App so the slider can be sized here.

use eframe::egui;

use super::super::state::AppState;
use super::{hint_label, row, subgroup_label, subgroup_separator};
use crate::timeline::ScrubState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let len = state.timeline.buffered_len;

    if len == 0 {
        hint_label(
            ui,
            "No frames buffered yet. Start a layout / simulation; its position \
             history streams into the ring and you can scrub it here.",
        );
        subgroup_separator(ui);
        knobs(ui, state);
        return;
    }

    let max_idx = len.saturating_sub(1);
    let paused = state.timeline.is_paused();

    // ── Transport controls ───────────────────────────────────────────────
    ui.horizontal(|ui| {
        // Play/Pause toggle. Live → show "⏸ Pause"; Paused → show "▶ Play".
        let toggle_label = if paused { "▶ Play" } else { "⏸ Pause" };
        if ui
            .button(toggle_label)
            .on_hover_text(if paused {
                "Resume live simulation"
            } else {
                "Pause and scrub the buffered history"
            })
            .clicked()
        {
            if paused {
                state.timeline.resume_live();
            } else {
                // Pause at the current head so the canvas holds this moment.
                state.timeline.pause_at(max_idx);
            }
        }

        // Step-back / step-forward only make sense while paused; pressing one
        // while live first pauses at the head, then steps.
        if ui
            .button("⏮ Step −")
            .on_hover_text("Step one buffered frame back")
            .clicked()
        {
            let cur = state.timeline.current_idx();
            state.timeline.pause_at(cur.saturating_sub(1));
        }
        if ui
            .button("Step + ⏭")
            .on_hover_text("Step one buffered frame forward")
            .clicked()
        {
            let cur = state.timeline.current_idx();
            if cur + 1 > max_idx {
                // Stepping past the head returns to live.
                state.timeline.resume_live();
            } else {
                state.timeline.pause_at(cur + 1);
            }
        }
    });

    ui.add_space(4.0);

    // ── Scrub slider ─────────────────────────────────────────────────────
    // While live the slider tracks the head (read-only-ish); dragging it pauses
    // and seeks. While paused it drives the paused index.
    let mut idx = state.timeline.current_idx().min(max_idx);
    row(ui, "Frame", |ui| {
        let resp = ui.add(egui::Slider::new(&mut idx, 0..=max_idx).integer());
        if resp.changed() {
            state.timeline.pause_at(idx);
        }
    });

    // ── Readout ──────────────────────────────────────────────────────────
    let shown = state.timeline.current_idx().min(max_idx);
    let status = match &state.timeline.scrub {
        ScrubState::Live => "live".to_string(),
        ScrubState::Paused { .. } => "paused".to_string(),
    };
    ui.monospace(format!(
        "frame {shown} / {max_idx}   ({status})",
    ));
    ui.monospace(format!(
        "buffer {len} / {}   ~{}",
        state.timeline.depth,
        human_bytes(state.timeline.buffered_bytes),
    ));

    subgroup_separator(ui);
    knobs(ui, state);
}

/// Capture knobs: ring depth + capture stride. Both are user parameters and
/// persist (see `TimelineState`).
fn knobs(ui: &mut egui::Ui, state: &mut AppState) {
    subgroup_label(ui, "Capture");
    row(ui, "Depth (frames)", |ui| {
        ui.add(egui::Slider::new(&mut state.timeline.depth, 30..=1000).integer());
    });
    row(ui, "Stride (every Nth)", |ui| {
        ui.add(egui::Slider::new(&mut state.timeline.stride, 1..=30).integer());
    });
    hint_label(
        ui,
        "Depth = how many frames the ring keeps; stride = capture every Nth \
         live frame. Raise stride (or lower depth) for large graphs — see the \
         memory budget in the timeline module.",
    );
}

/// Compact human-readable byte count for the readout.
fn human_bytes(b: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}
