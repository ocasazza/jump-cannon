//! Edge Inspector panel — lists anomalous edges detected by the graph-metrics
//! `edge_anomaly` module (crates/graph-metrics/src/edge_anomaly.rs).
//!
//! Shows a sortable, threshold-filtered list of edges whose actual Jaccard
//! similarity deviates from the expected value. Each entry surfaces the
//! anomaly score, the actual and expected Jaccard, and click-to-select
//! actions for source and target nodes.
//!
//! For now the data is hardcoded — the backend plumbing to serve
//! `detect_anomalous_edges` through graph-api is a follow-up task.

use dioxus::prelude::*;

use crate::Ctx;

// --- data model (mirrors crates/graph-metrics/src/edge_anomaly.rs) ----------------

/// An anomalous edge detected by `edge_anomaly::detect_anomalous_edges`.
#[derive(Clone, Debug, PartialEq)]
struct EdgeAnomaly {
    edge_idx: u32,
    source: u32,
    target: u32,
    /// The anomaly score: actual Jaccard divided by expected Jaccard.
    /// > 1 means the edge exists far more than expected.
    anomaly_score: f32,
    actual_jaccard: f32,
    expected_jaccard: f32,
}

// --- hardcoded sample data --------------------------------------------------------

fn sample_anomalies() -> Vec<EdgeAnomaly> {
    vec![
        EdgeAnomaly {
            edge_idx: 234,
            source: 12,
            target: 891,
            anomaly_score: 4.2,
            actual_jaccard: 0.084,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 567,
            source: 45,
            target: 123,
            anomaly_score: 3.8,
            actual_jaccard: 0.076,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 89,
            source: 7,
            target: 456,
            anomaly_score: 5.1,
            actual_jaccard: 0.102,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 1023,
            source: 301,
            target: 777,
            anomaly_score: 2.4,
            actual_jaccard: 0.048,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 342,
            source: 56,
            target: 234,
            anomaly_score: 6.7,
            actual_jaccard: 0.134,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 711,
            source: 99,
            target: 512,
            anomaly_score: 1.9,
            actual_jaccard: 0.038,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 415,
            source: 23,
            target: 667,
            anomaly_score: 3.1,
            actual_jaccard: 0.062,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 888,
            source: 150,
            target: 920,
            anomaly_score: 7.2,
            actual_jaccard: 0.144,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 201,
            source: 33,
            target: 189,
            anomaly_score: 4.8,
            actual_jaccard: 0.096,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 630,
            source: 78,
            target: 345,
            anomaly_score: 3.5,
            actual_jaccard: 0.07,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 445,
            source: 19,
            target: 801,
            anomaly_score: 2.8,
            actual_jaccard: 0.056,
            expected_jaccard: 0.02,
        },
        EdgeAnomaly {
            edge_idx: 990,
            source: 210,
            target: 555,
            anomaly_score: 5.5,
            actual_jaccard: 0.11,
            expected_jaccard: 0.02,
        }
    ]
}

// --- sort modes -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortBy {
    Anomaly,
    EdgeIndex,
    Source,
    Target,
}

impl SortBy {
    fn all() -> [SortBy; 4] {
        [SortBy::Anomaly, SortBy::EdgeIndex, SortBy::Source, SortBy::Target]
    }

    fn label(self) -> &'static str {
        match self {
            SortBy::Anomaly => "Anomaly",
            SortBy::EdgeIndex => "Edge #",
            SortBy::Source => "Source",
            SortBy::Target => "Target",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SortBy::Anomaly => "Anomaly",
            SortBy::EdgeIndex => "EdgeIndex",
            SortBy::Source => "Source",
            SortBy::Target => "Target",
        }
    }
}

// --- panel state (module-level signals) -------------------------------------------

static THRESHOLD: GlobalSignal<f32> = Signal::global(|| 3.0);
static SORT: GlobalSignal<SortBy> = Signal::global(|| SortBy::Anomaly);

// --- view -------------------------------------------------------------------------

pub fn panel(_ctx: Ctx) -> Element {
    rsx! { EdgeInspectorPanel { ctx: _ctx } }
}

#[component]
fn EdgeInspectorPanel(ctx: Ctx) -> Element {
    let threshold = *THRESHOLD.read();
    let sort = *SORT.read();

    let all = sample_anomalies();

    // Filter by threshold
    let filtered: Vec<EdgeAnomaly> = all
        .into_iter()
        .filter(|a| a.anomaly_score >= threshold)
        .collect();

    // Sort
    let mut sorted = filtered;
    match sort {
        SortBy::Anomaly => {
            sorted.sort_by(|a, b| b.anomaly_score.partial_cmp(&a.anomaly_score).unwrap());
        }
        SortBy::EdgeIndex => sorted.sort_by_key(|a| a.edge_idx),
        SortBy::Source => sorted.sort_by_key(|a| a.source),
        SortBy::Target => sorted.sort_by_key(|a| a.target),
    }

    let count = sorted.len();
    let total = sample_anomalies().len();

    // Build sort options once
    let sort_options: Vec<(SortBy, bool)> = SortBy::all()
        .into_iter()
        .map(|k| (k, k == sort))
        .collect();

    let threshold_str = format!("{threshold:.1}");
    let empty_msg = format!("No anomalous edges found above {threshold:.1}×");

    rsx! {
        div { class: "edge-inspector-panel",
            // --- controls row --------------------------------------------------
            div { class: "ei-controls",
                label { class: "ei-label", "Sort by:" }
                select {
                    class: "ei-select",
                    onchange: move |e| {
                        let val = e.value();
                        let next = match val.as_str() {
                            "Anomaly" => SortBy::Anomaly,
                            "EdgeIndex" => SortBy::EdgeIndex,
                            "Source" => SortBy::Source,
                            "Target" => SortBy::Target,
                            _ => SortBy::Anomaly,
                        };
                        *SORT.write() = next;
                    },
                    for (kind, is_selected) in sort_options.into_iter() {
                        option {
                            value: kind.as_str(),
                            selected: is_selected,
                            "{kind.label()}"
                        }
                    }
                }
                label {
                    class: "ei-label",
                    title: "Only show edges whose anomaly score is at least this many times the expected value",
                    "Threshold:"
                }
                span { class: "ei-threshold-value", "{threshold:.1}×" }
                input {
                    class: "ei-slider",
                    r#type: "range",
                    min: "1.0",
                    max: "10.0",
                    step: "0.1",
                    value: "{threshold_str}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<f32>() {
                            *THRESHOLD.write() = v;
                        }
                    },
                }
            }
            // --- edge list -----------------------------------------------------
            div { class: "ei-list",
                if count == 0 {
                    div { class: "ei-empty", "{empty_msg}" }
                } else {
                    for anomaly in sorted.into_iter() {
                        {
                            let key = format!("ei-{}", anomaly.edge_idx);
                            rsx! {
                                EdgeRow {
                                    key,
                                    anomaly,
                                    ctx,
                                }
                            }
                        }
                    }
                }
            }
            // --- footer summary ------------------------------------------------
            div { class: "ei-footer",
                if count == total {
                    {format!("{count} anomalous edge{} found", if count != 1 { "s" } else { "" })}
                } else {
                    {format!("{count} anomalous edge{} shown ({total} total, threshold ≥ {threshold:.1}×)", if count != 1 { "s" } else { "" })}
                }
            }
        }
    }
}

/// One anomalous edge row: source to target, anomaly score, actual/expected Jaccard,
/// and select/highlight actions.
#[component]
fn EdgeRow(anomaly: EdgeAnomaly, ctx: Ctx) -> Element {
    let score = anomaly.anomaly_score;
    let actual = anomaly.actual_jaccard;
    let expected = anomaly.expected_jaccard;

    // Choose a severity class based on the anomaly score
    let severity = if score >= 5.0 {
        "ei-severe"
    } else if score >= 3.0 {
        "ei-moderate"
    } else {
        "ei-mild"
    };

    let source_str = anomaly.source.to_string();
    let target_str = anomaly.target.to_string();

    // Each onclick closure needs its own owned copy so they can
    // independently clone inside (FnMut requirement).
    let src1 = source_str.clone();
    let src2 = source_str.clone();
    let tgt1 = target_str.clone();
    let tgt2 = target_str.clone();

    rsx! {
        div { class: "ei-row {severity}",
            // --- summary line ---
            div { class: "ei-summary",
                span { class: "ei-icon", "⚠" }
                span { class: "ei-edge-id", "Edge #{anomaly.edge_idx}:" }
                span { class: "ei-endpoints",
                    button {
                        class: "ei-node-btn",
                        title: "Select source node {anomaly.source}",
                        onclick: move |_| { ctx.selected.set(Some(src1.clone())); },
                        "{anomaly.source}"
                    }
                    span { class: "ei-arrow", " → " }
                    button {
                        class: "ei-node-btn",
                        title: "Select target node {anomaly.target}",
                        onclick: move |_| { ctx.selected.set(Some(tgt1.clone())); },
                        "{anomaly.target}"
                    }
                }
                span { class: "ei-score", "Anomaly: {score:.1}×" }
            }
            // --- detail line ---
            div { class: "ei-detail",
                span { class: "ei-metric", "Jaccard: {actual:.3}" }
                span { class: "ei-metric", "Expected: {expected:.3}" }
                button {
                    class: "btn ei-action-btn",
                    title: "Select source node {anomaly.source}",
                    onclick: move |_| { ctx.selected.set(Some(src2.clone())); },
                    "Select source"
                }
                button {
                    class: "btn ei-action-btn",
                    title: "Select target node {anomaly.target}",
                    onclick: move |_| { ctx.selected.set(Some(tgt2.clone())); },
                    "Select target"
                }
                button {
                    class: "btn ei-action-btn",
                    title: "Highlight the path from {anomaly.source} to {anomaly.target} in the graph (not yet wired)",
                    "Highlight path"
                }
            }
        }
    }
}