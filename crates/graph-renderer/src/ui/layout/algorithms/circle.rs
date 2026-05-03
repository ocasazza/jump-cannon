//! UI factory + widgets for the `circle` static layout.

use eframe::egui;
use graph_layouts::{BoxedStatic, CircleAxis, CircleLayout, CircleSettings, DynStaticLayout};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <CircleLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(CircleSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<CircleLayout>::new())
}

const AXES: &[(CircleAxis, &str)] = &[
    (CircleAxis::Z, "Z (xy plane)"),
    (CircleAxis::X, "X (yz plane)"),
    (CircleAxis::Y, "Y (xz plane)"),
];

fn axis_label(a: CircleAxis) -> &'static str {
    AXES.iter().find(|(x, _)| *x == a).map(|(_, l)| *l).unwrap_or("Z (xy plane)")
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: CircleSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| CircleSettings::default());
    let mut changed = false;

    if ui
        .add(egui::Slider::new(&mut s.radius, 1.0..=2000.0).text("radius"))
        .changed()
    {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("axis");
        let mut axis = s.axis;
        egui::ComboBox::from_id_salt("circle-axis")
            .selected_text(axis_label(axis))
            .show_ui(ui, |ui| {
                for (a, label) in AXES {
                    if ui.selectable_label(axis == *a, *label).clicked() {
                        axis = *a;
                    }
                }
            });
        if axis != s.axis {
            s.axis = axis;
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
