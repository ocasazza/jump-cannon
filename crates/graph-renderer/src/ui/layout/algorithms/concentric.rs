//! UI factory + widgets for the `concentric` (by-degree) static layout.

use eframe::egui;
use graph_layouts::{
    BoxedStatic, ConcentricLayout, ConcentricMetric, ConcentricSettings, DynStaticLayout,
};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <ConcentricLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(ConcentricSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<ConcentricLayout>::new())
}

const METRICS: &[(ConcentricMetric, &str)] = &[
    (ConcentricMetric::Degree, "Degree (in + out)"),
    (ConcentricMetric::InDegree, "In-degree"),
    (ConcentricMetric::OutDegree, "Out-degree"),
];

fn metric_label(m: ConcentricMetric) -> &'static str {
    METRICS
        .iter()
        .find(|(x, _)| *x == m)
        .map(|(_, l)| *l)
        .unwrap_or("Degree (in + out)")
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: ConcentricSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| ConcentricSettings::default());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("metric");
        let mut metric = s.metric;
        egui::ComboBox::from_id_salt("concentric-metric")
            .selected_text(metric_label(metric))
            .show_ui(ui, |ui| {
                for (m, label) in METRICS {
                    if ui.selectable_label(metric == *m, *label).clicked() {
                        metric = *m;
                    }
                }
            });
        if metric != s.metric {
            s.metric = metric;
            changed = true;
        }
    });

    if ui
        .add(egui::Slider::new(&mut s.min_radius, 1.0..=1000.0).text("min radius"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.level_spacing, 1.0..=500.0).text("level spacing"))
        .changed()
    {
        changed = true;
    }

    if ui.checkbox(&mut s.clockwise, "clockwise").changed() {
        changed = true;
    }

    ui.horizontal(|ui| {
        ui.label("buckets");
        let mut bc = s.bucket_count;
        let resp = ui.add(egui::DragValue::new(&mut bc).range(0..=64).speed(1.0));
        if resp.changed() {
            s.bucket_count = bc;
            changed = true;
        }
        ui.label(
            egui::RichText::new("(0 = distinct values)")
                .size(10.0)
                .italics(),
        );
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
