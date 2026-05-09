//! UI factory + widgets for the `sphere` (Fibonacci) static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, SphereLayout, SphereSettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <SphereLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(SphereSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<SphereLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: SphereSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| SphereSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.radius, 1.0..=2000.0).text("radius"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.jitter, 0.0..=1.0).text("jitter"))
        .changed()
    {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("seed");
        let mut seed_val = s.seed;
        let resp = ui.add(egui::DragValue::new(&mut seed_val).speed(1.0));
        if resp.changed() {
            s.seed = seed_val;
            changed = true;
        }
        if ui.small_button("re-roll").clicked() {
            s.seed = s.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
