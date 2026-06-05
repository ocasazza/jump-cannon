//! Layout sidebar.
//!
//! A thin shell built around ONE unified "Engine" picker. The picker is
//! grouped into two sub-lists:
//!
//!   * **Local** — every non-bridge layout from the `LayoutRegistry`
//!     (the static solvers + the local GPU physics sim). Selecting one
//!     sets `state.layout.active` directly.
//!   * **Remote** — the engines advertised by the graph-compute worker
//!     (`state.compute`). Each entry routes through one of the two
//!     "bridge" layouts and patches that bridge's settings so the
//!     `/graph/layout/stream?layout_id=` query self-selects the worker
//!     engine per-connection. The UI never calls `PUT /compute/layout`.
//!
//! Bridge routing (worker engine id → bridge + settings patch):
//!   * `geometric`     → active `"geometric"`, LensConfig.use_gpu=false
//!   * `geometric-gpu` → active `"geometric"`, LensConfig.use_gpu=true
//!   * everything else → active `"remote-fa2"`, RemoteFa2Settings.layout_id=<id>

use eframe::egui;

use crate::ui::layout::registry::LayoutRegistry;
use crate::ui::state::AppState;
use crate::ui::theme::palette;

use graph_layouts::geometric::LensConfig;
use crate::ui::layout::algorithms::remote_fa2::RemoteFa2Settings;

use super::{hint_label, row, subgroup_label, subgroup_separator};

/// The two registry ids that are "bridges" to the remote worker rather
/// than real local layouts. They are excluded from the Local group and
/// instead surfaced (with the worker's own engine names) under Remote.
const BRIDGE_GEOMETRIC: &str = "geometric";
const BRIDGE_REMOTE_FA2: &str = "remote-fa2";

/// Read the current geometric lens config out of the settings map (or
/// default). Cheap clone of one JSON blob; called only for the picker.
fn lens_config(state: &AppState) -> LensConfig {
    state
        .layout
        .settings
        .get(BRIDGE_GEOMETRIC)
        .and_then(|v| serde_json::from_value::<LensConfig>(v.clone()).ok())
        .unwrap_or_default()
}

/// Read the current remote-fa2 settings out of the settings map (or default).
fn remote_fa2_settings(state: &AppState) -> RemoteFa2Settings {
    state
        .layout
        .settings
        .get(BRIDGE_REMOTE_FA2)
        .and_then(|v| serde_json::from_value::<RemoteFa2Settings>(v.clone()).ok())
        .unwrap_or_default()
}

/// Route a worker engine id to the appropriate bridge and patch that
/// bridge's persisted settings. The `?layout_id=` query on the stream
/// (built from these settings) self-selects the worker engine, so no
/// `PUT /compute/layout` is needed.
fn select_remote_engine(state: &mut AppState, registry: &LayoutRegistry, engine_id: &str) {
    match engine_id {
        BRIDGE_GEOMETRIC | "geometric-gpu" => {
            let want_gpu = engine_id == "geometric-gpu";
            state.layout.active = BRIDGE_GEOMETRIC.to_string();
            let default = || {
                registry
                    .get(BRIDGE_GEOMETRIC)
                    .map(|f| f.default_settings())
                    .unwrap_or(serde_json::Value::Null)
            };
            let json = state.layout.settings_for_mut(BRIDGE_GEOMETRIC, default);
            let mut cfg: LensConfig = serde_json::from_value(json.clone()).unwrap_or_default();
            cfg.use_gpu = want_gpu;
            cfg.use_multilevel = false;
            if let Ok(v) = serde_json::to_value(&cfg) {
                *json = v;
            }
        }
        other => {
            state.layout.active = BRIDGE_REMOTE_FA2.to_string();
            let default = || {
                registry
                    .get(BRIDGE_REMOTE_FA2)
                    .map(|f| f.default_settings())
                    .unwrap_or(serde_json::Value::Null)
            };
            let json = state.layout.settings_for_mut(BRIDGE_REMOTE_FA2, default);
            let mut cfg: RemoteFa2Settings =
                serde_json::from_value(json.clone()).unwrap_or_default();
            cfg.layout_id = other.to_string();
            if let Ok(v) = serde_json::to_value(&cfg) {
                *json = v;
            }
        }
    }
}

/// Compute the label shown on the collapsed Engine combo, derived from
/// the active layout id and (for the bridges) its settings.
fn selected_text(state: &AppState, registry: &LayoutRegistry) -> String {
    let active = state.layout.active.as_str();
    match active {
        BRIDGE_GEOMETRIC => {
            if lens_config(state).use_gpu {
                "Geometric (GPU)".to_string()
            } else {
                "Geometric constraints".to_string()
            }
        }
        BRIDGE_REMOTE_FA2 => {
            let id = remote_fa2_settings(state).layout_id;
            // Prefer the worker's advertised display_name for this id.
            state
                .compute
                .current()
                .and_then(|r| r.ok())
                .and_then(|eng| {
                    eng.engines
                        .iter()
                        .find(|e| e.id == id)
                        .map(|e| e.display_name.clone())
                })
                .unwrap_or(id)
        }
        other => registry
            .get(other)
            .map(|f| f.descriptor().display_name.to_string())
            .unwrap_or_else(|| "(unknown)".to_string()),
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, registry: &LayoutRegistry) {
    state.snapshot_source = Some("Layout".into());

    // Lazy first fetch of the remote engine list — exactly once per
    // session. Subsequent refreshes are user-driven (the ↻ button).
    if !state.compute.requested_once {
        state.compute.requested_once = true;
        state.compute.refresh_requested = true;
    }

    let active_id = state.layout.active.clone();
    let combo_label = selected_text(state, registry);

    row(ui, "Engine", |ui| {
        egui::ComboBox::from_id_salt("engine")
            .selected_text(combo_label)
            .show_ui(ui, |ui| {
                // ── Local ──────────────────────────────────────────────
                subgroup_label(ui, "Local");
                for factory in registry.iter() {
                    let id = factory.descriptor().id;
                    if id == BRIDGE_GEOMETRIC || id == BRIDGE_REMOTE_FA2 {
                        continue; // bridges live under Remote
                    }
                    if ui
                        .selectable_label(id == active_id.as_str(), factory.descriptor().display_name)
                        .clicked()
                    {
                        state.layout.active = id.to_string();
                    }
                }

                ui.add_space(4.0);

                // ── Remote ─────────────────────────────────────────────
                subgroup_label(ui, "Remote");
                let snapshot = state.compute.current();
                match &snapshot {
                    Some(Ok(eng)) if eng.connected => {
                        for e in &eng.engines {
                            // Highlight the entry the active bridge currently maps to.
                            let selected = match active_id.as_str() {
                                BRIDGE_GEOMETRIC => {
                                    let gpu = lens_config(state).use_gpu;
                                    (gpu && e.id == "geometric-gpu")
                                        || (!gpu && e.id == BRIDGE_GEOMETRIC)
                                }
                                BRIDGE_REMOTE_FA2 => {
                                    remote_fa2_settings(state).layout_id == e.id
                                }
                                _ => false,
                            };
                            let resp = ui
                                .selectable_label(selected, &e.display_name)
                                .on_hover_text(if e.description.is_empty() {
                                    e.kind.clone()
                                } else {
                                    format!("{} — {}", e.kind, e.description)
                                });
                            if resp.clicked() {
                                select_remote_engine(state, registry, &e.id);
                            }
                        }
                    }
                    Some(Ok(_)) => {
                        ui.colored_label(palette::WARNING, "no compute worker");
                    }
                    Some(Err(_)) => {
                        ui.colored_label(palette::BAD, "engines unavailable");
                    }
                    None => {
                        ui.colored_label(palette::GREY, "loading…");
                    }
                }

                ui.add_space(4.0);
                if ui
                    .small_button("↻")
                    .on_hover_text("Refresh remote engine list")
                    .clicked()
                {
                    state.compute.refresh_requested = true;
                }
            });
        ui.add_space(4.0);
        if ui.small_button("↺").on_hover_text("Reset to defaults").clicked() {
            if let Some(factory) = registry.get(&state.layout.active) {
                state
                    .layout
                    .settings
                    .insert(state.layout.active.clone(), factory.default_settings());
            }
        }
    });

    hint_label(
        ui,
        "Remote engines stream from the graph-compute worker via graph-api.",
    );

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
            row(ui, "", |ui| {
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
        //
        // NOTE: we hide this for remote layouts (geometric, remote-fa2)
        // because they don't support local auto-halt/waking; the sim is
        // always "playing" as long as the websocket is connected.
        if matches!(factory.kind(), graph_layouts::LayoutKind::Physics)
            && active_id != BRIDGE_GEOMETRIC
            && active_id != BRIDGE_REMOTE_FA2
        {
            subgroup_separator(ui);
            row(ui, "", |ui| {
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
}
