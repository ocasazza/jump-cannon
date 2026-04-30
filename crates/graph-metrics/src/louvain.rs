use std::collections::{HashMap, HashSet};
use vault_data::VaultGraph;

pub fn compute_louvain(graph: &mut VaultGraph, max_outer_iter: usize) {
    let n_orig = graph.nodes.len();
    if n_orig == 0 {
        return;
    }

    // 1. Build initial undirected weighted adjacency from VaultGraph edges.
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let id_to_idx: HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    let mut adj_raw: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n_orig];
    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (
            id_to_idx.get(edge.source.as_str()),
            id_to_idx.get(edge.target.as_str()),
        ) {
            if si == ti {
                continue;
            }
            adj_raw[si].push((ti, 1.0));
            adj_raw[ti].push((si, 1.0));
        }
    }

    // Collapse parallel edges into a single weighted edge per neighbor.
    let mut adj: Vec<Vec<(usize, f64)>> = Vec::with_capacity(n_orig);
    for nbrs in adj_raw.into_iter() {
        let mut combined: HashMap<usize, f64> = HashMap::with_capacity(nbrs.len());
        for (j, w) in nbrs {
            *combined.entry(j).or_insert(0.0) += w;
        }
        adj.push(combined.into_iter().collect());
    }

    // Handle the no-edges case explicitly.
    let total_weight: f64 = adj
        .iter()
        .flat_map(|nbrs| nbrs.iter().map(|(_, w)| *w))
        .sum();
    if total_weight <= 0.0 {
        for (i, id) in ids.iter().enumerate() {
            if let Some(node) = graph.nodes.get_mut(id) {
                node.metrics.community = i;
            }
        }
        graph.num_communities = n_orig;
        return;
    }

    // 2. Outer loop: alternate phase 1 + phase 2.
    let mut levels: Vec<Vec<usize>> = Vec::new();
    let mut current_adj = adj;

    for _outer in 0..max_outer_iter {
        let community = phase1_local_optimization(&current_adj);
        let unique = unique_count(&community);

        if unique == current_adj.len() {
            // Nothing improved this level — stop without recording a useless level.
            break;
        }

        let (super_adj, _compact) = phase2_aggregate(&current_adj, &community);

        // Push the compacted community vector so level-walking maps to 0..k indices.
        let compacted = compact_community(&community);
        levels.push(compacted);
        current_adj = super_adj;

        if current_adj.len() <= 1 {
            break;
        }
    }

    // 3. Walk level chain to compute final community per original node.
    let mut final_comm: Vec<usize> = (0..n_orig).collect();
    for level in &levels {
        for c in final_comm.iter_mut() {
            *c = level[*c];
        }
    }

    // 4. Compact final ids to 0..k.
    let mut compact: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    for c in final_comm.iter_mut() {
        let mapped = *compact.entry(*c).or_insert_with(|| {
            let v = next_id;
            next_id += 1;
            v
        });
        *c = mapped;
    }

    // 5. Write back to graph nodes.
    for (i, id) in ids.iter().enumerate() {
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.community = final_comm[i];
        }
    }
    graph.num_communities = next_id;
}

/// Phase 1: each node is moved to the neighbor community that maximizes
/// modularity gain. Iterates until no node moves (or guard hit).
///
/// Modularity gain (Blondel et al. 2008), with m = total edge weight = m2/2:
///     ΔQ(v → C) = k_v_in_C / m - ki[v] * sigma_tot[C] / (2 m^2)
/// where k_v_in_C is the sum of weights from v to nodes in C, computed
/// AFTER v has been removed from its current community (so sigma_tot[cv]
/// excludes ki[v] when evaluating "stay in cv").
fn phase1_local_optimization(adj: &[Vec<(usize, f64)>]) -> Vec<usize> {
    let n = adj.len();
    let mut community: Vec<usize> = (0..n).collect();

    let ki: Vec<f64> = adj
        .iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| *w).sum())
        .collect();
    let m2: f64 = ki.iter().sum::<f64>(); // 2m for undirected
    let m: f64 = m2 / 2.0;

    if m <= 0.0 {
        return community;
    }

    let mut sigma_tot: Vec<f64> = ki.clone();

    let mut changed = true;
    let mut guard = 0usize;
    while changed && guard < 50 {
        changed = false;
        guard += 1;

        for v in 0..n {
            let cv = community[v];

            // Sum of edge weights from v to each neighbor community.
            // Self-loops are stored with weight 2*w (doubled per the
            // undirected-adjacency convention used by ki); halve when
            // counting them as a single edge into v's own community.
            let mut k_v_to_c: HashMap<usize, f64> = HashMap::new();
            for &(u, w) in &adj[v] {
                let cu = community[u];
                let contrib = if u == v { w * 0.5 } else { w };
                *k_v_to_c.entry(cu).or_insert(0.0) += contrib;
            }

            // Remove v from its current community.
            sigma_tot[cv] -= ki[v];

            // Evaluate gain for each candidate community (including cv).
            // Default best = stay in cv with gain 0 (pre-move modularity reference).
            let mut best_c = cv;
            let mut best_gain = 0.0f64;

            for (&c, &k_v_in_c) in &k_v_to_c {
                let gain = k_v_in_c / m - ki[v] * sigma_tot[c] / (2.0 * m * m);
                if gain > best_gain {
                    best_gain = gain;
                    best_c = c;
                }
            }

            // Insert v into best community.
            sigma_tot[best_c] += ki[v];
            if best_c != cv {
                community[v] = best_c;
                changed = true;
            }
        }
    }

    community
}

/// Phase 2: aggregate `adj` into a super-graph where each community in
/// `community` becomes one node. Self-loops carry intra-community weight.
fn phase2_aggregate(
    adj: &[Vec<(usize, f64)>],
    community: &[usize],
) -> (Vec<Vec<(usize, f64)>>, HashMap<usize, usize>) {
    let mut compact: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    let comm_compacted: Vec<usize> = community
        .iter()
        .map(|c| {
            *compact.entry(*c).or_insert_with(|| {
                let v = next_id;
                next_id += 1;
                v
            })
        })
        .collect();
    let k = next_id;

    let mut super_adj_map: Vec<HashMap<usize, f64>> = vec![HashMap::new(); k];
    for (i, nbrs) in adj.iter().enumerate() {
        let ci = comm_compacted[i];
        for &(j, w) in nbrs {
            let cj = comm_compacted[j];
            // Each undirected edge is present as (i,j) AND (j,i) in adj.
            // Halving here yields total edge weight for inter-community
            // pairs and intra-community weight for self-loops (standard
            // undirected self-loop convention: each loop contributes twice
            // to its endpoint's degree, so the halved-sum equals the
            // intra-community edge weight, and degree gets 2× back via
            // both nbr entries summing without halving — but we DO halve,
            // matching the undirected adjacency convention used by phase1
            // where ki = sum(w) over the doubled adjacency = 2m).
            *super_adj_map[ci].entry(cj).or_insert(0.0) += w * 0.5;
        }
    }

    // For phase1 to see ki = 2m at the next level, self-loops must be
    // counted twice in the per-node degree. Convert: store self-loop weight
    // s, but emit it as a single (i,i,2s) entry so iteration yields 2s for
    // ki. We've halved above; double self-loops to restore the "edge
    // appears twice" convention.
    for (i, map) in super_adj_map.iter_mut().enumerate() {
        if let Some(w) = map.get_mut(&i) {
            *w *= 2.0;
        }
    }

    let super_adj: Vec<Vec<(usize, f64)>> = super_adj_map
        .into_iter()
        .map(|map| map.into_iter().collect())
        .collect();

    (super_adj, compact)
}

fn compact_community(community: &[usize]) -> Vec<usize> {
    let mut compact: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    community
        .iter()
        .map(|c| {
            *compact.entry(*c).or_insert_with(|| {
                let v = next_id;
                next_id += 1;
                v
            })
        })
        .collect()
}

fn unique_count(community: &[usize]) -> usize {
    community.iter().copied().collect::<HashSet<_>>().len()
}
