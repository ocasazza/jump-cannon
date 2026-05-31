//! UI factory + widgets for the `fcose` (force-directed) static layout.

use eframe::egui;
use graph_layouts::{
    BoxedStatic, DynStaticLayout, FcoseLayout, FcoseQuality, FcoseSettings,
};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <FcoseLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(FcoseSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<FcoseLayout>::new())
}

const QUALITIES: &[(FcoseQuality, &str)] = &[
    (FcoseQuality::Draft, "Draft"),
    (FcoseQuality::Default, "Default"),
    (FcoseQuality::Proof, "Proof"),
];

fn quality_label(q: FcoseQuality) -> &'static str {
    QUALITIES
        .iter()
        .find(|(x, _)| *x == q)
        .map(|(_, l)| *l)
        .unwrap_or("Default")
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: FcoseSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| FcoseSettings::default());
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

    if ui
        .add(egui::Slider::new(&mut s.node_overlap, 0.0..=100.0).text("node overlap"))
        .changed()
    {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("quality");
        let mut quality = s.quality;
        egui::ComboBox::from_id_salt("fcose-quality")
            .selected_text(quality_label(quality))
            .show_ui(ui, |ui| {
                for (q, label) in QUALITIES {
                    if ui.selectable_label(quality == *q, *label).clicked() {
                        quality = *q;
                    }
                }
            });
        if quality != s.quality {
            s.quality = quality;
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
