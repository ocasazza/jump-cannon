//! Shared frontmatter key/value grid used by both the metadata modal
//! and the right-hand inspector sidebar.
//!
//! Renders one row per (key, value) pair from a parsed
//! `serde_json::Map<String, Value>` for values that the chip walker in
//! `ui::frontmatter_chip` does NOT render — long-form strings, nested
//! arrays/objects, nulls, and any key in [`SKIP_KEYS`] (already
//! promoted to typed `NodeMeta` fields and rendered above the strip).
//!
//! The dedupe rule is: the chip walker renders the values it can shape
//! into a chip, and this grid renders the leftovers. A value is never
//! shown by both surfaces.

use eframe::egui;
use serde_json::Value;

use crate::ui::theme::palette;

/// Keys already promoted to typed `NodeMeta` fields (and rendered as
/// chips elsewhere) or otherwise display-only. Mirrors the SKIP_KEYS in
/// `frontmatter_chip.rs` — the two lists must stay in sync so the grid
/// and chip surfaces don't double-render the same field.
const SKIP_KEYS: &[&str] = &[
    "tags", "tag", "doctype", "folder", "title", "id", "path",
];

fn is_skipped(key: &str) -> bool {
    SKIP_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key))
}

/// Returns true when the chip walker (`render_frontmatter_chips`)
/// would emit at least one chip for this value, so the grid should NOT
/// also render the row. Mirrors the case analysis in
/// `frontmatter_chip::render_value` / `render_string`.
fn chip_walker_handles(value: &Value) -> bool {
    match value {
        Value::String(s) => {
            let t = s.trim();
            // Mirrors frontmatter_chip::render_string: long strings
            // (>120 chars) and empty strings fall through to the grid.
            !t.is_empty() && t.chars().count() <= 120
        }
        Value::Array(arr) => {
            // Chip walker renders any scalar element. If at least one
            // element is scalar, it's handled there.
            arr.iter().any(|v| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_)))
        }
        Value::Number(_) | Value::Bool(_) => true,
        Value::Null | Value::Object(_) => false,
    }
}

/// Render the leftover-frontmatter grid. Returns nothing — this surface
/// is read-only (long text / nested JSON / nulls have no obvious click
/// affordance). Hidden when there are no leftover rows.
pub fn show_frontmatter_grid(
    ui: &mut egui::Ui,
    map: &serde_json::Map<String, Value>,
    grid_id: &str,
) {
    let rows: Vec<(&String, &Value)> = map
        .iter()
        .filter(|(k, v)| !is_skipped(k) && !chip_walker_handles(v))
        .collect();
    if rows.is_empty() {
        return;
    }
    egui::Grid::new(grid_id)
        .num_columns(2)
        .striped(false)
        .show(ui, |ui| {
            for (key, value) in rows {
                label_cell(ui, key);
                value_cell(ui, value);
                ui.end_row();
            }
        });
}

fn label_cell(ui: &mut egui::Ui, key: &str) {
    ui.label(
        egui::RichText::new(key)
            .small()
            .weak()
            .monospace(),
    );
}

fn value_cell(ui: &mut egui::Ui, value: &Value) {
    match value {
        Value::String(s) => {
            // Long-form text — render in a bounded scroller so a
            // multi-paragraph YAML string doesn't stretch the panel.
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(s)
                            .small()
                            .color(palette::TEXT),
                    );
                });
        }
        Value::Null => {
            ui.label(egui::RichText::new("—").weak());
        }
        Value::Object(_) | Value::Array(_) => {
            // Pretty-print nested JSON so nested objects and
            // mixed arrays still surface their contents.
            let pretty = serde_json::to_string_pretty(value)
                .unwrap_or_else(|_| value.to_string());
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(pretty)
                            .monospace()
                            .small()
                            .color(palette::TEXT),
                    );
                });
        }
        // Numbers and bools are scalars — the chip walker would have
        // taken them, so we never reach this arm under normal flow.
        Value::Number(n) => {
            ui.label(egui::RichText::new(n.to_string()).monospace().small());
        }
        Value::Bool(b) => {
            ui.label(if *b { "true" } else { "false" });
        }
    }
}
