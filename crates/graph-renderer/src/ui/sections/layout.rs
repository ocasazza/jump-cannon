use eframe::egui;

use crate::ui::state::{AppState, LayoutPreset, RepulsionMode};
use crate::ui::theme::accent;

use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            let preset = state.layout.preset;
            state.layout = Default::default();
            preset.apply_to(&mut state.layout);
        }
    });
    // Preset buttons row.
    subgroup_label(ui, "Preset");
    ui.add_space(4.0);
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

    subgroup_separator(ui);

    // Physics parameters.
    subgroup_label(ui, "Physics");
    ui.add_space(4.0);
    let l = &mut state.layout;
    ui.add(egui::Slider::new(&mut l.repulsion, 0.0..=4000.0).text("repulsion"));
    ui.add(egui::Slider::new(&mut l.spring_k, 0.0..=1.0).text("spring_k"));
    ui.add(egui::Slider::new(&mut l.spring_len, 1.0..=400.0).text("spring_len"));
    ui.add(egui::Slider::new(&mut l.gravity, 0.0..=1.0).text("gravity"));
    ui.add(egui::Slider::new(&mut l.damping, 0.0..=1.0).text("damping"));
    ui.add(egui::Slider::new(&mut l.dt, 0.001..=0.1).text("dt"));
    ui.add(egui::Slider::new(&mut l.steps_per_call, 1.0..=32.0).text("steps/call"));

    subgroup_separator(ui);

    // Cooling group.
    subgroup_label(ui, "Cooling");
    hint_label(ui, "Drives sim toward steady state");
    ui.add_space(4.0);
    ui.add(egui::Slider::new(&mut l.cooling_alpha, 0.9..=1.0).text("cooling α"));
    ui.add(egui::Slider::new(&mut l.cooling_floor, 0.0..=1.0).text("cooling floor"));

    subgroup_separator(ui);

    // Auto-halt group.
    subgroup_label(ui, "Auto-halt");
    hint_label(ui, "Stop dispatching when truly settled");
    ui.add_space(4.0);
    ui.add(
        egui::Slider::new(&mut l.energy_threshold, 0.0..=1.0)
            .text("energy halt threshold"),
    );

    subgroup_separator(ui);

    // Repulsion backend toggle. Default Grid; BarnesHut wins on
    // clustered graphs ≥50k; NegativeSampling skips spatial structure
    // entirely (best paired with multilevel coarsening).
    subgroup_label(ui, "Repulsion backend");
    hint_label(ui, "Grid: dense small; BH: clustered; NS: huge");
    ui.add_space(4.0);
    egui::ComboBox::from_id_salt("repulsion-mode")
        .selected_text(l.repulsion_mode.label())
        .show_ui(ui, |ui| {
            for mode in RepulsionMode::ALL {
                ui.selectable_value(&mut l.repulsion_mode, *mode, mode.label());
            }
        });
    if matches!(l.repulsion_mode, RepulsionMode::NegativeSampling) {
        ui.add_space(4.0);
        // K — DRGraph reports good convergence at K in [5, 20]; default 8.
        ui.add(egui::Slider::new(&mut l.repulsion_samples, 1..=32).text("K samples"));
    }
}
