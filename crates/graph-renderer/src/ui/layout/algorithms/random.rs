//! UI factory + widgets for the `random` static layout.
//!
//! Step 3 of the layout abstraction. Renders sliders for the seed and
//! radius and round-trips the JSON-encoded `RandomSettings` block back
//! into the layout settings map.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, RandomLayout, RandomSettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <RandomLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(RandomSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<RandomLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: RandomSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| RandomSettings::default());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("seed");
        let mut seed_val = s.seed;
        let resp = ui.add(egui::DragValue::new(&mut seed_val).speed(1.0));
        if resp.changed() {
            s.seed = seed_val;
            changed = true;
        }
        if ui.small_button("re-roll").clicked() {
            // Cheap hash-step: keeps the value deterministic but visibly
            // distinct after each click.
            s.seed = s.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            changed = true;
        }
    });

    if ui
        .add(egui::Slider::new(&mut s.radius, 1.0..=2000.0).text("radius"))
        .changed()
    {
        changed = true;
    }

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
