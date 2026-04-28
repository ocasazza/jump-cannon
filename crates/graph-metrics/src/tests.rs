use vault_data::{VaultEdge, VaultGraph, VaultNode};

fn make_triangle() -> VaultGraph {
    let mut g = VaultGraph::new();
    for id in ["a", "b", "c"] {
        g.add_node(VaultNode { id: id.to_string(), ..Default::default() });
    }
    g.add_edge(VaultEdge { source: "a".into(), target: "b".into() });
    g.add_edge(VaultEdge { source: "b".into(), target: "c".into() });
    g.add_edge(VaultEdge { source: "c".into(), target: "a".into() });
    g
}

#[test]
fn unit_degree_triangle() {
    let mut g = make_triangle();
    crate::compute_degree(&mut g);
    for id in ["a", "b", "c"] {
        let m = &g.nodes[id].metrics;
        assert_eq!(m.degree, 2, "degree should be 2 for triangle node {id}");
    }
}

#[test]
fn unit_pagerank_sums_to_one() {
    let mut g = make_triangle();
    crate::compute_pagerank(&mut g, 0.85, 50);
    let total: f64 = g.nodes.values().map(|n| n.metrics.pagerank).sum();
    assert!((total - 1.0).abs() < 1e-6, "PageRank must sum to 1, got {total}");
}

#[test]
fn unit_wcc_triangle_is_one_component() {
    let mut g = make_triangle();
    crate::compute_wcc(&mut g);
    assert_eq!(g.num_wcc, 1);
    let comps: std::collections::HashSet<usize> = g.nodes.values().map(|n| n.metrics.wcc).collect();
    assert_eq!(comps.len(), 1);
}

#[test]
fn unit_louvain_two_cliques() {
    // Two triangles connected by a single bridge edge
    let mut g = VaultGraph::new();
    for id in ["a","b","c","d","e","f"] {
        g.add_node(VaultNode { id: id.to_string(), ..Default::default() });
    }
    // Clique 1
    g.add_edge(VaultEdge { source: "a".into(), target: "b".into() });
    g.add_edge(VaultEdge { source: "b".into(), target: "c".into() });
    g.add_edge(VaultEdge { source: "c".into(), target: "a".into() });
    // Clique 2
    g.add_edge(VaultEdge { source: "d".into(), target: "e".into() });
    g.add_edge(VaultEdge { source: "e".into(), target: "f".into() });
    g.add_edge(VaultEdge { source: "f".into(), target: "d".into() });
    // Bridge
    g.add_edge(VaultEdge { source: "c".into(), target: "d".into() });
    crate::compute_louvain(&mut g, 20);
    // Should find 2 communities
    assert_eq!(g.num_communities, 2, "expected 2 communities, got {}", g.num_communities);
}

#[test]
fn unit_kcore_triangle() {
    let mut g = make_triangle();
    crate::compute_kcore(&mut g);
    for id in ["a", "b", "c"] {
        assert!(g.nodes[id].metrics.kcore >= 1, "triangle nodes should have k-core >= 1");
    }
}
