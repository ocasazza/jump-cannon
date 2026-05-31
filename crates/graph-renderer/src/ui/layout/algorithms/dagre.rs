//! UI factory + widgets for the `dagre` static layout.

use eframe::egui;
use graph_layouts::{
    BoxedStatic, DagreLayout, DagreRanker, DagreSettings, DynStaticLayout, RankDirection,
};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

pub fn factory() -> LayoutFactory {
    LayoutFactory::Static {
        descriptor: <DagreLayout as graph_layouts::StaticLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(DagreSettings::default()).unwrap_or(Value::Null)
}

fn build_layout() -> Box<dyn DynStaticLayout> {
    Box::new(BoxedStatic::<DagreLayout>::new())
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: DagreSettings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| DagreSettings::default());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("rank direction");
        egui::ComboBox::from_id_salt("dagre_rank_direction")
            .selected_text(match s.rank_direction {
                RankDirection::TB => "Top → Bottom",
                RankDirection::BT => "Bottom → Top",
                RankDirection::LR => "Left → Right",
                RankDirection::RL => "Right → Left",
            })
            .show_ui(ui, |ui| {
                for (variant, label) in [
                    (RankDirection::TB, "Top → Bottom"),
                    (RankDirection::BT, "Bottom → Top"),
                    (RankDirection::LR, "Left → Right"),
                    (RankDirection::RL, "Right → Left"),
                ] {
                    if ui
                        .selectable_value(&mut s.rank_direction, variant, label)
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        ui.label("ranker");
        egui::ComboBox::from_id_salt("dagre_ranker")
            .selected_text(match s.ranker {
                DagreRanker::NetworkSimplex => "Network simplex",
                DagreRanker::TightTree => "Tight tree",
                DagreRanker::LongestPath => "Longest path",
            })
            .show_ui(ui, |ui| {
                for (variant, label) in [
                    (DagreRanker::NetworkSimplex, "Network simplex"),
                    (DagreRanker::TightTree, "Tight tree"),
                    (DagreRanker::LongestPath, "Longest path"),
                ] {
                    if ui.selectable_value(&mut s.ranker, variant, label).changed() {
                        changed = true;
                    }
                }
            });
    });

    if ui
        .add(egui::Slider::new(&mut s.rank_separation, 10.0..=300.0).text("rank separation"))
        .changed()
    {
        changed = true;
    }

    if ui
        .add(egui::Slider::new(&mut s.node_separation, 10.0..=300.0).text("node separation"))
        .changed()
    {
        changed = true;
    }

    if ui.checkbox(&mut s.acyclic, "acyclic (break cycles)").changed() {
        changed = true;
    }

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
