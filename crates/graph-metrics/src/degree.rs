use vault_data::VaultGraph;

pub fn compute_degree(graph: &mut VaultGraph) {
    // Reset
    for node in graph.nodes.values_mut() {
        node.metrics.degree = 0;
        node.metrics.indegree = 0;
        node.metrics.outdegree = 0;
    }
    for edge in &graph.edges {
        if let Some(n) = graph.nodes.get_mut(&edge.source) {
            n.metrics.outdegree += 1;
            n.metrics.degree += 1;
        }
        if let Some(n) = graph.nodes.get_mut(&edge.target) {
            n.metrics.indegree += 1;
            n.metrics.degree += 1;
        }
    }
}
