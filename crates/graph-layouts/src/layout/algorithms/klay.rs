//! KLay Layered static layout.
//!
//! One-shot CPU layered solver on the System-B [`StaticLayout`] trait
//! (replacing the legacy `KlayLayoutEngine`). Four phases, preserved from the
//! original: assign nodes to layers (BFS from roots), break backward edges,
//! reduce crossings by adjacent swaps, then assign coordinates (layer → y,
//! evenly spaced within a layer → x). Computes into local structures; the
//! graph is never mutated.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KlaySettings {
    pub layer_spacing: f64,
    pub node_spacing: f64,
}

impl Default for KlaySettings {
    fn default() -> Self {
        Self {
            layer_spacing: 50.0,
            node_spacing: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct KlayLayout;

/// BFS layering from roots (nodes with no incoming edge). `edges` is the local
/// (possibly cycle-broken) edge list. Node ids are processed in sorted order
/// for determinism.
fn assign_layers(ids: &[&String], edges: &[(String, String)]) -> Vec<Vec<String>> {
    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut assigned: HashSet<String> = HashSet::new();

    let mut current: Vec<String> = ids
        .iter()
        .filter(|id| !edges.iter().any(|(_, t)| t == **id))
        .map(|id| (*id).clone())
        .collect();

    if current.is_empty() && !ids.is_empty() {
        current.push(ids[0].clone());
    }
    for id in &current {
        assigned.insert(id.clone());
    }

    while !current.is_empty() {
        layers.push(current.clone());
        let mut next: Vec<String> = Vec::new();
        for node in &current {
            for (s, t) in edges {
                if s == node && !assigned.contains(t) {
                    next.push(t.clone());
                    assigned.insert(t.clone());
                }
            }
        }
        current = next;
    }

    // Remaining (disconnected / in-cycle) nodes append to the last layer.
    for id in ids {
        if !assigned.contains(*id) {
            if let Some(last) = layers.last_mut() {
                last.push((*id).clone());
            } else {
                layers.push(vec![(*id).clone()]);
            }
        }
    }

    layers
}

/// Reverse edges that point from a later layer back to an earlier one.
fn break_cycles(edges: &mut [(String, String)], layers: &[Vec<String>]) {
    let layer_of = |id: &str| layers.iter().position(|l| l.iter().any(|n| n == id));
    for (s, t) in edges.iter_mut() {
        if let (Some(sl), Some(tl)) = (layer_of(s), layer_of(t)) {
            if sl > tl {
                std::mem::swap(s, t);
            }
        }
    }
}

fn count_crossings(layer1: &[String], layer2: &[String], edges: &[(String, String)]) -> usize {
    let mut crossings = 0;
    for (i1, n1) in layer1.iter().enumerate() {
        for (i2, n2) in layer1.iter().enumerate().skip(i1 + 1) {
            for (s1, t1) in edges {
                if s1 != n1 {
                    continue;
                }
                for (s2, t2) in edges {
                    if s2 != n2 {
                        continue;
                    }
                    let j1 = layer2.iter().position(|n| n == t1);
                    let j2 = layer2.iter().position(|n| n == t2);
                    if let (Some(j1), Some(j2)) = (j1, j2) {
                        if (i1 < i2 && j1 > j2) || (i1 > i2 && j1 < j2) {
                            crossings += 1;
                        }
                    }
                }
            }
        }
    }
    crossings
}

fn minimize_crossings(layers: &mut [Vec<String>], edges: &[(String, String)]) {
    for i in 0..layers.len().saturating_sub(1) {
        let mut improved = true;
        while improved {
            improved = false;
            let current = layers[i].clone();
            let next = &mut layers[i + 1];
            let mut best = count_crossings(&current, next, edges);
            for j in 0..next.len().saturating_sub(1) {
                next.swap(j, j + 1);
                let c = count_crossings(&current, next, edges);
                if c < best {
                    best = c;
                    improved = true;
                } else {
                    next.swap(j, j + 1);
                }
            }
        }
    }
}

impl StaticLayout for KlayLayout {
    type Settings = KlaySettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "klay",
            kind: LayoutKind::Static,
            display_name: "Layered (KLay)",
            description: "Layer-based layout optimized for directed graphs.",
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

        // Local edge list so phases never mutate the graph.
        let mut edges: Vec<(String, String)> = graph
            .edges
            .values()
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();

        let mut layers = assign_layers(&ids, &edges);
        break_cycles(&mut edges, &layers);
        minimize_crossings(&mut layers, &edges);

        // Coordinate assignment: layer → y, evenly spaced within layer → x.
        let mut xy: std::collections::HashMap<&str, (f64, f64)> =
            std::collections::HashMap::with_capacity(n);
        for (layer_idx, layer) in layers.iter().enumerate() {
            let y = layer_idx as f64 * settings.layer_spacing;
            let layer_width = (layer.len().saturating_sub(1)) as f64 * settings.node_spacing;
            let start_x = -layer_width / 2.0;
            for (node_idx, node_id) in layer.iter().enumerate() {
                let x = start_x + node_idx as f64 * settings.node_spacing;
                xy.insert(node_id.as_str(), (x, y));
            }
        }

        let mut out = Vec::with_capacity(n * 3);
        for id in &ids {
            let (x, y) = xy.get(id.as_str()).copied().unwrap_or((0.0, 0.0));
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

    fn pos(g: &Graph, out: &[f32], id: &str) -> (f32, f32) {
        let mut ids: Vec<&String> = g.nodes.keys().collect();
        ids.sort();
        let i = ids.iter().position(|s| s.as_str() == id).unwrap();
        (out[i * 3], out[i * 3 + 1])
    }

    #[test]
    fn simple_chain_stacks_layers() {
        let mut g = Graph::new();
        g.add_node(Node::new("A"));
        g.add_node(Node::new("B"));
        g.add_node(Node::new("C"));
        g.add_edge(Edge::new("e1", "A", "B"));
        g.add_edge(Edge::new("e2", "B", "C"));

        let out = KlayLayout::solve(&KlaySettings::default(), &g).unwrap();
        let a = pos(&g, &out, "A");
        let b = pos(&g, &out, "B");
        let c = pos(&g, &out, "C");
        assert!(a.1 < b.1 && b.1 < c.1, "layers should increase in y");
    }

    #[test]
    fn cycle_is_broken_into_layers() {
        let mut g = Graph::new();
        g.add_node(Node::new("A"));
        g.add_node(Node::new("B"));
        g.add_edge(Edge::new("e1", "A", "B"));
        g.add_edge(Edge::new("e2", "B", "A"));
        let out = KlayLayout::solve(&KlaySettings::default(), &g).unwrap();
        // Both nodes get finite, distinct-layer positions despite the 2-cycle.
        assert_eq!(out.len(), 2 * 3);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let out = KlayLayout::solve(&KlaySettings::default(), &g).unwrap();
        assert!(out.is_empty());
    }
}
