//! UI factory + widgets for the `cise` static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, CiseLayout, CiseSettings, DynStaticLayout};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <CiseLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(CiseSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<CiseLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: CiseSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| CiseSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.circle_spacing, 1.0..=200.0).text("circle spacing"))
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
