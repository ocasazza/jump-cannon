//! In-browser graph metrics for client-owned graphs.
//!
//! graph-api computes the metric buffers when it loads a graph; a graph
//! generated or imported in the browser never reaches it. This module runs
//! the same `graph-metrics` algorithms on the client graph so the Style
//! panel's metric-driven modes (Community, PageRank, Degree, …) work without
//! a server. Betweenness is skipped above [`BETWEENNESS_MAX_NODES`] — 500
//! BFS passes over a large connected graph would freeze the UI thread.

use std::collections::HashMap;

use vault_data::{VaultEdge, VaultGraph, VaultNode};

use crate::GraphData;

/// Above this node count the `betweenness` buffer is not computed.
pub(crate) const BETWEENNESS_MAX_NODES: usize = 10_000;

/// Per-node metric buffers in renderer node order, keyed by graph-api's
/// binary-cache names. Betweenness is present only under the node cap.
pub(crate) fn compute(graph: &GraphData) -> HashMap<String, Vec<f32>> {
    let n = graph.n_nodes as usize;
    let mut out = HashMap::new();
    if n == 0 {
        return out;
    }
    let id = |i: u32| graph.ids[i as usize].clone();
    let mut vg = VaultGraph::new();
    for i in 0..graph.n_nodes {
        vg.add_node(VaultNode { id: id(i), ..Default::default() });
    }
    for pair in graph.scene.edges.chunks_exact(2) {
        vg.add_edge(VaultEdge::new(id(pair[0]), id(pair[1])));
    }

    graph_metrics::compute_degree(&mut vg);
    graph_metrics::compute_pagerank(&mut vg, 0.85, 50);
    if n <= BETWEENNESS_MAX_NODES {
        graph_metrics::compute_betweenness(&mut vg, 500);
    }
    graph_metrics::compute_kcore(&mut vg);
    graph_metrics::compute_wcc(&mut vg);
    graph_metrics::compute_louvain(&mut vg, 10);

    let mut collect = |name: &str, f: &dyn Fn(&vault_data::VaultNode) -> f32| {
        out.insert(name.to_string(), vg.nodes.values().map(f).collect());
    };
    collect("degree", &|n| n.metrics.degree as f32);
    collect("indegree", &|n| n.metrics.indegree as f32);
    collect("outdegree", &|n| n.metrics.outdegree as f32);
    collect("pagerank", &|n| n.metrics.pagerank as f32);
    collect("betweenness", &|n| n.metrics.betweenness as f32);
    collect("kcore", &|n| n.metrics.kcore as f32);
    collect("wcc", &|n| n.metrics.wcc as f32);
    collect("community", &|n| n.metrics.community as f32);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    fn graph() -> GraphData {
        let mut g = GraphData {
            graph_revision: None,
            n_nodes: 4,
            n_edges: 3,
            num_communities: 0,
            num_wcc: 1,
            ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            id_to_idx: Map::new(),
            scene: crate::render::Scene {
                positions: vec![0.0; 12],
                edges: vec![0, 1, 1, 2, 2, 0],
                colors: vec![0.0; 16],
                sizes: vec![0.0; 4],
                node_physics: None,
                edge_physics: None,
            },
        };
        g.id_to_idx = g.ids.iter().enumerate().map(|(i, k)| (k.clone(), i as u32)).collect();
        g
    }

    #[test]
    fn computes_the_served_metric_keys() {
        let m = compute(&graph());
        for key in ["degree", "indegree", "outdegree", "pagerank", "betweenness", "kcore", "wcc", "community"] {
            assert_eq!(m[key].len(), 4, "{key}");
        }
        // Triangle a-b-c plus isolated d: degree and wcc are exact.
        assert_eq!(m["degree"], vec![2.0, 2.0, 2.0, 0.0]);
        assert_eq!(m["wcc"], vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(m["kcore"], vec![2.0, 2.0, 2.0, 0.0]);
    }
}
