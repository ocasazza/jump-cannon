//! UI factory + widgets for the `gpu-force` physics layout.
//!
//! Owns the JSON-serialised `GpuForceOptions` block and the egui widgets
//! that mutate it. Mirrors the pre-refactor layout sidebar verbatim so
//! Step 1 ships zero behavioural change.

use eframe::egui;
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, GpuForceLayout, GpuForceOptions, RepulsionMode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;
use crate::ui::sections::{hint_label, subgroup_label, subgroup_separator};
use crate::ui::theme::accent;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Physics {
        descriptor: <GpuForceLayout as graph_layouts::PhysicsLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(GpuForceOptions::default()).unwrap_or(Value::Null)
}

fn build_layout(json: &Value) -> Box<dyn DynPhysicsLayout> {
    let opts: GpuForceOptions = serde_json::from_value(json.clone())
        .unwrap_or_else(|_| GpuForceOptions::default());
    Box::new(BoxedPhysics::new(GpuForceLayout::new(opts)))
}

// ---- Preset buttons --------------------------------------------------------

/// Canonical slider presets. Mirrors the pre-refactor `LayoutPreset` from
/// `ui/state.rs`. Now mutates `GpuForceOptions` directly so the values
/// flow back through the JSON-keyed settings map.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LayoutPreset {
    Fast,
    #[default]
    Balanced,
    Pretty,
}

impl LayoutPreset {
    pub fn label(self) -> &'static str {
        match self {
            LayoutPreset::Fast => "Fast",
            LayoutPreset::Balanced => "Balanced",
            LayoutPreset::Pretty => "Pretty",
        }
    }

    pub fn apply_to(self, o: &mut GpuForceOptions) {
        match self {
            LayoutPreset::Fast => {
                o.repulsion = 150.0;
                o.spring_k = 0.10;
                o.spring_len = 40.0;
                o.gravity = 0.005;
                o.damping = 0.85;
                o.dt = 0.045;
                o.steps_per_call = 1;
                o.cooling_alpha = 0.99;
                o.cooling_floor = 0.65;
                o.energy_threshold = 0.5;
            }
            LayoutPreset::Balanced => {
                o.repulsion = 250.0;
                o.spring_k = 0.06;
                o.spring_len = 60.0;
                o.gravity = 0.003;
                o.damping = 0.92;
                o.dt = 0.04;
                o.steps_per_call = 2;
                o.cooling_alpha = 0.999;
                o.cooling_floor = 0.85;
                o.energy_threshold = 0.005;
            }
            LayoutPreset::Pretty => {
                o.repulsion = 400.0;
                o.spring_k = 0.05;
                o.spring_len = 80.0;
                o.gravity = 0.002;
                o.damping = 0.92;
                o.dt = 0.025;
                o.steps_per_call = 4;
                o.cooling_alpha = 0.999;
                o.cooling_floor = 0.55;
                o.energy_threshold = 0.02;
            }
        }
    }

    /// Best-effort guess of which preset produced this options block.
    /// Used purely for highlighting the active preset button.
    pub fn detect(o: &GpuForceOptions) -> Option<Self> {
        for p in [LayoutPreset::Fast, LayoutPreset::Balanced, LayoutPreset::Pretty] {
            let mut probe = GpuForceOptions::default();
            p.apply_to(&mut probe);
            if (probe.repulsion - o.repulsion).abs() < 0.001
                && (probe.spring_k - o.spring_k).abs() < 0.0001
                && (probe.spring_len - o.spring_len).abs() < 0.001
                && (probe.dt - o.dt).abs() < 0.0001
                && probe.steps_per_call == o.steps_per_call
            {
                return Some(p);
            }
        }
        None
    }
}

// ---- Repulsion combo helpers ----------------------------------------------

const REPULSION_MODES: &[(RepulsionMode, &str)] = &[
    (RepulsionMode::Grid, "Grid (27-cell)"),
    (RepulsionMode::BarnesHut, "Barnes-Hut"),
    (RepulsionMode::NegativeSampling, "Negative sampling"),
];

fn repulsion_mode_label(m: RepulsionMode) -> &'static str {
    REPULSION_MODES
        .iter()
        .find(|(mode, _)| *mode == m)
        .map(|(_, l)| *l)
        .unwrap_or("Grid (27-cell)")
}

// ---- Sidebar widgets -------------------------------------------------------

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut opts: GpuForceOptions = serde_json::from_value(json.clone())
        .unwrap_or_else(|_| GpuForceOptions::default());
    let mut changed = false;

    // Reset row — mirrors pre-refactor behaviour: reset to defaults then
    // re-apply the currently active preset.
    let active_preset = LayoutPreset::detect(&opts).unwrap_or_default();
    ui.horizontal(|ui| {
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            opts = GpuForceOptions::default();
            active_preset.apply_to(&mut opts);
            changed = true;
        }
    });

    // Preset row.
    subgroup_label(ui, "Preset");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for preset in [LayoutPreset::Fast, LayoutPreset::Balanced, LayoutPreset::Pretty] {
            let active = active_preset == preset;
            let mut text = egui::RichText::new(preset.label());
            if active {
                text = text.color(accent::GREEN);
            }
            let btn = egui::Button::new(text).stroke(if active {
                egui::Stroke::new(1.0, accent::GREEN)
            } else {
                egui::Stroke::new(1.0, egui::Color32::WHITE)
            });
            if ui.add(btn).clicked() {
                preset.apply_to(&mut opts);
                changed = true;
            }
        }
    });

    subgroup_separator(ui);

    // Physics sliders.
    subgroup_label(ui, "Physics");
    ui.add_space(4.0);
    let mut steps_f = opts.steps_per_call as f32;
    let mut samples_f = opts.repulsion_samples as f32;
    // Log scale on the magnitude knobs (repulsion / spring_len / spring_k /
    // gravity) so big graphs that need 4000+ repulsion / 400+ spring_len
    // are reachable from the same slider that small graphs use at 50 / 30.
    let resps = [
        ui.add(egui::Slider::new(&mut opts.repulsion, 0.1..=100_000.0).logarithmic(true).text("repulsion")),
        ui.add(egui::Slider::new(&mut opts.spring_k, 0.0001..=10.0).logarithmic(true).text("spring_k")),
        ui.add(egui::Slider::new(&mut opts.spring_len, 1.0..=10_000.0).logarithmic(true).text("spring_len")),
        ui.add(egui::Slider::new(&mut opts.gravity, 0.00001..=1.0).logarithmic(true).text("gravity")),
        ui.add(egui::Slider::new(&mut opts.damping, 0.0..=1.0).text("damping")),
        ui.add(egui::Slider::new(&mut opts.dt, 0.0001..=1.0).logarithmic(true).text("dt")),
        ui.add(egui::Slider::new(&mut steps_f, 1.0..=64.0).text("steps/call")),
    ];
    for r in &resps {
        if r.changed() { changed = true; }
    }
    let new_steps = steps_f.round().max(1.0) as u32;
    if new_steps != opts.steps_per_call {
        opts.steps_per_call = new_steps;
        changed = true;
    }

    subgroup_separator(ui);

    // Cooling.
    subgroup_label(ui, "Cooling");
    hint_label(ui, "Drives sim toward steady state");
    ui.add_space(4.0);
    let r1 = ui.add(egui::Slider::new(&mut opts.cooling_alpha, 0.9..=1.0).text("cooling α"));
    let r2 = ui.add(egui::Slider::new(&mut opts.cooling_floor, 0.0..=1.0).text("cooling floor"));
    if r1.changed() || r2.changed() { changed = true; }

    subgroup_separator(ui);

    // Auto-halt.
    subgroup_label(ui, "Auto-halt");
    hint_label(ui, "Stop dispatching when truly settled");
    ui.add_space(4.0);
    if ui
        .add(egui::Slider::new(&mut opts.energy_threshold, 0.0..=1.0).text("energy halt threshold"))
        .changed()
    {
        changed = true;
    }

    subgroup_separator(ui);

    // Repulsion backend.
    subgroup_label(ui, "Repulsion backend");
    hint_label(ui, "Grid: dense small; BH: clustered; NS: huge");
    ui.add_space(4.0);
    let mut mode = opts.repulsion_mode;
    egui::ComboBox::from_id_salt("repulsion-mode")
        .selected_text(repulsion_mode_label(mode))
        .show_ui(ui, |ui| {
            for (m, label) in REPULSION_MODES {
                if ui.selectable_label(mode == *m, *label).clicked() {
                    mode = *m;
                }
            }
        });
    if mode != opts.repulsion_mode {
        opts.repulsion_mode = mode;
        changed = true;
    }
    if matches!(opts.repulsion_mode, RepulsionMode::NegativeSampling) {
        ui.add_space(4.0);
        if ui
            .add(egui::Slider::new(&mut samples_f, 1.0..=32.0).text("K samples"))
            .changed()
        {
            opts.repulsion_samples = samples_f.round().max(1.0) as u32;
            changed = true;
        }
    }

    if changed {
        if let Ok(v) = serde_json::to_value(&opts) {
            *json = v;
        }
    }
}
