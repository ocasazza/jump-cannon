//! Concentric (by-degree) static layout.
//!
//! One-shot CPU solver. Groups nodes onto concentric rings keyed on the
//! chosen degree metric (highest score = innermost ring). Runs in
//! O(n + e): one pass over edges to accumulate degrees, one sort over
//! nodes, one placement pass.
//!
//! This is the new-pattern replacement for the legacy `concentric.rs`
//! that targets the deprecated `LayoutEngine` trait — do not import from
//! that file.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ConcentricMetric {
    #[default]
    Degree,
    InDegree,
    OutDegree,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConcentricSettings {
    pub metric: ConcentricMetric,
    pub min_radius: f32,
    pub level_spacing: f32,
    pub clockwise: bool,
    /// 0 = use distinct score values (one ring per distinct score).
    /// >0 = bucket scores linearly into this many rings.
    pub bucket_count: u32,
}

impl Default for ConcentricSettings {
    fn default() -> Self {
        Self {
            metric: ConcentricMetric::Degree,
            min_radius: 50.0,
            level_spacing: 80.0,
            clockwise: true,
            bucket_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConcentricLayout;

impl StaticLayout for ConcentricLayout {
    type Settings = ConcentricSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "concentric",
            kind: LayoutKind::Static,
            display_name: "Concentric (by degree)",
            description:
                "Group nodes onto concentric rings keyed on degree (or in/out-degree). Highest-degree nodes occupy the innermost ring.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
        // Stable id-sorted node order so packed positions line up with
        // the GPU positions buffer index-for-index.
        let mut node_order: Vec<&String> = graph.nodes.keys().collect();
        node_order.sort();
        let n = node_order.len();

        // Single pass over edges: accumulate (in_deg, out_deg) per node.
        let mut deg: HashMap<&str, (u32, u32)> = HashMap::with_capacity(n);
        for id in &node_order {
            deg.insert(id.as_str(), (0, 0));
        }
        for edge in graph.edges.values() {
            if let Some(slot) = deg.get_mut(edge.source.as_str()) {
                slot.1 = slot.1.saturating_add(1);
            }
            if let Some(slot) = deg.get_mut(edge.target.as_str()) {
                slot.0 = slot.0.saturating_add(1);
            }
        }

        // (output_index, score) — output_index is the position in the
        // id-sorted order so the final write can index directly.
        let mut scored: Vec<(usize, u32)> = Vec::with_capacity(n);
        for (i, id) in node_order.iter().enumerate() {
            let (in_d, out_d) = deg.get(id.as_str()).copied().unwrap_or((0, 0));
            let score = match settings.metric {
                ConcentricMetric::Degree => in_d.saturating_add(out_d),
                ConcentricMetric::InDegree => in_d,
                ConcentricMetric::OutDegree => out_d,
            };
            scored.push((i, score));
        }

        // Sort descending by score so the highest-scoring nodes land on
        // the innermost ring.
        scored.sort_by(|a, b| b.1.cmp(&a.1));

        // Build levels.
        let levels: Vec<Vec<usize>> = if settings.bucket_count == 0 {
            // Group runs of equal score → one level per distinct score.
            let mut out: Vec<Vec<usize>> = Vec::new();
            let mut i = 0;
            while i < scored.len() {
                let s = scored[i].1;
                let mut group: Vec<usize> = Vec::new();
                while i < scored.len() && scored[i].1 == s {
                    group.push(scored[i].0);
                    i += 1;
                }
                out.push(group);
            }
            out
        } else {
            // Fixed-width buckets across [min_score, max_score]. Highest
            // score = bucket 0 (innermost).
            let buckets = settings.bucket_count.max(1) as usize;
            let mut out: Vec<Vec<usize>> = vec![Vec::new(); buckets];
            if !scored.is_empty() {
                let max_score = scored.first().map(|(_, s)| *s).unwrap_or(0);
                let min_score = scored.last().map(|(_, s)| *s).unwrap_or(0);
                let range = (max_score - min_score) as f32;
                for (idx, score) in &scored {
                    let bucket = if range <= 0.0 {
                        0
                    } else {
                        // Fraction below max → bucket index (0 = innermost).
                        let f = (max_score - *score) as f32 / range;
                        let b = (f * buckets as f32).floor() as usize;
                        b.min(buckets - 1)
                    };
                    out[bucket].push(*idx);
                }
                // Drop empty leading/trailing buckets so radii stay tight.
                out.retain(|v| !v.is_empty());
            }
            out
        };

        // Place onto rings.
        let mut out: Vec<f32> = vec![0.0; n * 3];
        let tau = std::f32::consts::TAU;
        let dir: f32 = if settings.clockwise { 1.0 } else { -1.0 };
        for (k, level) in levels.iter().enumerate() {
            let radius = settings.min_radius + (k as f32) * settings.level_spacing;
            let count = level.len().max(1) as f32;
            for (j, &node_idx) in level.iter().enumerate() {
                let theta = dir * tau * (j as f32) / count;
                let x = theta.cos() * radius;
                let y = theta.sin() * radius;
                let base = node_idx * 3;
                out[base] = x;
                out[base + 1] = y;
                out[base + 2] = 0.0;
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

    #[test]
    fn star_graph_center_innermost() {
        // Star: "c" at center connected to 5 leaves. The center has the
        // highest degree → innermost ring at min_radius. Leaves all share
        // degree 1 → next ring at min_radius + level_spacing.
        let mut g = Graph::new();
        g.add_node(Node::new("c"));
        for i in 0..5 {
            let leaf = format!("l{i}");
            g.add_node(Node::new(leaf.clone()));
            g.add_edge(Edge::new(format!("e{i}"), "c", leaf));
        }

        let s = ConcentricSettings::default();
        let packed = ConcentricLayout::solve(&s, &g).expect("solve");
        assert_eq!(packed.len(), 6 * 3);

        // id-sorted order: ["c", "l0", "l1", "l2", "l3", "l4"]
        let mut ids: Vec<&String> = g.nodes.keys().collect();
        ids.sort();
        let pos = |id: &str| -> (f32, f32, f32) {
            let i = ids.iter().position(|s| s.as_str() == id).unwrap();
            (packed[i * 3], packed[i * 3 + 1], packed[i * 3 + 2])
        };

        let (cx, cy, cz) = pos("c");
        let r_c = (cx * cx + cy * cy).sqrt();
        assert!((r_c - s.min_radius).abs() < 1e-3, "center radius {r_c}");
        assert_eq!(cz, 0.0);

        let expected_leaf_r = s.min_radius + s.level_spacing;
        for i in 0..5 {
            let (x, y, z) = pos(&format!("l{i}"));
            let r = (x * x + y * y).sqrt();
            assert!(
                (r - expected_leaf_r).abs() < 1e-3,
                "leaf {i} radius {r} != {expected_leaf_r}"
            );
            assert_eq!(z, 0.0);
        }
    }

    #[test]
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let s = ConcentricSettings::default();
        let out = ConcentricLayout::solve(&s, &g).expect("solve");
        assert!(out.is_empty());
    }
}
