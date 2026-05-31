//! UI factory + widgets for the `cose_bilkent` static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, CoseBilkentLayout, CoseBilkentSettings, DynStaticLayout};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <CoseBilkentLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(CoseBilkentSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<CoseBilkentLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: CoseBilkentSettings = serde_json::from_value(json.clone())
        .unwrap_or_else(|_| CoseBilkentSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.node_repulsion, 100.0..=10000.0).text("node repulsion"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.ideal_edge_length, 10.0..=300.0).text("ideal edge length"))
        .changed()
    {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("iterations");
        if ui
            .add(
                egui::DragValue::new(&mut s.iterations)
                    .range(1..=2000)
                    .speed(1.0),
            )
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
