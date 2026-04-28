use std::collections::{HashMap, VecDeque};
use vault_data::VaultGraph;

pub fn compute_betweenness(graph: &mut VaultGraph, k_sources: usize) {
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let n = ids.len();
    if n == 0 { return; }

    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    // Build undirected adjacency
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            adj[si].push(ti);
            adj[ti].push(si);
        }
    }

    let k = k_sources.min(n);
    let mut betweenness = vec![0.0f64; n];

    for s in 0..k {
        let mut stack = Vec::new();
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0.0f64; n];
        sigma[s] = 1.0;
        let mut dist = vec![-1i64; n];
        dist[s] = 0;
        let mut queue = VecDeque::new();
        queue.push_back(s);

        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for &w in &adj[v] {
                if dist[w] < 0 {
                    queue.push_back(w);
                    dist[w] = dist[v] + 1;
                }
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    pred[w].push(v);
                }
            }
        }

        let mut delta = vec![0.0f64; n];
        while let Some(w) = stack.pop() {
            for &v in &pred[w] {
                delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
            }
            if w != s {
                betweenness[w] += delta[w];
            }
        }
    }

    // Normalize
    let scale = if n > 2 { 1.0 / ((n - 1) as f64 * (n - 2) as f64) } else { 1.0 };
    let sample_scale = n as f64 / k as f64;

    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.betweenness = betweenness[i] * scale * sample_scale;
        }
    }
}
