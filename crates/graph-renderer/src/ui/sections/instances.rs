//! Instances panel — read-only view of `ActionInstance`s recorded via
//! the command palette. Mirrors `archive/nuxt/components/actions/ActionCard.vue`
//! but render-only for now (re-execution + edit-in-place can come later
//! once we have a use case driving it).

use eframe::egui;

use crate::ui::actions::{ActionRegistry, ParamValue};
use crate::ui::state::{self, AppState};

use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, registry: &mut ActionRegistry) {
    yaml_io_panel(ui, state);
    subgroup_separator(ui);

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

/// Import / Export YAML sub-region. Lives at the top of the Instances
/// section so the user can dump every UI setting as YAML and paste it
/// back in. The full `AppState` is round-tripped (every Serialize field),
/// not just `action_instances`.
fn yaml_io_panel(ui: &mut egui::Ui, state: &mut AppState) {
    subgroup_label(ui, "Import / Export YAML");

    // ---- Export row ------------------------------------------------------
    ui.horizontal(|ui| {
        if ui.button("Export").clicked() {
            match state::export_state_yaml(state) {
                Ok(s) => state.yaml_export_buffer = s,
                Err(e) => state.yaml_export_buffer = format!("# export error: {e}"),
            }
        }
        let has_export = !state.yaml_export_buffer.is_empty();
        if ui
            .add_enabled(has_export, egui::Button::new("Copy"))
            .clicked()
        {
            let yaml = state.yaml_export_buffer.clone();
            ui.output_mut(|o| o.copied_text = yaml);
        }
        if ui
            .add_enabled(has_export, egui::Button::new("✕"))
            .on_hover_text("Clear export buffer")
            .clicked()
        {
            state.yaml_export_buffer.clear();
        }
    });

    if !state.yaml_export_buffer.is_empty() {
        ui.add(
            egui::TextEdit::multiline(&mut state.yaml_export_buffer.as_str())
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(12),
        );
    }

    ui.add_space(6.0);

    // ---- Import row ------------------------------------------------------
    subgroup_label(ui, "Paste YAML to import");
    ui.add(
        egui::TextEdit::multiline(&mut state.yaml_import_buffer)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(12)
            .hint_text("Paste an AppState YAML document here, then click Load."),
    );

    ui.horizontal(|ui| {
        let has_import = !state.yaml_import_buffer.trim().is_empty();
        if ui
            .add_enabled(has_import, egui::Button::new("Load"))
            .clicked()
        {
            match state::import_state_yaml(&state.yaml_import_buffer) {
                Ok(imported) => {
                    // Full replacement — including which panels are open,
                    // active section, query, etc. That's the "every
                    // setting" contract.
                    *state = imported;
                }
                Err(e) => {
                    state.yaml_import_error = Some(e);
                }
            }
        }
        if ui.button("Clear").clicked() {
            state.yaml_import_buffer.clear();
            state.yaml_import_error = None;
        }
    });

    if let Some(err) = &state.yaml_import_error {
        ui.colored_label(egui::Color32::from_rgb(220, 70, 70), format!("Parse error: {err}"));
    }

    ui.add_space(6.0);

    // ---- Reset to defaults (two-step) -----------------------------------
    ui.horizontal(|ui| {
        let (label, color) = if state.yaml_reset_armed {
            ("Confirm reset", egui::Color32::from_rgb(220, 70, 70))
        } else {
            ("Reset to defaults", egui::Color32::from_rgb(170, 60, 60))
        };
        let btn = egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).small())
            .fill(color);
        if ui.add(btn).clicked() {
            if state.yaml_reset_armed {
                *state = AppState::default();
            } else {
                state.yaml_reset_armed = true;
            }
        }
        if state.yaml_reset_armed && ui.small_button("Cancel").clicked() {
            state.yaml_reset_armed = false;
        }
    });
}

fn param_value_display(v: &ParamValue) -> String {
    match v {
        ParamValue::String(s) => format!("\"{s}\""),
        ParamValue::Number(n) => format!("{n}"),
        ParamValue::Boolean(b) => format!("{b}"),
        ParamValue::Selected(items) => format!("[{}]", items.join(", ")),
    }
}
