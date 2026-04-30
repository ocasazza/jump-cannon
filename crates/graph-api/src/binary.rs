//! Binary buffer serializers for hot-path bulk numeric data.
//!
//! These endpoints serve raw little-endian f32/u32 arrays — Cosmograph and
//! similar libs accept Float32Array/Uint32Array directly without parsing.
//
// Future: when backend lives on luna, these endpoints stay binary; the wire
// cost (and parse cost) is ~10x lower than JSON.

use vault_data::VaultGraph;
use std::collections::HashMap;

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

/// Per-metric flat f32 buffer. Returns None if the metric name is unknown.
pub fn metric_buffer(graph: &VaultGraph, name: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(graph.nodes.len() * 4);
    for node in graph.nodes.values() {
        let v: f32 = match name {
            "degree"      => node.metrics.degree as f32,
            "indegree"    => node.metrics.indegree as f32,
            "outdegree"   => node.metrics.outdegree as f32,
            "pagerank"    => node.metrics.pagerank as f32,
            "betweenness" => node.metrics.betweenness as f32,
            "kcore"       => node.metrics.kcore as f32,
            "community"   => node.metrics.community as f32,
            "wcc"         => node.metrics.wcc as f32,
            _ => return None,
        };
        out.extend_from_slice(&v.to_le_bytes());
    }
    Some(out)
}
