use std::collections::HashMap;
use vault_data::VaultGraph;

/// Core number of every node in the simple undirected graph underlying
/// `graph` (direction, parallel edges, and self-loops collapsed), by
/// Batagelj–Zaversnik bucket peeling in O(n + m).
pub fn compute_kcore(graph: &mut VaultGraph) {
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let n = ids.len();
    if n == 0 { return; }

    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            if si != ti {
                adj[si].push(ti);
                adj[ti].push(si);
            }
        }
    }
    for nbrs in &mut adj {
        nbrs.sort_unstable();
        nbrs.dedup();
    }

    // `order` holds nodes sorted by current degree; `bin_start[d]` is the
    // first slot of degree-d nodes and `pos[v]` is v's slot.
    let mut deg: Vec<usize> = adj.iter().map(Vec::len).collect();
    let max_deg = deg.iter().copied().max().unwrap_or(0);
    let mut bin_start = vec![0usize; max_deg + 1];
    for &d in &deg {
        bin_start[d] += 1;
    }
    let mut start = 0;
    for slot in bin_start.iter_mut() {
        let count = *slot;
        *slot = start;
        start += count;
    }
    let mut pos = vec![0usize; n];
    let mut order = vec![0usize; n];
    {
        let mut next = bin_start.clone();
        for v in 0..n {
            pos[v] = next[deg[v]];
            order[pos[v]] = v;
            next[deg[v]] += 1;
        }
    }

    for i in 0..n {
        let v = order[i];
        for &u in &adj[v] {
            if deg[u] > deg[v] {
                // Move u to the front of its bin, then shrink the bin by one.
                let du = deg[u];
                let first = bin_start[du];
                let w = order[first];
                if w != u {
                    order.swap(pos[u], first);
                    pos[w] = pos[u];
                    pos[u] = first;
                }
                bin_start[du] += 1;
                deg[u] -= 1;
            }
        }
    }

    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.kcore = deg[i];
        }
    }
}
