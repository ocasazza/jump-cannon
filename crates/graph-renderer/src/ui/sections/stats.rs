use eframe::egui;

use crate::ui::state::{AppState, SimStatus};
use crate::ui::theme::accent;

use super::{subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Stats".into());
    // Status dot.
    let (dot_color, label) = match state.sim_status {
        SimStatus::Running => (accent::GREEN, "running"),
        SimStatus::Settled => (accent::YELLOW, "settled"),
        SimStatus::Error => (accent::RED, "error"),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, dot_color);
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .size(11.0)
                .color(egui::Color32::from_gray(180)),
        );
    });

    ui.add_space(10.0);
    let s = &state.stats;
    let n = if s.n_nodes == 0 { "—".to_string() } else { s.n_nodes.to_string() };
    let m = if s.n_edges == 0 { "—".to_string() } else { s.n_edges.to_string() };
    let c = if s.n_communities == 0 { "—".to_string() } else { s.n_communities.to_string() };
    ui.label(egui::RichText::new(format!("nodes       {n}")).monospace());
    ui.label(egui::RichText::new(format!("edges       {m}")).monospace());
    ui.label(egui::RichText::new(format!("communities {c}")).monospace());

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

    ui.add_space(20.0);
    ui.separator();
    ui.label(egui::RichText::new("Danger zone").color(accent::RED).small());
    ui.horizontal(|ui| {
        if ui.button(
            egui::RichText::new("Reset everything").color(accent::RED)
        ).clicked() {
            *state = AppState::default();
        }
    });
}
