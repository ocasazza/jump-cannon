use std::collections::HashMap;
use vault_data::VaultGraph;

pub fn compute_kcore(graph: &mut VaultGraph) {
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let n = ids.len();
    if n == 0 { return; }

    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let mut degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            adj[si].push(ti);
            adj[ti].push(si);
            degree[si] += 1;
            degree[ti] += 1;
        }
    }

    let mut core = degree.clone();
    let removed = vec![false; n];
    let _ = removed; // unused — kept for clarity

    loop {
        let mut changed = false;
        for v in 0..n {
            let effective = adj[v].iter().filter(|&&w| core[w] >= core[v]).count();
            if effective < core[v] {
                core[v] = effective;
                changed = true;
            }
        }
        if !changed { break; }
    }

    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.kcore = core[i];
        }
    }
}
