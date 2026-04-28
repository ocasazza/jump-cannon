use std::collections::HashMap;
use vault_data::VaultGraph;

pub fn compute_louvain(graph: &mut VaultGraph, max_iter: usize) {
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let n = ids.len();
    if n == 0 { return; }

    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut total_weight = 0.0f64;
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            adj[si].push((ti, 1.0));
            adj[ti].push((si, 1.0));
            total_weight += 1.0;
        }
    }
    if total_weight == 0.0 {
        for (i, id) in ids.iter().enumerate() {
            if let Some(node) = graph.nodes.get_mut(id) {
                node.metrics.community = i;
            }
        }
        graph.num_communities = n;
        return;
    }
    let m2 = 2.0 * total_weight;

    let mut community: Vec<usize> = (0..n).collect();
    let ki: Vec<f64> = adj.iter().map(|nbrs| nbrs.iter().map(|(_, w)| w).sum()).collect();

    // sigma_in[c] = sum of weights inside community c
    // sigma_tot[c] = sum of ki for all nodes in community c
    let mut sigma_in: Vec<f64> = vec![0.0; n];
    let mut sigma_tot: Vec<f64> = ki.clone();

    for _iter in 0..max_iter {
        let mut moved = false;
        for v in 0..n {
            let cv = community[v];
            // Compute k_v_in: sum of weights from v to its current community
            let k_v_in: f64 = adj[v].iter()
                .filter(|(w, _)| community[*w] == cv)
                .map(|(_, wt)| wt)
                .sum();

            // Remove v from its community
            sigma_in[cv] -= k_v_in * 2.0;
            sigma_tot[cv] -= ki[v];

            // Find best community among neighbors
            let mut best_c = cv;
            let mut best_gain = 0.0f64;

            // Collect neighbor communities
            let mut nbr_weights: HashMap<usize, f64> = HashMap::new();
            for &(w, wt) in &adj[v] {
                *nbr_weights.entry(community[w]).or_default() += wt;
            }

            for (c, k_v_c) in &nbr_weights {
                let gain = k_v_c - ki[v] * sigma_tot[*c] / m2;
                if gain > best_gain {
                    best_gain = gain;
                    best_c = *c;
                }
            }

            // Add v to best community
            community[v] = best_c;
            let k_v_new_in: f64 = adj[v].iter()
                .filter(|(w, _)| community[*w] == best_c)
                .map(|(_, wt)| wt)
                .sum();
            sigma_in[best_c] += k_v_new_in * 2.0;
            sigma_tot[best_c] += ki[v];

            if best_c != cv { moved = true; }
        }
        if !moved { break; }
    }

    // Compact community ids to 0..num_communities
    let mut id_map: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    for v in 0..n {
        let mapped = *id_map.entry(community[v]).or_insert_with(|| {
            let c = next_id;
            next_id += 1;
            c
        });
        if let Some(node) = graph.nodes.get_mut(&ids[v]) {
            node.metrics.community = mapped;
        }
    }
    graph.num_communities = next_id;
}
