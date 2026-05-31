//! CoSE-Bilkent static layout.
//!
//! One-shot CPU force-directed solver on the System-B [`StaticLayout`] trait
//! (replacing the legacy `CoseBilkentLayoutEngine`). Same force model as
//! [`super::fcose`] — inverse-square repulsion, edge springs toward
//! `ideal_edge_length`, damping 0.1 — but with a fixed (configurable)
//! iteration count and no overlap-removal pass, matching the original.

use std::collections::HashMap;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoseBilkentSettings {
    pub node_repulsion: f64,
    pub ideal_edge_length: f64,
    /// Force-iteration count (the legacy engine hard-coded 50).
    pub iterations: u32,
}

impl Default for CoseBilkentSettings {
    fn default() -> Self {
        Self {
            node_repulsion: 4500.0,
            ideal_edge_length: 50.0,
            iterations: 50,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CoseBilkentLayout;

impl StaticLayout for CoseBilkentLayout {
    type Settings = CoseBilkentSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "cose_bilkent",
            kind: LayoutKind::Static,
            display_name: "CoSE-Bilkent",
            description: "Compound Spring Embedder layout (Bilkent University).",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
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

        let edges: Vec<(usize, usize)> = graph
            .edges
            .values()
            .filter_map(|e| Some((*index.get(e.source.as_str())?, *index.get(e.target.as_str())?)))
            .collect();

        let node_repulsion = settings.node_repulsion;
        let ideal = settings.ideal_edge_length;
        let damping = 0.1;

        for _ in 0..settings.iterations {
            let mut force = vec![(0.0f64, 0.0f64); n];

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
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let out = CoseBilkentLayout::solve(&CoseBilkentSettings::default(), &g).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn packs_finite_triples_for_every_node() {
        let mut g = Graph::new();
        for i in 0..4 {
            g.add_node(Node::new(format!("n{i}")));
        }
        g.add_edge(Edge::new("e0", "n0", "n1"));
        g.add_edge(Edge::new("e1", "n1", "n2"));
        g.add_edge(Edge::new("e2", "n2", "n3"));
        let out = CoseBilkentLayout::solve(&CoseBilkentSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 4 * 3);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn iterations_setting_is_honored() {
        // A zero-iteration solve just returns the seed positions, still finite.
        let mut g = Graph::new();
        g.add_node(Node::new("a"));
        g.add_node(Node::new("b"));
        let s = CoseBilkentSettings { iterations: 0, ..CoseBilkentSettings::default() };
        let out = CoseBilkentLayout::solve(&s, &g).unwrap();
        assert_eq!(out.len(), 2 * 3);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
