use std::collections::HashMap;
use vault_data::VaultGraph;

fn find(parent: &mut Vec<usize>, x: usize) -> usize {
    if parent[x] != x {
        parent[x] = find(parent, parent[x]);
    }
    parent[x]
}

pub fn compute_wcc(graph: &mut VaultGraph) {
    let ids: Vec<String> = graph.nodes.keys().cloned().collect();
    let n = ids.len();
    if n == 0 { return; }

    let idx: HashMap<String, usize> = ids.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

    let mut parent: Vec<usize> = (0..n).collect();

    for edge in &graph.edges {
        if let (Some(&si), Some(&ti)) = (idx.get(&edge.source), idx.get(&edge.target)) {
            let rs = find(&mut parent, si);
            let rt = find(&mut parent, ti);
            if rs != rt {
                parent[rs] = rt;
            }
        }
    }

    // Canonicalize component ids
    let mut comp_map: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;

    for (i, id) in ids.iter().enumerate() {
        let root = find(&mut parent, i);
        let comp = *comp_map.entry(root).or_insert_with(|| {
            let c = next_id;
            next_id += 1;
            c
        });
        if let Some(node) = graph.nodes.get_mut(id) {
            node.metrics.wcc = comp;
        }
    }

    graph.num_wcc = next_id;
}
