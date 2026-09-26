use vault_data::{VaultEdge, VaultGraph, VaultNode};

fn make_triangle() -> VaultGraph {
    let mut g = VaultGraph::new();
    for id in ["a", "b", "c"] {
        g.add_node(VaultNode { id: id.to_string(), ..Default::default() });
    }
    g.add_edge(VaultEdge::new("a", "b"));
    g.add_edge(VaultEdge::new("b", "c"));
    g.add_edge(VaultEdge::new("c", "a"));
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
    g.add_edge(VaultEdge::new("a", "b"));
    g.add_edge(VaultEdge::new("b", "c"));
    g.add_edge(VaultEdge::new("c", "a"));
    // Clique 2
    g.add_edge(VaultEdge::new("d", "e"));
    g.add_edge(VaultEdge::new("e", "f"));
    g.add_edge(VaultEdge::new("f", "d"));
    // Bridge
    g.add_edge(VaultEdge::new("c", "d"));
    crate::compute_louvain(&mut g, 20);
    // Should find 2 communities
    assert_eq!(g.num_communities, 2, "expected 2 communities, got {}", g.num_communities);
}

#[test]
fn unit_louvain_five_clusters() {
    let mut g = VaultGraph::new();
    for c in 0..5 {
        for i in 0..20 {
            g.add_node(VaultNode { id: format!("c{}_n{}", c, i), ..Default::default() });
        }
    }
    // Intra-cluster: dense
    for c in 0..5 {
        for i in 0..20 {
            for j in (i+1)..20 {
                g.add_edge(VaultEdge::new(
                    format!("c{}_n{}", c, i),
                    format!("c{}_n{}", c, j),
                ));
            }
        }
    }
    // Inter-cluster: 1 edge between cluster reps
    for c1 in 0..5 {
        for c2 in (c1+1)..5 {
            g.add_edge(VaultEdge::new(format!("c{}_n0", c1), format!("c{}_n0", c2)));
        }
    }
    crate::compute_louvain(&mut g, 20);
    assert_eq!(g.num_communities, 5, "expected 5 communities, got {}", g.num_communities);
}

#[test]
fn unit_louvain_community_levels_dendrogram() {
    // Two disjoint 6-cliques joined by a single bridge edge.
    let mut g = VaultGraph::new();
    for c in 0..2 {
        for i in 0..6 {
            g.add_node(VaultNode { id: format!("c{}_n{}", c, i), ..Default::default() });
        }
    }
    for c in 0..2 {
        for i in 0..6 {
            for j in (i + 1)..6 {
                g.add_edge(VaultEdge::new(
                    format!("c{}_n{}", c, i),
                    format!("c{}_n{}", c, j),
                ));
            }
        }
    }
    // Bridge connecting the two cliques.
    g.add_edge(VaultEdge::new("c0_n0", "c1_n0"));

    crate::compute_louvain(&mut g, 20);

    let levels = g.community_levels.clone();
    let l = levels.len();
    assert!(l >= 1, "expected at least one dendrogram level, got {l}");

    let n = g.nodes.len();
    for level in &levels {
        assert_eq!(level.len(), n, "each level must carry one id per node");
    }

    // Level 0 (coarsest) is byte-identical to metrics.community, in node order.
    let community: Vec<u32> = g.nodes.values().map(|node| node.metrics.community as u32).collect();
    assert_eq!(levels[0], community, "level 0 must equal metrics.community");

    // Every level's ids are compacted to a contiguous 0..k range.
    for (k, level) in levels.iter().enumerate() {
        let mut distinct: Vec<u32> =
            level.iter().copied().collect::<std::collections::HashSet<_>>().into_iter().collect();
        distinct.sort_unstable();
        let expected: Vec<u32> = (0..distinct.len() as u32).collect();
        assert_eq!(distinct, expected, "level {k} ids must be compacted to 0..k-1");
    }

    // Higher k is finer: every class at level k+1 refines a class at level k.
    // Nodes sharing a finer community must share the coarser one too.
    for k in 0..l.saturating_sub(1) {
        let coarse = &levels[k];
        let fine = &levels[k + 1];
        for a in 0..n {
            for b in (a + 1)..n {
                if fine[a] == fine[b] {
                    assert_eq!(
                        coarse[a], coarse[b],
                        "level {} must refine level {}: nodes {a},{b} share a finer community but not the coarser one",
                        k + 1, k
                    );
                }
            }
        }
    }
}

#[test]
fn unit_kcore_triangle() {
    let mut g = make_triangle();
    crate::compute_kcore(&mut g);
    for id in ["a", "b", "c"] {
        assert!(g.nodes[id].metrics.kcore >= 1, "triangle nodes should have k-core >= 1");
    }
}
