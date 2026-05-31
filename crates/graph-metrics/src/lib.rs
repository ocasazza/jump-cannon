mod degree;
mod pagerank;
mod betweenness;
mod kcore;
mod wcc;
mod louvain;
mod edge_strength;

pub use degree::compute_degree;
pub use pagerank::compute_pagerank;
pub use betweenness::compute_betweenness;
pub use kcore::compute_kcore;
pub use wcc::compute_wcc;
pub use louvain::compute_louvain;
pub use edge_strength::{compute_edge_strength, EdgeStrength, EdgeStrengthKind};

use vault_data::VaultGraph;

/// Run all metrics in dependency order and write them into graph.nodes[*].metrics.
pub fn compute_all(graph: &mut VaultGraph) {
    compute_degree(graph);
    compute_pagerank(graph, 0.85, 50);
    compute_betweenness(graph, 500);
    compute_kcore(graph);
    compute_wcc(graph);
    compute_louvain(graph, 10);
}

#[cfg(test)]
mod tests;
