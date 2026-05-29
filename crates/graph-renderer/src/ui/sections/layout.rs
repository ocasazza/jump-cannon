//! Layout sidebar.
//!
//! Step 1 of the layout abstraction: this section is now a thin shell
//! that picks an active layout from a `LayoutRegistry` and delegates
//! the slider rendering to the algorithm-specific UI fn keyed by id.

use eframe::egui;

use crate::ui::layout::registry::LayoutRegistry;
use crate::ui::state::AppState;
use crate::ui::theme::palette;

use super::{subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, registry: &LayoutRegistry) {
    state.snapshot_source = Some("Layout".into());
    // Algorithm picker. Step 1 only registers gpu-force, so the combo is
    // effectively a one-item list — but the wiring is here for Steps 2/3.
    let active_id = state.layout.active.clone();
    let active_label = registry
        .get(&active_id)
        .map(|f| f.descriptor().display_name)
        .unwrap_or("(unknown)");

    ui.horizontal(|ui| {
        ui.label("Algorithm:");
        egui::ComboBox::from_id_salt("layout-algo")
            .selected_text(active_label)
            .show_ui(ui, |ui| {
                for factory in registry.iter() {
                    let id = factory.descriptor().id;
                    if ui
                        .selectable_label(
                            id == active_id.as_str(),
                            factory.descriptor().display_name,
                        )
                        .clicked()
                    {
                        state.layout.active = id.to_string();
                    }
                }
            });
        ui.add_space(8.0);
        if ui.small_button("↺ Reset").clicked() {
            if let Some(factory) = registry.get(&state.layout.active) {
                state
                    .layout
                    .settings
                    .insert(state.layout.active.clone(), factory.default_settings());
            }
        }
    });

    subgroup_separator(ui);

    let id = state.layout.active.clone();
    if let Some(factory) = registry.get(&id) {
        let default_factory = || factory.default_settings();
        let static_id: graph_layouts::LayoutId = factory.descriptor().id;
        let json = state.layout.settings_for_mut(static_id, default_factory);
        factory.ui(ui, json);

        // Static layouts need an explicit Solve trigger — they don't
        // re-run on every frame the way physics does. Sidebar sets the
        // flag; `App::update` reads-and-clears it and dispatches.
        if matches!(factory.kind(), graph_layouts::LayoutKind::Static) {
            subgroup_separator(ui);
            ui.horizontal(|ui| {
                if ui.button("Solve").clicked() {
                    state.layout_solve_requested = true;
                }
            });
        }
        // Physics layouts: a Wake button gives the user a recovery
        // path when the auto-halt has triggered (KE under threshold
        // for HALT_FRAMES consecutive readbacks). Without this, the
        // only way to reignite a halted sim was to touch a non-cursor
        // slider, which forces `set_options` to call `wake()`. Hitting
        // the dedicated button is more discoverable and doesn't
        // require nudging an unrelated knob.
        if matches!(factory.kind(), graph_layouts::LayoutKind::Physics) {
            subgroup_separator(ui);
            ui.horizontal(|ui| {
                if ui
                    .button("Wake")
                    .on_hover_text(
                        "Re-energize the sim. Useful when the layout looks frozen \
                         (KE below energy_threshold → auto-halt fired).",
                    )
                    .clicked()
                {
                    // `layout_solve_requested` is a one-shot flag drained
                    // by `App::update`. For physics layouts the handler
                    // routes the flag into `pipes.wake_physics_layout()`,
                    // which forwards through the trait to the layout's
                    // `wake()` impl.
                    state.layout_solve_requested = true;
                }
            });
        }
    } else {
        ui.label(egui::RichText::new(
            "No layout registered for active id — pick one above.",
        ));
    }

    subgroup_separator(ui);
    show_remote_engine_picker(ui, state);
}

/// "Remote engine" picker — surfaces the engines advertised by the
/// graph-compute worker (via graph-api `/compute/engines`) and lets the
/// user switch which one drives the `/graph/layout/stream`. The actual
/// HTTP calls happen in `App::update`, which drains the one-shot flags on
/// `state.compute`; this fn is purely view + intent-raising.
fn show_remote_engine_picker(ui: &mut egui::Ui, state: &mut AppState) {
    subgroup_label(ui, "Remote engine");

    // Lazy first fetch: kick off exactly once when the section is first
    // rendered. Subsequent refreshes are user-driven (the ↻ button) — we
    // never poll every frame.
    if !state.compute.requested_once {
        state.compute.requested_once = true;
        state.compute.refresh_requested = true;
    }

    let snapshot = state.compute.current();

    ui.horizontal(|ui| {
        match &snapshot {
            Some(Ok(eng)) if eng.connected => {
                let active = eng.active.clone();
                let selected_label = eng
                    .engines
                    .iter()
                    .find(|e| e.id == active)
                    .map(|e| e.display_name.clone())
                    .unwrap_or_else(|| {
                        if active.is_empty() {
                            "(none)".to_string()
                        } else {
                            active.clone()
                        }
                    });

                egui::ComboBox::from_id_salt("compute-engine")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for e in &eng.engines {
                            let resp = ui
                                .selectable_label(e.id == active, &e.display_name)
                                .on_hover_text(if e.description.is_empty() {
                                    e.kind.clone()
                                } else {
                                    format!("{} — {}", e.kind, e.description)
                                });
                            if resp.clicked() && e.id != active {
                                state.compute.select = Some(e.id.clone());
                            }
                        }
                    });
            }
            Some(Ok(_)) => {
                // Reached graph-api, but the broker has no worker.
                ui.colored_label(palette::WARNING, "no compute worker");
            }
            Some(Err(_)) => {
                ui.colored_label(palette::BAD, "engines unavailable");
            }
            None => {
                ui.colored_label(palette::GREY, "loading…");
            }
        }

        ui.add_space(8.0);
        if ui
            .small_button("↻")
            .on_hover_text("Refresh remote engine list")
            .clicked()
        {
            state.compute.refresh_requested = true;
        }
    });

    super::hint_label(
        ui,
        "Drives the Remote (compute) layout's stream. Select the local \
         layout \"Remote (compute)\" above to view it.",
    );
}
