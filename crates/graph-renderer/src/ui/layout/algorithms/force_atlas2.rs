//! UI factory + widgets for the `force_atlas2` physics layout (skeleton).
//!
//! Mirrors the `gpu_force` factory shape. The underlying layout is a
//! no-op stub; this UI exists so the registration plumbing is exercised
//! and future force-model work has a place to plug in.

use eframe::egui;
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, ForceAtlas2Layout, ForceAtlas2Settings, PhysicsLayout,
};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Physics {
        descriptor: <ForceAtlas2Layout as graph_layouts::PhysicsLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(ForceAtlas2Settings::default()).unwrap_or(Value::Null)
}

fn build_layout(json: &Value) -> Box<dyn DynPhysicsLayout> {
    let s: ForceAtlas2Settings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| ForceAtlas2Settings::default());
    Box::new(BoxedPhysics::new(ForceAtlas2Layout::new(s)))
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: ForceAtlas2Settings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| ForceAtlas2Settings::default());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("gravity");
        if ui
            .add(egui::Slider::new(&mut s.gravity, 0.0..=10.0))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("strong_gravity");
        if ui.checkbox(&mut s.strong_gravity, "").changed() {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("scaling_ratio");
        if ui
            .add(egui::Slider::new(&mut s.scaling_ratio, 0.1..=100.0).logarithmic(true))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("edge_weight_influence");
        if ui
            .add(egui::Slider::new(&mut s.edge_weight_influence, 0.0..=2.0))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("jitter_tolerance");
        if ui
            .add(egui::Slider::new(&mut s.jitter_tolerance, 0.0..=10.0))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("lin_log_mode");
        if ui.checkbox(&mut s.lin_log_mode, "").changed() {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("prevent_overlap");
        if ui.checkbox(&mut s.prevent_overlap, "").changed() {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("barnes_hut");
        if ui.checkbox(&mut s.barnes_hut, "").changed() {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("barnes_hut_theta");
        if ui
            .add(egui::Slider::new(&mut s.barnes_hut_theta, 0.0..=2.0))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("time_step");
        if ui
            .add(egui::Slider::new(&mut s.time_step, 0.01..=10.0).logarithmic(true))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("steps_per_frame");
        if ui
            .add(egui::DragValue::new(&mut s.steps_per_frame).range(1..=64))
            .changed()
        {
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
