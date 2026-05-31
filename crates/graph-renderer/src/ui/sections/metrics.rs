//! Metrics section — live layout-quality readout with pinning.
//!
//! Reads the last [`MetricsSnapshot`] computed by `App::drain_metrics_request`
//! (which pulls CPU positions + edges from the live pipeline and calls
//! `graph_layouts::metrics`). The panel itself never touches the GPU — it sets
//! one-shot request flags and renders whatever the App last computed.
//!
//! Metrics can be **pinned** (persisted in `AppState`); pinned metrics are shown
//! highlighted at the top. The pinned set is the data model a future
//! always-visible HUD strip can read.

use eframe::egui;

use super::super::state::{AppState, MetricKind, MetricsSnapshot};
use super::{hint_label, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        if ui
            .button("Compute")
            .on_hover_text("Recompute edge-based metrics from the current layout")
            .clicked()
        {
            state.metrics.compute_requested = true;
        }
        if ui
            .button("+ full stress")
            .on_hover_text("Also compute all-pairs stress (O(n²)) and crossings (O(E²)); small graphs only")
            .clicked()
        {
            state.metrics.compute_requested = true;
            state.metrics.compute_full_requested = true;
        }
    });
    ui.checkbox(&mut state.metrics.auto, "Live")
        .on_hover_text("Recompute the cheap (edge-based) metrics every frame");
    subgroup_separator(ui);

    let Some(snap) = state.metrics.last else {
        hint_label(
            ui,
            "Press Compute to read layout-quality metrics for the current graph.",
        );
        return;
    };

    ui.label(format!("nodes {}   edges {}", snap.n_nodes, snap.n_edges));
    subgroup_separator(ui);

    // Pinned metrics first (highlighted), then the full list.
    let pinned = state.metrics.pinned.clone();
    if !pinned.is_empty() {
        subgroup_label(ui, "Pinned");
        for m in &pinned {
            metric_row(ui, state, *m, &snap);
        }
        subgroup_separator(ui);
    }

    subgroup_label(ui, "All metrics");
    for &m in MetricKind::ALL {
        metric_row(ui, state, m, &snap);
    }
}

fn metric_row(ui: &mut egui::Ui, state: &mut AppState, m: MetricKind, snap: &MetricsSnapshot) {
    let resp = ui
        .horizontal(|ui| {
            let pinned = state.metrics.pinned.contains(&m);
            if ui
                .selectable_label(pinned, "📌")
                .on_hover_text("Pin / unpin this metric")
                .clicked()
            {
                if pinned {
                    state.metrics.pinned.retain(|x| *x != m);
                } else {
                    state.metrics.pinned.push(m);
                }
            }
            ui.label(m.label());
            // Shared value/format logic lives on MetricKind (see state.rs).
            let text = m.format_value(snap);
            if m.value(snap).is_some() {
                ui.monospace(text);
            } else {
                ui.weak(text);
            }
        })
        .response;
    resp.on_hover_text(m.hint());
}
