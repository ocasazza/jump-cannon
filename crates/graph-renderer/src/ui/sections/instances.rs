//! Instances panel — read-only view of `ActionInstance`s recorded via
//! the command palette. Mirrors `archive/nuxt/components/actions/ActionCard.vue`
//! but render-only for now (re-execution + edit-in-place can come later
//! once we have a use case driving it).

use eframe::egui;

use crate::ui::actions::{ActionRegistry, ParamValue};
use crate::ui::file_io;
use crate::ui::state::{self, AppState};

/// Apply an imported AppState YAML to the live state — a full replacement that
/// preserves the in-memory snapshot ring (it's `#[serde(skip)]`), then stamps a
/// timeline entry. Shared by the paste-Load, file-Upload, and server-preset paths.
fn apply_imported_yaml(state: &mut AppState, yaml: &str) -> Result<(), String> {
    let imported = state::import_state_yaml(yaml)?;
    let ring = std::mem::take(&mut state.snapshots);
    *state = imported;
    state.snapshots = ring;
    state.snapshot_now("import yaml");
    Ok(())
}

use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, registry: &mut ActionRegistry) {
    // Order matters: the State timeline is the most-frequently-useful
    // sub-region (the user expects to see `default` + `restored` the
    // instant the panel opens), so it sits ABOVE the YAML import/export
    // block — which dwarfs the panel with two ~12-row TextEdits and was
    // pushing the timeline below the visible fold of the default
    // [280, 520] floating panel.
    state_timeline_panel(ui, state);
    subgroup_separator(ui);
    share_link_panel(ui, state);
    subgroup_separator(ui);
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

    // Drain async results from file-upload + server-preset fetch (they arrive
    // across frames via static slots). Applying here keeps the buttons trivial.
    if let Some(text) = file_io::take_upload() {
        match apply_imported_yaml(state, &text) {
            Ok(()) => state.yaml_import_error = None,
            Err(e) => state.yaml_import_error = Some(format!("upload parse: {e}")),
        }
    }
    if let Some(res) = file_io::take_preset() {
        match res {
            Ok(yaml) => match apply_imported_yaml(state, &yaml) {
                Ok(()) => state.yaml_import_error = None,
                Err(e) => state.yaml_import_error = Some(format!("preset parse: {e}")),
            },
            Err(e) => state.yaml_import_error = Some(format!("preset fetch: {e}")),
        }
    }

    // The export buffer is a LIVE mirror of the current AppState: re-serialized
    // every frame the panel is open, so what you see / copy / download always
    // reflects the present UI state — no stale "click Export first" snapshot.
    // (`yaml_export_buffer` is `#[serde(skip)]`, so this doesn't recurse.)
    state.yaml_export_buffer =
        state::export_state_yaml(state).unwrap_or_else(|e| format!("# export error: {e}"));

    // ---- Export row (live) ----------------------------------------------
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Live state")
                .italics()
                .color(egui::Color32::from_rgb(150, 130, 200)),
        );
        if ui
            .button("Copy")
            .on_hover_text("Copy the current state as YAML")
            .clicked()
        {
            let yaml = state.yaml_export_buffer.clone();
            ui.output_mut(|o| o.copied_text = yaml);
        }
        if ui
            .button("⬇ File")
            .on_hover_text("Download the entire current app state as a .yaml file")
            .clicked()
        {
            if let Err(e) = file_io::download_text(
                "jump-cannon-appstate.yaml",
                "application/yaml",
                &state.yaml_export_buffer,
            ) {
                state.yaml_import_error = Some(format!("download: {e}"));
            }
        }
    });

    ui.add(
        egui::TextEdit::multiline(&mut state.yaml_export_buffer.as_str())
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(12),
    );

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
                    // setting" contract. Preserve the in-memory snapshot
                    // ring across the swap (it's `#[serde(skip)]` and
                    // would otherwise be wiped) and stamp the import as
                    // its own entry so the user sees the YAML load in
                    // the timeline.
                    let ring = std::mem::take(&mut state.snapshots);
                    *state = imported;
                    state.snapshots = ring;
                    state.snapshot_now("import yaml");
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
        ui.colored_label(
            egui::Color32::from_rgb(220, 70, 70),
            format!("Parse error: {err}"),
        );
    }

    ui.add_space(6.0);

    // ---- Load from file / dev-server preset -----------------------------
    subgroup_label(ui, "Load from file or dev-server preset");
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("⬆ Upload .yaml")
            .on_hover_text("Pick a YAML file and load the full app state from it")
            .clicked()
        {
            file_io::open_upload(".yaml,.yml,application/yaml");
        }
        ui.separator();
        ui.label("Presets:");
        for &name in file_io::PRESET_NAMES {
            if ui
                .button(name)
                .on_hover_text(format!("Load /configs/{name} from the dev server"))
                .clicked()
            {
                file_io::fetch_preset(name);
            }
        }
    });

    ui.add_space(6.0);

    // ---- Reset to defaults (two-step) -----------------------------------
    ui.horizontal(|ui| {
        let (label, color) = if state.yaml_reset_armed {
            ("Confirm reset", egui::Color32::from_rgb(220, 70, 70))
        } else {
            ("Reset to defaults", egui::Color32::from_rgb(170, 60, 60))
        };
        let btn = egui::Button::new(
            egui::RichText::new(label)
                .color(egui::Color32::WHITE)
                .small(),
        )
        .fill(color);
        if ui.add(btn).clicked() {
            if state.yaml_reset_armed {
                // Preserve the timeline across the reset so the user
                // can roll back. Tag the reset as its own entry.
                let ring = std::mem::take(&mut state.snapshots);
                *state = AppState::default();
                state.snapshots = ring;
                state.snapshot_now("reset to defaults");
            } else {
                state.yaml_reset_armed = true;
            }
        }
        if state.yaml_reset_armed && ui.small_button("Cancel").clicked() {
            state.yaml_reset_armed = false;
        }
    });
}

/// Shareable hash / link sub-region. "Copy share link" encodes the current
/// `AppState` (compact JSON → DEFLATE → base64url) into a short token and copies
/// `<origin>/#s=<hash>` on WASM (or the bare hash on native). "Load from link /
/// hash" decodes a pasted token/link back into the full `AppState`.
fn share_link_panel(ui: &mut egui::Ui, state: &mut AppState) {
    use crate::ui::share;

    subgroup_label(ui, "Share link");

    // ---- Produce row -----------------------------------------------------
    ui.horizontal(|ui| {
        if ui
            .button("Copy share link")
            .on_hover_text("Encode the entire UI state into a short hash and copy a shareable link")
            .clicked()
        {
            match share::encode(state) {
                Ok(hash) => {
                    let link = current_origin()
                        .map(|o| share::link_for(&o, &hash))
                        .unwrap_or(hash);
                    ui.output_mut(|o| o.copied_text = link.clone());
                    state.share_link_buffer = link;
                }
                Err(e) => state.share_link_buffer = format!("encode error: {e}"),
            }
        }
        let has = !state.share_link_buffer.is_empty();
        if ui
            .add_enabled(has, egui::Button::new("✕"))
            .on_hover_text("Clear")
            .clicked()
        {
            state.share_link_buffer.clear();
        }
    });

    if !state.share_link_buffer.is_empty() {
        ui.add(
            egui::TextEdit::multiline(&mut state.share_link_buffer.as_str())
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(2),
        );
    }

    ui.add_space(4.0);

    // ---- Load row --------------------------------------------------------
    subgroup_label(ui, "Load from link / hash");
    ui.add(
        egui::TextEdit::singleline(&mut state.share_import_buffer)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .hint_text("Paste a share link or #s=<hash>, then Load."),
    );
    ui.horizontal(|ui| {
        let has = !state.share_import_buffer.trim().is_empty();
        if ui.add_enabled(has, egui::Button::new("Load")).clicked() {
            match share::decode(&state.share_import_buffer) {
                Ok(imported) => {
                    let ring = std::mem::take(&mut state.snapshots);
                    *state = imported;
                    state.snapshots = ring;
                    state.snapshot_now("load share link");
                }
                Err(e) => state.share_import_error = Some(e),
            }
        }
        if ui.button("Clear").clicked() {
            state.share_import_buffer.clear();
            state.share_import_error = None;
        }
    });
    if let Some(err) = &state.share_import_error {
        ui.colored_label(
            egui::Color32::from_rgb(220, 70, 70),
            format!("Decode error: {err}"),
        );
    }
}

/// The page origin on WASM (`https://host`), or `None` on native — the panel
/// then shows the bare hash instead of a full link.
#[cfg(target_arch = "wasm32")]
fn current_origin() -> Option<String> {
    web_sys::window()?.location().origin().ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_origin() -> Option<String> {
    None
}

/// Render the live snapshot timeline (newest first), with one
/// "Restore" button per row plus a footer with capacity + clear.
///
/// Restore deserialises the snapshot's stored JSON back into
/// `AppState`, preserves the in-memory ring across the swap, and
/// pushes a `restore @ <orig_timestamp>` entry — so the restore
/// itself becomes a timeline event the user can undo.
fn state_timeline_panel(ui: &mut egui::Ui, state: &mut AppState) {
    subgroup_label(ui, "State timeline");

    // Capacity / clear row.
    let len = state.snapshots.entries.len();
    let cap = state.snapshots.max;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{len} / {cap}"))
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // "Clear timeline" leaves capacity untouched and reseeds
            // a single `cleared` snapshot of the current state so the
            // panel never becomes empty mid-session (otherwise the
            // very next mutation would silently become the "earliest"
            // visible entry with nothing to restore back to).
            if ui.small_button("Clear timeline").clicked() {
                state.snapshots.entries.clear();
                state.snapshot_now("cleared");
            }
        });
    });

    if state.snapshots.entries.is_empty() {
        hint_label(ui, "Timeline empty.");
        return;
    }

    // Decide which (if any) entry to restore after the immutable
    // borrow ends. We can't mutate `state` from inside the iteration
    // because we're already holding a borrow of `state.snapshots`.
    let mut to_restore: Option<usize> = None;
    let entries = state.snapshots.entries.clone();

    egui::ScrollArea::vertical()
        .max_height(220.0)
        .id_salt("state_timeline_scroll")
        .show(ui, |ui| {
            for (idx, entry) in entries.iter().enumerate().rev() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format_timestamp_ms(entry.timestamp_ms))
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                    ui.label(egui::RichText::new(&entry.source).size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Restore").clicked() {
                            to_restore = Some(idx);
                        }
                    });
                });
            }
        });

    if let Some(idx) = to_restore {
        let entry = entries[idx].clone();
        match serde_json::from_str::<AppState>(&entry.state_json) {
            Ok(imported) => {
                // Preserve the timeline across the swap (it's
                // `#[serde(skip)]` and would otherwise be wiped), then
                // stamp the restore so it's itself in the timeline.
                let ring = std::mem::take(&mut state.snapshots);
                *state = imported;
                state.snapshots = ring;
                state.snapshot_now(format!(
                    "restore @ {}",
                    format_timestamp_ms(entry.timestamp_ms)
                ));
            }
            Err(e) => {
                state.yaml_import_error = Some(format!("restore failed: {e}"));
            }
        }
    }
}

/// Format a Unix-epoch-millis timestamp as `HH:MM:SS.mmm` in UTC.
/// Tiny by-hand helper — we don't want a chrono dep for one label.
fn format_timestamp_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{millis:03}")
}

fn param_value_display(v: &ParamValue) -> String {
    match v {
        ParamValue::String(s) => format!("\"{s}\""),
        ParamValue::Number(n) => format!("{n}"),
        ParamValue::Boolean(b) => format!("{b}"),
        ParamValue::Selected(items) => format!("[{}]", items.join(", ")),
    }
}
