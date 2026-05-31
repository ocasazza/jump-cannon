//! UI factory + widgets for the `klay` (Layered KLay) static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, DynStaticLayout, KlayLayout, KlaySettings};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <KlayLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(KlaySettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<KlayLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: KlaySettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| KlaySettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.layer_spacing, 10.0..=300.0).text("layer_spacing"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.node_spacing, 10.0..=300.0).text("node_spacing"))
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
