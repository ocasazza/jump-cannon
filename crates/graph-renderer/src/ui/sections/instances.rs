//! Instances panel — read-only view of `ActionInstance`s recorded via
//! the command palette. Mirrors `archive/nuxt/components/actions/ActionCard.vue`
//! but render-only for now (re-execution + edit-in-place can come later
//! once we have a use case driving it).

use eframe::egui;

use crate::ui::actions::{ActionRegistry, ParamValue};
use crate::ui::state::AppState;

use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, _state: &mut AppState, registry: &mut ActionRegistry) {
    if registry.instances.is_empty() {
        hint_label(
            ui,
            "No action instances yet. Press Ctrl+P to open the command palette.",
        );
        return;
    }

    let mut to_remove: Vec<u64> = Vec::new();
    let instances = registry.instances.clone();
    for (idx, inst) in instances.iter().enumerate() {
        let title = registry
            .get(&inst.action_id)
            .map(|a| a.title.clone())
            .unwrap_or_else(|| inst.action_id.clone());

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&title).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    to_remove.push(inst.id);
                }
                ui.label(
                    egui::RichText::new(format!("#{}", inst.id))
                        .size(10.0)
                        .color(egui::Color32::GRAY),
                );
            });
        });

        if !inst.params.is_empty() {
            subgroup_label(ui, "Params");
            for (k, v) in &inst.params {
                ui.label(format!("{k}: {}", param_value_display(v)));
            }
        }

        if !inst.state.is_null() {
            subgroup_label(ui, "State");
            let pretty = serde_json::to_string_pretty(&inst.state)
                .unwrap_or_else(|_| inst.state.to_string());
            ui.add(
                egui::TextEdit::multiline(&mut pretty.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3),
            );
        }

        if idx + 1 < instances.len() {
            subgroup_separator(ui);
        }
    }

    for id in to_remove {
        registry.remove_instance(id);
    }
}

fn param_value_display(v: &ParamValue) -> String {
    match v {
        ParamValue::String(s) => format!("\"{s}\""),
        ParamValue::Number(n) => format!("{n}"),
        ParamValue::Boolean(b) => format!("{b}"),
        ParamValue::Selected(items) => format!("[{}]", items.join(", ")),
    }
}
