//! Force-directed (fCoSE) static layout.
//!
//! One-shot CPU force-directed solver on the System-B [`StaticLayout`] trait
//! (replacing the legacy `FcoseLayoutEngine` that targeted the removed
//! `LayoutEngine`/`ForceDirectedLayout` traits). Repulsion is inverse-square
//! between all node pairs; attraction is a spring along each edge toward
//! `ideal_edge_length`; positions integrate with a fixed damping factor. For
//! small graphs (< 500 nodes) a post-pass nudges overlapping nodes apart.

use std::collections::HashMap;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

/// Solve-quality preset → iteration count (draft 30 / default 50 / proof 100,
/// matching the legacy `quality` string knob).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FcoseQuality {
    Draft,
    #[default]
    Default,
    Proof,
}

impl FcoseQuality {
    fn iterations(self) -> usize {
        match self {
            FcoseQuality::Draft => 30,
            FcoseQuality::Default => 50,
            FcoseQuality::Proof => 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FcoseSettings {
    pub node_repulsion: f64,
    pub ideal_edge_length: f64,
    pub node_overlap: f64,
    pub quality: FcoseQuality,
}

impl Default for FcoseSettings {
    fn default() -> Self {
        Self {
            node_repulsion: 4500.0,
            ideal_edge_length: 50.0,
            node_overlap: 10.0,
            quality: FcoseQuality::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FcoseLayout;

impl StaticLayout for FcoseLayout {
    type Settings = FcoseSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "fcose",
            kind: LayoutKind::Static,
            display_name: "Force-Directed (fCoSE)",
            description: "Force-directed layout optimized for compound graphs.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
        // Stable id-sorted node order so packed positions line up with the GPU
        // positions buffer index-for-index.
        let mut ids: Vec<&String> = graph.nodes.keys().collect();
        ids.sort();
        let n = ids.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let index: HashMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        // Seed positions in a disc of radius 100 (matches the legacy engine).
        let mut rng = rand::thread_rng();
        let mut pos: Vec<(f64, f64)> = Vec::with_capacity(n);
        for _ in 0..n {
            let angle = rng.gen::<f64>() * 2.0 * std::f64::consts::PI;
            let dist = rng.gen::<f64>() * 100.0;
            pos.push((dist * angle.cos(), dist * angle.sin()));
        }

        // Edge endpoints as index pairs (skip edges referencing unknown nodes).
        let edges: Vec<(usize, usize)> = graph
            .edges
            .values()
            .filter_map(|e| Some((*index.get(e.source.as_str())?, *index.get(e.target.as_str())?)))
            .collect();

        let node_repulsion = settings.node_repulsion;
        let ideal = settings.ideal_edge_length;
        let damping = 0.1;

        for _ in 0..settings.quality.iterations() {
            let mut force = vec![(0.0f64, 0.0f64); n];

            // Repulsion: inverse-square between all ordered pairs.
            for i in 0..n {
                for j in 0..n {
                    if i == j {
                        continue;
                    }
                    let dx = pos[i].0 - pos[j].0;
                    let dy = pos[i].1 - pos[j].1;
                    let d2 = dx * dx + dy * dy;
                    if d2 < 0.1 {
                        continue;
                    }
                    let f = node_repulsion / d2;
                    let inv = d2.sqrt();
                    force[i].0 += f * dx / inv;
                    force[i].1 += f * dy / inv;
                }
            }

            // Attraction: spring along edges toward ideal length.
            for &(s, t) in &edges {
                let dx = pos[t].0 - pos[s].0;
                let dy = pos[t].1 - pos[s].1;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < 0.1 {
                    continue;
                }
                let f = (dist - ideal) / 3.0;
                let fx = f * dx / dist;
                let fy = f * dy / dist;
                force[s].0 += fx;
                force[s].1 += fy;
                force[t].0 -= fx;
                force[t].1 -= fy;
            }

            for i in 0..n {
                pos[i].0 += force[i].0 * damping;
                pos[i].1 += force[i].1 * damping;
            }
        }

        // Overlap removal — O(n²); skipped for large graphs (legacy gated < 500).
        if n < 500 {
            let node_size = 10.0;
            let min_distance = node_size * 2.0 * (1.0 - settings.node_overlap / 100.0);
            let max_iterations = 50;
            let mut iteration = 0;
            let mut overlaps = true;
            while overlaps && iteration < max_iterations {
                overlaps = false;
                for i in 0..n {
                    for j in (i + 1)..n {
                        let dx = pos[j].0 - pos[i].0;
                        let dy = pos[j].1 - pos[i].1;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist < min_distance {
                            overlaps = true;
                            let f = min_distance - dist;
                            let (fx, fy) = if dist > 0.1 {
                                (f * dx / dist, f * dy / dist)
                            } else {
                                (rng.gen::<f64>() * 2.0 - 1.0, rng.gen::<f64>() * 2.0 - 1.0)
                            };
                            pos[i].0 -= fx / 2.0;
                            pos[i].1 -= fy / 2.0;
                            pos[j].0 += fx / 2.0;
                            pos[j].1 += fy / 2.0;
                        }
                    }
                }
                iteration += 1;
            }
        }

        let mut out = Vec::with_capacity(n * 3);
        for (x, y) in pos {
            out.push(x as f32);
            out.push(y as f32);
            out.push(0.0);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

    #[test]
    fn quality_iteration_counts() {
        assert_eq!(FcoseQuality::Draft.iterations(), 30);
        assert_eq!(FcoseQuality::Default.iterations(), 50);
        assert_eq!(FcoseQuality::Proof.iterations(), 100);
    }

    #[test]
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let out = FcoseLayout::solve(&FcoseSettings::default(), &g).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn packs_finite_triples_for_every_node() {
        let mut g = Graph::new();
        for i in 0..6 {
            g.add_node(Node::new(format!("n{i}")));
        }
        for i in 0..5 {
            g.add_edge(Edge::new(format!("e{i}"), format!("n{i}"), format!("n{}", i + 1)));
        }
        let out = FcoseLayout::solve(&FcoseSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 6 * 3);
        assert!(out.iter().all(|v| v.is_finite()));
        // z is always 0 for this 2D layout.
        for k in 0..6 {
            assert_eq!(out[k * 3 + 2], 0.0);
        }
    }
}
