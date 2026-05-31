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
            .on_hover_text("Also compute all-pairs scale-normalized stress (O(n²); small graphs only)")
            .clicked()
        {
            state.metrics.compute_requested = true;
            state.metrics.compute_full_requested = true;
        }
    });
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
    let value = match m {
        MetricKind::EdgeLengthCv => Some(snap.edge_length_cv),
        MetricKind::EdgeStress => Some(snap.edge_stress),
        MetricKind::FullStress => snap.full_stress,
    };
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
            match value {
                Some(v) => ui.monospace(format!("{v:.4}")),
                None => ui.weak("— (press + full stress)"),
            };
        })
        .response;
    resp.on_hover_text(m.hint());
}
