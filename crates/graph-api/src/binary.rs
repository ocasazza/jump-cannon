//! Binary buffer serializers for hot-path bulk numeric data.
//!
//! These endpoints serve raw little-endian f32/u32 arrays — Cosmograph and
//! similar libs accept Float32Array/Uint32Array directly without parsing.
//
// Future: when backend lives on luna, these endpoints stay binary; the wire
// cost (and parse cost) is ~10x lower than JSON.

use std::collections::HashMap;
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

/// Sorted, deduplicated table of distinct edge kinds in the graph. Shared
/// by the JSON discovery endpoint and the binary per-edge index buffer so
/// the two always agree on indices.
pub fn edge_kinds_table(graph: &VaultGraph) -> Vec<String> {
    let mut kinds: Vec<&str> = graph
        .edges
        .iter()
        .filter_map(|edge| edge.kind.as_deref())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds.into_iter().map(str::to_string).collect()
}

/// Per-edge kind-table index buffer, aligned edge-for-edge with
/// [`edges_buffer`] (same iteration and the same endpoint filter). An edge
/// without a kind encodes `u32::MAX`.
pub fn edge_kinds_buffer(
    graph: &VaultGraph,
    id_to_idx: &HashMap<String, u32>,
    kinds_table: &[String],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(graph.edges.len() * 4);
    for edge in &graph.edges {
        if !id_to_idx.contains_key(&edge.source) || !id_to_idx.contains_key(&edge.target) {
            continue;
        }
        let idx = edge
            .kind
            .as_deref()
            .and_then(|kind| kinds_table.iter().position(|k| k == kind).map(|i| i as u32))
            .unwrap_or(u32::MAX);
        out.extend_from_slice(&idx.to_le_bytes());
    }
    out
}

/// Sorted, deduplicated table of distinct node types (`meta.doctype`).
/// Shared by the JSON discovery endpoint and the binary per-node index
/// buffer so the two always agree on indices.
pub fn node_types_table(graph: &VaultGraph) -> Vec<String> {
    let mut types: Vec<&str> = graph
        .nodes
        .values()
        .filter_map(|node| node.meta.doctype.as_deref())
        .collect();
    types.sort_unstable();
    types.dedup();
    types.into_iter().map(str::to_string).collect()
}

/// Per-node type-table index buffer in node (id_to_idx) order. A node
/// without a type encodes `u32::MAX`.
pub fn node_types_buffer(graph: &VaultGraph, types_table: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(graph.nodes.len() * 4);
    for node in graph.nodes.values() {
        let idx = node
            .meta
            .doctype
            .as_deref()
            .and_then(|t| types_table.iter().position(|k| k == t).map(|i| i as u32))
            .unwrap_or(u32::MAX);
        out.extend_from_slice(&idx.to_le_bytes());
    }
    out
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
