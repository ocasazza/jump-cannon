use vault_data::{VaultEdge, VaultGraph, VaultNode};
use graph_metrics::compute_all;

#[test]
fn integration_metrics_pipeline() {
    let mut g = VaultGraph::new();
    for id in ["a", "b", "c"] {
        g.add_node(VaultNode { id: id.to_string(), ..Default::default() });
    }
    g.add_edge(VaultEdge { source: "a".into(), target: "b".into() });
    g.add_edge(VaultEdge { source: "b".into(), target: "c".into() });
    compute_all(&mut g);
    // PageRank should sum to ~1
    let pr_sum: f64 = g.nodes.values().map(|n| n.metrics.pagerank).sum();
    assert!((pr_sum - 1.0).abs() < 1e-5, "PageRank sum={pr_sum}");
    // All nodes should have a community assigned
    assert!(g.nodes.values().all(|n| n.metrics.community < 10));
}
