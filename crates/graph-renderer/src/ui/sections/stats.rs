use eframe::egui;

use crate::ui::state::{AppState, SimStatus};
use crate::ui::theme::accent;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
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
    ui.label(egui::RichText::new("nodes      —").monospace());
    ui.label(egui::RichText::new("edges      —").monospace());
    ui.label(egui::RichText::new("communities —").monospace());

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new("CHEATSHEET")
            .size(10.0)
            .color(egui::Color32::from_gray(150)),
    );
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
}
