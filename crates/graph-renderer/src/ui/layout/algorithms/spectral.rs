//! UI factory + widgets for the `spectral` (Fiedler) static seed layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, SpectralLayout, SpectralSettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <SpectralLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(SpectralSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<SpectralLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: SpectralSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| SpectralSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.radius, 1.0..=2000.0).text("radius"))
        .changed()
    {
        changed = true;
    }
    if ui
        .add(egui::Slider::new(&mut s.iterations, 10..=1000).text("iterations"))
        .on_hover_text("Power-iteration steps per Fiedler axis. Clustered graphs converge in few.")
        .changed()
    {
        changed = true;
    }
    if ui
        .checkbox(&mut s.three_d, "3D (third Fiedler axis)")
        .changed()
    {
        changed = true;
    }

    ui.label(
        egui::RichText::new("Seed only — follow with a force/geometric layout to refine.")
            .small()
            .weak(),
    );

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
