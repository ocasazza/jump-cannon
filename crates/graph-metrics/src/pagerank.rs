use std::collections::HashMap;
use vault_data::VaultGraph;

pub fn compute_pagerank(graph: &mut VaultGraph, damping: f64, iters: usize) {
    let n = graph.nodes.len();
    if n == 0 { return; }

    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let mut scores = vec![1.0 / n as f64; n];

    // Build adjacency: out_neighbors[i] = list of j
    let mut out_neighbors: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            out_neighbors[si].push(ti);
        }
    }

    for _ in 0..iters {
        let mut new_scores = vec![(1.0 - damping) / n as f64; n];
        for (i, neighbors) in out_neighbors.iter().enumerate() {
            if neighbors.is_empty() {
                // Dangling node: distribute evenly
                let share = damping * scores[i] / n as f64;
                for j in 0..n {
                    new_scores[j] += share;
                }
            } else {
                let share = damping * scores[i] / neighbors.len() as f64;
                for &j in neighbors {
                    new_scores[j] += share;
                }
            }
        }
        scores = new_scores;
    }

    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.pagerank = scores[i];
        }
    }
}
