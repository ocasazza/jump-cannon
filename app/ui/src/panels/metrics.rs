//! Metrics panel — Dioxus port of crates/graph-renderer/src/ui/sections/metrics.rs.
//!
//! Live layout-quality readout with pinning. Where the egui panel set
//! one-shot request flags drained by `App::drain_metrics_request`, this port
//! computes inline: read CPU positions + edges from the wgpu renderer
//! (`render::with_host`) and call `graph_layouts::metrics` — same math, same
//! size gates. The panel never touches the GPU; `positions_cpu` is the
//! renderer's async GPU→CPU readback mirror.
//!
//! Metrics can be **pinned** (persisted); pinned metrics are shown
//! highlighted at the top. The pinned set is the data model a future
//! always-visible HUD strip can read.

use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::render;
use crate::Ctx;

const STORE_KEY: &str = "jc_metrics_v1";

// --- metric model (mirrors ui/state.rs::{MetricKind, MetricsSnapshot}) ---------

/// A layout-quality metric the Metrics panel can display and pin.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum MetricKind {
    EdgeLengthCv,
    EdgeStress,
    FullStress,
    Crossings,
}

impl MetricKind {
    const ALL: &'static [MetricKind] = &[
        MetricKind::EdgeLengthCv,
        MetricKind::EdgeStress,
        MetricKind::FullStress,
        MetricKind::Crossings,
    ];

    fn label(self) -> &'static str {
        match self {
            MetricKind::EdgeLengthCv => "Edge-length CV",
            MetricKind::EdgeStress => "Edge stress (norm.)",
            MetricKind::FullStress => "Full stress (norm.)",
            MetricKind::Crossings => "Edge crossings",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            MetricKind::EdgeLengthCv => {
                "Coefficient of variation of edge lengths. 0 = perfectly uniform. Cheap, O(E)."
            }
            MetricKind::EdgeStress => {
                "Scale-normalized stress over edges only (target distance 1). Cheap, O(E)."
            }
            MetricKind::FullStress => {
                "Scale-normalized stress over ALL node pairs (graph-theoretic distances). \
                 O(n²) — computed on demand and only for small graphs."
            }
            MetricKind::Crossings => {
                "Number of edge pairs that cross in 2D — fewer is more readable. \
                 O(E²), so computed on demand alongside full stress."
            }
        }
    }

    /// This metric's raw value from a snapshot, if computed. Crossings is
    /// surfaced through the same `f32` channel; [`format_value`](Self::format_value)
    /// renders it as an integer.
    fn value(self, snap: &MetricsSnapshot) -> Option<f32> {
        match self {
            MetricKind::EdgeLengthCv => Some(snap.edge_length_cv),
            MetricKind::EdgeStress => Some(snap.edge_stress),
            MetricKind::FullStress => snap.full_stress,
            MetricKind::Crossings => snap.crossings.map(|c| c as f32),
        }
    }

    /// Display string for this metric's value, or `"—"` when not yet computed.
    /// Crossings render as an integer; everything else as a 3-decimal float.
    fn format_value(self, snap: &MetricsSnapshot) -> String {
        match self.value(snap) {
            None => "—".to_string(),
            Some(v) if matches!(self, MetricKind::Crossings) => format!("{}", v as u32),
            Some(v) => format!("{v:.3}"),
        }
    }
}

/// Latest computed layout-quality values for the active layout.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
struct MetricsSnapshot {
    n_nodes: u32,
    n_edges: u32,
    edge_length_cv: f32,
    edge_stress: f32,
    /// `None` until a full-stress compute is requested (and the graph is small
    /// enough that the O(n²) pass is allowed).
    full_stress: Option<f32>,
    /// Edge-crossing count — `None` until the on-demand O(E²) pass runs.
    crossings: Option<u32>,
}

// --- panel state -----------------------------------------------------------------

/// localStorage shape — the same fields egui's `MetricsState` persists
/// (the one-shot request flags it `skip`s are direct calls here).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    pinned: Vec<MetricKind>,
    #[serde(default)]
    last: Option<MetricsSnapshot>,
    #[serde(default)]
    auto: bool,
}

fn restore() -> Persisted {
    LocalStorage::get(STORE_KEY).unwrap_or_default()
}

static PINNED: GlobalSignal<Vec<MetricKind>> = Signal::global(|| restore().pinned);
static SNAP: GlobalSignal<Option<MetricsSnapshot>> = Signal::global(|| restore().last);
static AUTO: GlobalSignal<bool> = Signal::global(|| restore().auto);

fn persist() {
    let _ = LocalStorage::set(
        STORE_KEY,
        Persisted { pinned: PINNED.read().clone(), last: *SNAP.read(), auto: *AUTO.read() },
    );
}

/// Mirror of `App::drain_metrics_request`: cheap O(E) metrics always; the
/// O(n²) full stress and O(E²) crossings only on the explicit "+ full stress"
/// request, each gated by size so the UI stays responsive. Non-full computes
/// preserve the last expensive values across cheap/auto recomputes.
fn compute(full: bool) {
    const MAX_FULL_NODES: usize = 2000;
    const MAX_CROSSING_EDGES: usize = 20_000;

    let Some((positions, edge_pairs)) = render::with_host(|h| {
        let positions = h.pipes.positions_cpu().to_vec();
        let pairs: Vec<(u32, u32)> =
            h.pipes.edges_cpu().chunks_exact(2).map(|c| (c[0], c[1])).collect();
        (positions, pairs)
    }) else {
        return; // renderer not mounted yet — nothing to measure
    };
    let n = positions.len() / 3;

    let edge_length_cv = graph_layouts::metrics::edge_length_cv(&positions, &edge_pairs);
    let edge_stress =
        graph_layouts::metrics::scale_normalized_stress_uniform(&positions, &edge_pairs);

    let (full_stress, crossings) = if full {
        let fs = (n > 0 && n <= MAX_FULL_NODES).then(|| {
            graph_layouts::metrics::all_pairs_normalized_stress(&positions, &edge_pairs, n)
        });
        let cr = (edge_pairs.len() <= MAX_CROSSING_EDGES)
            .then(|| graph_layouts::metrics::edge_crossings(&positions, &edge_pairs));
        (fs, cr)
    } else {
        let prev = *SNAP.read();
        (prev.and_then(|s| s.full_stress), prev.and_then(|s| s.crossings))
    };

    *SNAP.write() = Some(MetricsSnapshot {
        n_nodes: n as u32,
        n_edges: edge_pairs.len() as u32,
        edge_length_cv,
        edge_stress,
        full_stress,
        crossings,
    });
    persist();
}

// --- view ------------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    rsx! { MetricsPanel {} }
}

#[component]
fn MetricsPanel() -> Element {
    // Live mode: egui recomputes the cheap metrics every frame; this panel
    // has no per-frame hook (the rAF loop is renderer-internal), so a short
    // timer stands in. Cheap metrics are O(E) — 150 ms keeps it incidental.
    use_future(move || async move {
        loop {
            if *AUTO.read() {
                compute(false);
            }
            gloo_timers::future::TimeoutFuture::new(150).await;
        }
    });

    let auto = *AUTO.read();
    let snap = *SNAP.read();
    let pinned = PINNED.read().clone();
    rsx! {
        div { class: "metrics-panel",
            div { class: "metrics-actions",
                button {
                    class: "btn",
                    title: "Recompute edge-based metrics from the current layout",
                    onclick: move |_| compute(false),
                    "Compute"
                }
                button {
                    class: "btn",
                    title: "Also compute all-pairs stress (O(n²)) and crossings (O(E²)); small graphs only",
                    onclick: move |_| compute(true),
                    "+ full stress"
                }
            }
            label {
                class: "metrics-live",
                title: "Recompute the cheap (edge-based) metrics every frame",
                input {
                    r#type: "checkbox",
                    checked: auto,
                    onchange: move |e| {
                        *AUTO.write() = e.checked();
                        persist();
                    },
                }
                "Live"
            }
            hr {}
            if let Some(snap) = snap {
                div { class: "metrics-counts", { format!("nodes {}   edges {}", snap.n_nodes, snap.n_edges) } }
                hr {}
                // Pinned metrics first (highlighted), then the full list.
                if !pinned.is_empty() {
                    div { class: "subgroup", "Pinned" }
                    for (i, m) in pinned.into_iter().enumerate() {
                        MetricRow { key: "p{i}", kind: m, snap, highlighted: true }
                    }
                    hr {}
                }
                div { class: "subgroup", "All metrics" }
                for (i, m) in MetricKind::ALL.iter().enumerate() {
                    MetricRow { key: "a{i}", kind: *m, snap, highlighted: false }
                }
            } else {
                div { class: "metrics-hint",
                    "Press Compute to read layout-quality metrics for the current graph."
                }
            }
        }
    }
}

/// One metric line: pin toggle + label + monospace value (weak "—" when the
/// metric hasn't been computed). The whole row carries the hint tooltip.
#[component]
fn MetricRow(kind: MetricKind, snap: MetricsSnapshot, highlighted: bool) -> Element {
    let is_pinned = PINNED.read().contains(&kind);
    let has_value = kind.value(&snap).is_some();
    let text = kind.format_value(&snap);
    rsx! {
        div {
            class: if highlighted { "metric-row highlighted" } else { "metric-row" },
            title: kind.hint(),
            button {
                class: if is_pinned { "pin on" } else { "pin" },
                title: "Pin / unpin this metric",
                onclick: move |_| {
                    {
                        let mut p = PINNED.write();
                        if let Some(i) = p.iter().position(|x| *x == kind) {
                            p.remove(i);
                        } else {
                            p.push(kind);
                        }
                    }
                    persist();
                },
                "📌"
            }
            span { class: "metric-label", { kind.label() } }
            span {
                class: if has_value { "metric-value" } else { "metric-value weak" },
                "{text}"
            }
        }
    }
}
