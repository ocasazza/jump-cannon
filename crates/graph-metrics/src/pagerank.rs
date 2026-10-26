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

    let mut new_scores = vec![0.0f64; n];
    for _ in 0..iters {
        // Dangling nodes spread their rank uniformly; fold that into the base.
        let dangling: f64 = out_neighbors
            .iter()
            .zip(&scores)
            .filter(|(neighbors, _)| neighbors.is_empty())
            .map(|(_, s)| s)
            .sum();
        new_scores.fill(((1.0 - damping) + damping * dangling) / n as f64);
        for (i, neighbors) in out_neighbors.iter().enumerate() {
            if !neighbors.is_empty() {
                let share = damping * scores[i] / neighbors.len() as f64;
                for &j in neighbors {
                    new_scores[j] += share;
                }
            }
        }
        std::mem::swap(&mut scores, &mut new_scores);
    }

    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.pagerank = scores[i];
        }
    }
}
