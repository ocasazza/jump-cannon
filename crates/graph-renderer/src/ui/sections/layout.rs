use eframe::egui;

use crate::ui::state::{AppState, LayoutPreset};
use crate::ui::theme::accent;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("LAYOUT");
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            let preset = state.layout.preset;
            state.layout = Default::default();
            preset.apply_to(&mut state.layout);
        }
    });
    ui.add_space(4.0);

    ui.label("Preset");
    ui.horizontal(|ui| {
        for (preset, label) in [
            (LayoutPreset::Fast, "Fast"),
            (LayoutPreset::Balanced, "Balanced"),
            (LayoutPreset::Pretty, "Pretty"),
        ] {
            let active = state.layout.preset == preset;
            let mut text = egui::RichText::new(label);
            if active {
                text = text.color(accent::GREEN);
            }
            let btn = egui::Button::new(text)
                .stroke(if active {
                    egui::Stroke::new(1.0, accent::GREEN)
                } else {
                    egui::Stroke::new(1.0, egui::Color32::WHITE)
                });
            if ui.add(btn).clicked() {
                preset.apply_to(&mut state.layout);
            }
        }
    });

    ui.add_space(10.0);
    let l = &mut state.layout;
    ui.add(egui::Slider::new(&mut l.repulsion, 0.0..=4000.0).text("repulsion"));
    ui.add(egui::Slider::new(&mut l.spring_k, 0.0..=1.0).text("spring_k"));
    ui.add(egui::Slider::new(&mut l.spring_len, 1.0..=400.0).text("spring_len"));
    ui.add(egui::Slider::new(&mut l.gravity, 0.0..=1.0).text("gravity"));
    ui.add(egui::Slider::new(&mut l.damping, 0.0..=1.0).text("damping"));
    ui.add(egui::Slider::new(&mut l.dt, 0.001..=0.1).text("dt"));
    ui.add(egui::Slider::new(&mut l.steps_per_call, 1.0..=32.0).text("steps/call"));
    ui.add_space(6.0);
    ui.label("Cooling — drives sim toward steady state");
    ui.add(egui::Slider::new(&mut l.cooling_alpha, 0.9..=1.0).text("cooling α"));
    ui.add(egui::Slider::new(&mut l.cooling_floor, 0.0..=1.0).text("cooling floor"));
    ui.add_space(6.0);
    ui.label("Auto-halt — stop dispatching when truly settled");
    ui.add(
        egui::Slider::new(&mut l.energy_threshold, 0.0..=1.0)
            .text("energy halt threshold"),
    );
}
