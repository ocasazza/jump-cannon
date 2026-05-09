//! UI factory + widgets for the `hilbert` static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, HilbertLayout, HilbertSettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

fn row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        contents(ui);
    });
}

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <HilbertLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(HilbertSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<HilbertLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: HilbertSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| HilbertSettings::default());
    let mut changed = false;

    row(ui, "extent", |ui| {
        if ui.add(egui::Slider::new(&mut s.extent, 10.0..=10_000.0)).changed() {
            changed = true;
        }
    });

    row(ui, "order", |ui| {
        if ui.add(egui::DragValue::new(&mut s.order).range(1..=10)).changed() {
            changed = true;
        }
    });

    row(ui, "flatten", |ui| {
        if ui.checkbox(&mut s.flatten, "").changed() {
            changed = true;
        }
    });

    row(ui, "center", |ui| {
        if ui.checkbox(&mut s.center, "").changed() {
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
