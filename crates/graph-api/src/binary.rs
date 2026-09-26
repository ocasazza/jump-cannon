//! Binary buffer serializers for hot-path bulk numeric data.
//!
//! These endpoints serve raw little-endian f32/u32 arrays — Cosmograph and
//! similar libs accept Float32Array/Uint32Array directly without parsing.
//
// Future: when backend lives on luna, these endpoints stay binary; the wire
// cost (and parse cost) is ~10x lower than JSON.

use std::collections::{BTreeSet, HashMap, HashSet};

use data_loader::EdgeTypeSchema;
use vault_data::VaultGraph;

/// Flat [x0, y0, x1, y1, ...] little-endian f32 buffer.
pub fn positions_buffer(graph: &VaultGraph) -> Vec<u8> {
    let mut out = Vec::with_capacity(graph.nodes.len() * 8);
    for node in graph.nodes.values() {
        out.extend_from_slice(&node.x.to_le_bytes());
        out.extend_from_slice(&node.y.to_le_bytes());
    }
    out
}

/// Flat [src_idx, tgt_idx, ...] little-endian u32 buffer using dense node indices.
pub fn edges_buffer(graph: &VaultGraph, id_to_idx: &HashMap<String, u32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(graph.edges.len() * 8);
    for edge in &graph.edges {
        if let (Some(&s), Some(&t)) = (id_to_idx.get(&edge.source), id_to_idx.get(&edge.target)) {
            out.extend_from_slice(&s.to_le_bytes());
            out.extend_from_slice(&t.to_le_bytes());
        }
    }
    out
}

/// Per-edge kind indices parallel to [`edges_buffer`]: one little-endian
/// `u16` per served edge, `0` for an untyped edge and `k` for
/// `palette[k - 1]`.
///
/// The palette is the importer's declared `edge_types` in declaration order,
/// followed by any kind the graph carries that the schema does not declare,
/// sorted. Declared-first keeps a kind's index — and so its color — stable
/// across reloads even when the data churns; the sorted tail keeps the
/// undeclared case deterministic. The palette is capped at `u16::MAX` kinds;
/// anything beyond the cap is served untyped.
pub struct EdgeKinds {
    pub palette: Vec<String>,
    pub indices: Vec<u8>,
}

pub fn edge_kinds(
    graph: &VaultGraph,
    id_to_idx: &HashMap<String, u32>,
    declared: &[EdgeTypeSchema],
) -> EdgeKinds {
    let declared_keys: HashSet<&str> = declared.iter().map(|edge_type| edge_type.key.as_str()).collect();
    let mut extra: BTreeSet<&str> = BTreeSet::new();
    for edge in &graph.edges {
        if let Some(kind) = edge.kind.as_deref() {
            if !declared_keys.contains(kind) {
                extra.insert(kind);
            }
        }
    }
    let mut palette: Vec<String> = declared.iter().map(|edge_type| edge_type.key.clone()).collect();
    palette.extend(extra.into_iter().map(str::to_owned));
    palette.truncate(usize::from(u16::MAX));
    let slots: HashMap<&str, u16> = palette
        .iter()
        .enumerate()
        .map(|(i, kind)| (kind.as_str(), i as u16 + 1))
        .collect();

    let mut indices = Vec::with_capacity(graph.edges.len() * 2);
    for edge in &graph.edges {
        if id_to_idx.contains_key(&edge.source) && id_to_idx.contains_key(&edge.target) {
            let slot = edge
                .kind
                .as_deref()
                .and_then(|kind| slots.get(kind).copied())
                .unwrap_or(0);
            indices.extend_from_slice(&slot.to_le_bytes());
        }
    }
    EdgeKinds { palette, indices }
}

/// Per-metric flat f32 buffer. Returns None if the metric name is unknown.
///
/// The `tag` metric is a per-node categorical bucket id derived from the
/// node's **primary tag** — the first tag in lexicographic byte order
/// drawn from `node.meta.tags`. We pick the first sorted tag (rather
/// than the array's natural order, which mirrors frontmatter authoring
/// order) so the bucket assignment is deterministic regardless of how
/// the user wrote the YAML. Untagged nodes hash to bucket `0`. The
/// renderer mirrors this tiebreaker in
/// `crate::ui::field_index::FieldIndex::tag_primary_metric` so the
/// client-side and server-side derivations agree.
pub fn metric_buffer(graph: &VaultGraph, name: &str) -> Option<Vec<u8>> {
    // `community_levels` reports the dendrogram depth L (a single f32), not a
    // per-node vector. `community_l{k}` returns the per-node community ids at
    // level k (level 0 coarsest = `community`; higher k finer), or None when
    // k is out of range so the route answers 404.
    if name == "community_levels" {
        let l = graph.community_levels.len() as f32;
        return Some(l.to_le_bytes().to_vec());
    }
    if let Some(suffix) = name.strip_prefix("community_l") {
        let k: usize = suffix.parse().ok()?;
        let level = graph.community_levels.get(k)?;
        let mut out = Vec::with_capacity(level.len() * 4);
        for &id in level {
            out.extend_from_slice(&(id as f32).to_le_bytes());
        }
        return Some(out);
    }

    let mut out = Vec::with_capacity(graph.nodes.len() * 4);
    for node in graph.nodes.values() {
        let v: f32 = match name {
            "degree" => node.metrics.degree as f32,
            "indegree" => node.metrics.indegree as f32,
            "outdegree" => node.metrics.outdegree as f32,
            "pagerank" => node.metrics.pagerank as f32,
            "betweenness" => node.metrics.betweenness as f32,
            "kcore" => node.metrics.kcore as f32,
            "community" => node.metrics.community as f32,
            "wcc" => node.metrics.wcc as f32,
            "tag" => primary_tag_bucket(&node.meta.tags),
            _ => return None,
        };
        out.extend_from_slice(&v.to_le_bytes());
    }
    Some(out)
}

/// Hash the lexicographically-first tag in `tags` to a `u32` bucket id
/// (cast to f32 for the wire format). Empty list → bucket `0`. The
/// hasher (`DefaultHasher`) is **not** stable across rustc versions —
/// we never persist these values, so process-stable is sufficient.
fn primary_tag_bucket(tags: &[String]) -> f32 {
    let mut sorted: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    let Some(primary) = sorted.first() else {
        return 0.0;
    };
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    primary.hash(&mut h);
    (h.finish() as u32) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_data::VaultNode;

    fn decode_f32(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    #[test]
    fn community_level_buffers() {
        let mut g = VaultGraph::new();
        // Coarsest community per node, in insertion order.
        let comms = [0usize, 0, 1, 1];
        for (i, c) in comms.iter().enumerate() {
            let mut node = VaultNode { id: format!("n{i}"), ..Default::default() };
            node.metrics.community = *c;
            g.add_node(node);
        }
        // Two dendrogram levels: level 0 coarsest (== community), level 1 finer.
        g.community_levels = vec![vec![0, 0, 1, 1], vec![0, 1, 2, 3]];

        // `community_levels` reports the depth L as a single f32.
        let levels_buf = metric_buffer(&g, "community_levels").expect("community_levels");
        assert_eq!(decode_f32(&levels_buf), vec![2.0], "community_levels must decode to L");

        // `community_l0` bytes are byte-identical to the `community` metric bytes.
        let l0 = metric_buffer(&g, "community_l0").expect("community_l0");
        let community = metric_buffer(&g, "community").expect("community");
        assert_eq!(l0, community, "community_l0 bytes must equal community bytes");

        // `community_l1` decodes to the finer level's per-node ids.
        let l1 = metric_buffer(&g, "community_l1").expect("community_l1");
        assert_eq!(decode_f32(&l1), vec![0.0, 1.0, 2.0, 3.0]);

        // Out-of-range level is unknown, so the route answers 404.
        assert!(metric_buffer(&g, "community_l2").is_none());
    }

    #[test]
    fn edge_kind_indices_follow_the_edges_buffer_and_a_declared_first_palette() {
        use vault_data::VaultEdge;

        let mut g = VaultGraph::new();
        for id in ["a", "b", "c"] {
            g.add_node(VaultNode { id: id.into(), ..Default::default() });
        }
        let typed = |s: &str, t: &str, kind: &str| VaultEdge {
            kind: Some(kind.into()),
            ..VaultEdge::new(s, t)
        };
        g.add_edge(typed("a", "b", "pursues"));
        g.add_edge(VaultEdge::new("b", "c"));
        g.add_edge(typed("c", "a", "zeta")); // undeclared
        g.add_edge(typed("a", "ghost", "in_repo")); // dropped from both buffers
        g.add_edge(typed("a", "c", "alpha")); // undeclared, sorts before zeta
        g.add_edge(typed("b", "a", "in_repo"));

        let id_to_idx: HashMap<String, u32> =
            g.nodes.keys().enumerate().map(|(i, id)| (id.clone(), i as u32)).collect();
        let declared = [
            EdgeTypeSchema::directed("in_repo", ""),
            EdgeTypeSchema::directed("pursues", ""),
        ];
        let kinds = edge_kinds(&g, &id_to_idx, &declared);

        assert_eq!(kinds.palette, ["in_repo", "pursues", "alpha", "zeta"]);
        let decoded: Vec<u16> = kinds
            .indices
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        // One slot per served edge, in `/graph/edges` order: 0 = untyped.
        assert_eq!(decoded, [2, 0, 4, 3, 1]);
        assert_eq!(decoded.len() * 8, edges_buffer(&g, &id_to_idx).len());
    }
}
