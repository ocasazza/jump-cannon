//! UI factory + widgets for the `grid` static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, GridLayout, GridSettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <GridLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(GridSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<GridLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: GridSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| GridSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.spacing, 1.0..=500.0).text("spacing"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.aspect, 0.25..=4.0).text("aspect"))
        .changed()
    {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("layers");
        if ui
            .add(egui::DragValue::new(&mut s.layers).range(1..=32).speed(1.0))
            .changed()
        {
            changed = true;
        }
    });

    if ui.checkbox(&mut s.center, "center").changed() {
        changed = true;
    }

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
