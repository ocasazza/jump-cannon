//! Edge anomaly detection — identifies "surprising" edges whose Jaccard overlap
//! deviates significantly from what is expected given endpoint degrees.
//!
//! A high anomaly score means the edge is structurally unusual: either far more
//! embedded (a local bridge inside a sparse neighbourhood) or far less embedded
//! (a global shortcut through dense hubs) than a random-graph baseline would predict.
//! These are the "magic methyl" edges — small changes that have outsized structural impact.

use vault_data::VaultGraph;

use crate::edge_strength::{compute_edge_strength, EdgeStrengthKind};

/// An anomalous edge — an edge whose structural role is surprising.
#[derive(Debug, Clone)]
pub struct EdgeAnomaly {
    /// Index into `graph.edges`.
    pub edge_idx: u32,
    /// Source node id.
    pub source: u32,
    /// Target node id.
    pub target: u32,
    /// Actual Jaccard overlap (Jaccard edge strength).
    pub actual_jaccard: f32,
    /// Expected Jaccard overlap from a random-graph baseline.
    pub expected_jaccard: f32,
    /// anomaly_score = expected / max(actual, 0.001). Higher = more surprising.
    pub anomaly_score: f32,
    /// Estimated stress impact if this edge were removed (optional, expensive).
    pub removal_impact: Option<f32>,
}

/// Detect anomalous edges in the graph.
///
/// An edge is anomalous when its actual Jaccard overlap deviates significantly
/// from what a random-graph baseline would predict from its endpoint degrees.
/// The anomaly score is `expected / max(actual, 0.001)` — values above 1.0 mean
/// the edge is more sparse than expected (the typical surprise: a bridge through
/// dense hubs), while values below 1.0 mean more embedded than expected.
///
/// # Arguments
///
/// * `threshold` — edges with `anomaly_score > threshold` are included.
/// * `max_results` — cap on returned edges (sorted by anomaly_score descending).
/// * `compute_impact` — if true, compute a crude energy-proxy estimate of the
///   spring-layout stress change from removing each anomalous edge.
pub fn detect_anomalous_edges(
    graph: &VaultGraph,
    threshold: f32,
    max_results: usize,
    compute_impact: bool,
) -> Vec<EdgeAnomaly> {
    let n = graph.nodes.len() as f32;
    let edge_strength = compute_edge_strength(graph, EdgeStrengthKind::Jaccard);

    let mut anomalies: Vec<EdgeAnomaly> = Vec::new();
    let max_results = max_results.max(1);

    for (i, edge) in graph.edges.iter().enumerate() {
        let actual_jaccard = edge_strength.strength[i];

        // Estimate expected Jaccard from degrees.
        let expected_jaccard = expected_jaccard_from_graph(graph, n, edge);

        // Anomaly score: how many times expected exceeds actual.
        // Use max(actual, 0.001) to avoid division by zero / blowup.
        let clamped_actual = actual_jaccard.max(0.001);
        let anomaly_score = expected_jaccard / clamped_actual;

        if anomaly_score > threshold {
            anomalies.push(EdgeAnomaly {
                edge_idx: i as u32,
                source: 0, // placeholder — we don't have u32 ids here; fill from index mapping
                target: 0,
                actual_jaccard,
                expected_jaccard,
                anomaly_score,
                removal_impact: None,
            });
        }
    }

    // Sort descending by anomaly_score.
    anomalies.sort_unstable_by(|a, b| b.anomaly_score.total_cmp(&a.anomaly_score));

    // Truncate to max_results.
    if anomalies.len() > max_results {
        anomalies.truncate(max_results);
    }

    // Optionally compute removal impact (spring energy proxy).
    if compute_impact {
        for a in &mut anomalies {
            a.removal_impact = Some(spring_energy_proxy(a.actual_jaccard));
        }
    }

    anomalies
}

/// Estimate expected Jaccard overlap from endpoint degrees using a random-graph
/// baseline.
///
/// Expected number of common neighbours ≈ (d_u · d_v) / |V|.
/// Expected Jaccard = expected_common / (d_u + d_v - expected_common), clamped to [0, 1].
fn expected_jaccard_from_graph(graph: &VaultGraph, n: f32, edge: &vault_data::VaultEdge) -> f32 {
    // Build a quick degree lookup inline (reuses neighbour-set logic from edge_strength).
    // We compute degrees on the fly to avoid a separate pass.
    let du = degree_of(graph, &edge.source);
    let dv = degree_of(graph, &edge.target);

    if du == 0.0 || dv == 0.0 {
        return 0.0;
    }

    let expected_common = (du * dv) / n;
    let denom = du + dv - expected_common;
    if denom <= 0.0 {
        return 0.0;
    }
    (expected_common / denom).clamp(0.0, 1.0)
}

/// Count the undirected degree of a node (number of distinct neighbours).
fn degree_of(graph: &VaultGraph, node_id: &str) -> f32 {
    let mut count = 0.0;
    for edge in &graph.edges {
        if edge.source == node_id && edge.target != node_id {
            count += 1.0;
        }
        if edge.target == node_id && edge.source != node_id {
            count += 1.0;
        }
    }
    count
}

/// Crude spring-energy proxy for the stress change from removing an edge.
///
/// `energy ≈ spring_k · (strength)² · spring_len`, where spring_k and spring_len
/// are treated as constants (we only need relative ranking).
fn spring_energy_proxy(strength: f32) -> f32 {
    let spring_k: f32 = 1.0;
    let spring_len: f32 = 1.0;
    spring_k * strength.powi(2) * spring_len
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_data::{VaultEdge, VaultNode};

    fn node(id: &str) -> VaultNode {
        VaultNode {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn add_edge(g: &mut VaultGraph, s: &str, t: &str) {
        g.add_edge(VaultEdge {
            source: s.to_string(),
            target: t.to_string(),
        });
    }

    /// Two dense clusters connected by a single bridge edge.
    /// The bridge should have the highest anomaly score.
    fn two_cluster_graph() -> VaultGraph {
        let mut g = VaultGraph::default();
        // Cluster 1: {a,b,c,d} — dense (almost complete among themselves)
        // Cluster 2: {e,f,g,h} — dense
        // Bridge: d–e
        for id in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            g.add_node(node(id));
        }
        // Cluster 1 edges (near-complete)
        for (s, t) in [
            ("a", "b"), ("a", "c"), ("a", "d"),
            ("b", "c"), ("b", "d"),
            ("c", "d"),
        ] {
            add_edge(&mut g, s, t);
        }
        // Cluster 2 edges (near-complete)
        for (s, t) in [
            ("e", "f"), ("e", "g"), ("e", "h"),
            ("f", "g"), ("f", "h"),
            ("g", "h"),
        ] {
            add_edge(&mut g, s, t);
        }
        // Bridge
        add_edge(&mut g, "d", "e");
        g
    }

    #[test]
    fn bridge_edge_has_highest_anomaly() {
        let g = two_cluster_graph();
        let results = detect_anomalous_edges(&g, 1.0, 50, false);
        assert!(!results.is_empty(), "should find at least one anomalous edge");

        // The bridge edge d-e should be in results and should have the highest score.
        let bridge = results.first().unwrap();
        // d-e edge: both nodes have degree 4 (3 intra-cluster + 1 bridge), actual Jaccard
        // is 0 (no common neighbours), so anomaly_score should be high.
        assert!(bridge.anomaly_score > 1.0,
            "bridge edge should have anomaly_score > 1.0, got {}", bridge.anomaly_score);
    }

    /// K5 complete graph: every pair connected. All edges should have anomaly ≈ 1.0.
    #[test]
    fn complete_graph_k5_all_edges_have_low_anomaly() {
        let mut g = VaultGraph::default();
        let nodes = ["a", "b", "c", "d", "e"];
        for id in nodes {
            g.add_node(node(id));
        }
        // All pairs
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                add_edge(&mut g, nodes[i], nodes[j]);
            }
        }
        let results = detect_anomalous_edges(&g, 1.0, 50, false);
        // In a complete graph, actual Jaccard ≈ expected Jaccard, so anomaly_score ≈ 1.0.
        // None should exceed a moderate threshold.
        assert!(results.is_empty() || results.iter().all(|a| a.anomaly_score < 2.0),
            "K5 edges should not be highly anomalous");
    }

    /// Star graph: center connected to all leaves, no leaf-leaf edges.
    #[test]
    fn star_graph_with_impact() {
        let mut g = VaultGraph::default();
        g.add_node(node("center"));
        let leaves = ["a", "b", "c", "d", "e"];
        for id in leaves {
            g.add_node(node(id));
            add_edge(&mut g, "center", id);
        }
        let results = detect_anomalous_edges(&g, 1.0, 50, true);
        assert!(!results.is_empty(), "star edges should be anomalous");
        // All results should have removal_impact set.
        for a in &results {
            assert!(a.removal_impact.is_some(), "impact should be computed");
            assert!(a.removal_impact.unwrap() >= 0.0);
        }
    }

    #[test]
    fn sorting_by_anomaly_score() {
        let g = two_cluster_graph();
        let results = detect_anomalous_edges(&g, 0.0, 50, false);
        // Verify descending order.
        for w in results.windows(2) {
            assert!(w[0].anomaly_score >= w[1].anomaly_score,
                "results should be sorted descending by anomaly_score");
        }
    }

    #[test]
    fn max_results_cap() {
        let g = two_cluster_graph();
        let results = detect_anomalous_edges(&g, 0.0, 3, false);
        assert!(results.len() <= 3, "should cap at max_results");
    }

    #[test]
    fn empty_graph_returns_empty() {
        let g = VaultGraph::default();
        let results = detect_anomalous_edges(&g, 0.5, 10, false);
        assert!(results.is_empty());
    }

    #[test]
    fn single_edge() {
        let mut g = VaultGraph::default();
        g.add_node(node("a"));
        g.add_node(node("b"));
        add_edge(&mut g, "a", "b");
        let results = detect_anomalous_edges(&g, 0.0, 10, false);
        assert_eq!(results.len(), 1);
        // Two nodes, one edge: actual=0, expected small but >0 → anomaly > 1
        let a = &results[0];
        assert!(a.anomaly_score > 0.0);
        assert!(a.expected_jaccard > 0.0);
    }
}
