//! Dagre hierarchical (layered DAG) static layout.
//!
//! One-shot CPU solver on the System-B [`StaticLayout`] trait (replacing the
//! legacy `DagreLayoutEngine`). Phases, preserved from the original: rank nodes
//! into layers (longest-path, or network-simplex / tight-tree refinements),
//! optionally break backward edges, reduce crossings by adjacent swaps, then
//! assign coordinates honoring the rank direction. Computes into local
//! structures; the graph is never mutated.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

/// Rank (layer) growth direction.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RankDirection {
    #[default]
    TB,
    BT,
    LR,
    RL,
}

/// Ranking strategy.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DagreRanker {
    #[default]
    NetworkSimplex,
    TightTree,
    LongestPath,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DagreSettings {
    pub rank_direction: RankDirection,
    pub ranker: DagreRanker,
    pub rank_separation: f64,
    pub node_separation: f64,
    pub acyclic: bool,
}

impl Default for DagreSettings {
    fn default() -> Self {
        Self {
            rank_direction: RankDirection::TB,
            ranker: DagreRanker::NetworkSimplex,
            rank_separation: 50.0,
            node_separation: 50.0,
            acyclic: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DagreLayout;

/// Longest-path layering: roots (no incoming edge) on layer 0, then BFS along
/// edges. Node ids are processed in sorted order for determinism.
fn longest_path_ranking(ids: &[&String], edges: &[(String, String)]) -> Vec<Vec<String>> {
    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut assigned: HashSet<String> = HashSet::new();

    let mut roots: Vec<String> = ids
        .iter()
        .filter(|id| !edges.iter().any(|(_, t)| t == **id))
        .map(|id| (*id).clone())
        .collect();
    if roots.is_empty() && !ids.is_empty() {
        roots.push(ids[0].clone());
    }

    for r in &roots {
        assigned.insert(r.clone());
    }
    layers.push(roots);

    let mut current = 0;
    while current < layers.len() {
        let mut next: Vec<String> = Vec::new();
        for node in &layers[current] {
            for (s, t) in edges {
                if s == node && !assigned.contains(t) {
                    next.push(t.clone());
                    assigned.insert(t.clone());
                }
            }
        }
        if !next.is_empty() {
            layers.push(next);
        }
        current += 1;
    }

    let remaining: Vec<String> = ids
        .iter()
        .filter(|id| !assigned.contains(**id))
        .map(|id| (*id).clone())
        .collect();
    if !remaining.is_empty() {
        layers.push(remaining);
    }

    layers
}

/// Move nodes to adjacent layers when doing so shortens incident edges, keeping
/// the layering a valid DAG ordering. (Simplified network-simplex refinement.)
fn optimize_ranking(layers: &mut [Vec<String>], edges: &[(String, String)]) {
    let mut node_to_layer: HashMap<String, usize> = HashMap::new();
    for (idx, layer) in layers.iter().enumerate() {
        for node in layer {
            node_to_layer.insert(node.clone(), idx);
        }
    }

    let mut improved = true;
    while improved {
        improved = false;
        for layer_idx in 0..layers.len() {
            let mut i = 0;
            while i < layers[layer_idx].len() {
                let node_id = layers[layer_idx][i].clone();

                let edge_len_sum = |at: usize, n2l: &HashMap<String, usize>| -> usize {
                    let mut sum = 0usize;
                    for (s, t) in edges {
                        if *s == node_id || *t == node_id {
                            let other = if *s == node_id { t } else { s };
                            if let Some(ol) = n2l.get(other) {
                                sum = sum.saturating_add(at.abs_diff(*ol));
                            }
                        }
                    }
                    sum
                };

                let current_sum = edge_len_sum(layer_idx, &node_to_layer);

                let mut moved = false;
                for new_idx in [layer_idx.saturating_sub(1), layer_idx + 1] {
                    if new_idx >= layers.len() || new_idx == layer_idx {
                        continue;
                    }

                    // Preserve DAG validity for every edge touching this node.
                    let valid = edges.iter().all(|(s, t)| {
                        if *s != node_id && *t != node_id {
                            return true;
                        }
                        let sl = if *s == node_id {
                            new_idx
                        } else {
                            *node_to_layer.get(s).unwrap_or(&new_idx)
                        };
                        let tl = if *t == node_id {
                            new_idx
                        } else {
                            *node_to_layer.get(t).unwrap_or(&new_idx)
                        };
                        sl < tl
                    });
                    if !valid {
                        continue;
                    }

                    if edge_len_sum(new_idx, &node_to_layer) < current_sum {
                        let node = layers[layer_idx].remove(i);
                        layers[new_idx].push(node.clone());
                        node_to_layer.insert(node, new_idx);
                        improved = true;
                        moved = true;
                        i = i.saturating_sub(1);
                        break;
                    }
                }
                if !moved {
                    i += 1;
                }
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

impl StaticLayout for DagreLayout {
    type Settings = DagreSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "dagre",
            kind: LayoutKind::Static,
            display_name: "Hierarchical (Dagre)",
            description: "Directed-graph layout optimized for hierarchical visualizations.",
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

        let mut edges: Vec<(String, String)> = graph
            .edges
            .values()
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();

        // Phase 1: rank into layers.
        let mut layers = match settings.ranker {
            DagreRanker::LongestPath => longest_path_ranking(&ids, &edges),
            DagreRanker::NetworkSimplex => {
                let mut l = longest_path_ranking(&ids, &edges);
                optimize_ranking(&mut l, &edges);
                l
            }
            DagreRanker::TightTree => {
                let mut l = longest_path_ranking(&ids, &edges);
                l.retain(|layer| !layer.is_empty());
                l
            }
        };

        // Phase 2: break cycles (after ranking, matching the original order).
        if settings.acyclic {
            break_cycles(&mut edges, &layers);
        }

        // Phase 3: reduce crossings.
        minimize_crossings(&mut layers, &edges);

        // Phase 4: coordinate assignment.
        let is_horizontal =
            matches!(settings.rank_direction, RankDirection::LR | RankDirection::RL);
        let is_reversed =
            matches!(settings.rank_direction, RankDirection::BT | RankDirection::RL);
        let rank_sep = settings.rank_separation;
        let node_sep = settings.node_separation;

        let mut xy: HashMap<&str, (f64, f64)> = HashMap::with_capacity(n);
        let num_layers = layers.len();
        for (layer_idx, layer) in layers.iter().enumerate() {
            let layer_pos = if is_reversed {
                (num_layers - 1 - layer_idx) as f64 * rank_sep
            } else {
                layer_idx as f64 * rank_sep
            };
            let layer_width = (layer.len().saturating_sub(1)) as f64 * node_sep;
            let start = -layer_width / 2.0;
            for (node_idx, node_id) in layer.iter().enumerate() {
                let node_pos = start + node_idx as f64 * node_sep;
                let coord = if is_horizontal {
                    (layer_pos, node_pos)
                } else {
                    (node_pos, layer_pos)
                };
                xy.insert(node_id.as_str(), coord);
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

    fn chain() -> Graph {
        let mut g = Graph::new();
        g.add_node(Node::new("A"));
        g.add_node(Node::new("B"));
        g.add_node(Node::new("C"));
        g.add_edge(Edge::new("e1", "A", "B"));
        g.add_edge(Edge::new("e2", "B", "C"));
        g
    }

    #[test]
    fn tb_chain_increases_y() {
        let g = chain();
        let out = DagreLayout::solve(&DagreSettings::default(), &g).unwrap();
        let a = pos(&g, &out, "A");
        let b = pos(&g, &out, "B");
        let c = pos(&g, &out, "C");
        assert!(a.1 < b.1 && b.1 < c.1, "TB ranks should increase in y");
    }

    #[test]
    fn lr_chain_increases_x() {
        let g = chain();
        let s = DagreSettings { rank_direction: RankDirection::LR, ..DagreSettings::default() };
        let out = DagreLayout::solve(&s, &g).unwrap();
        let a = pos(&g, &out, "A");
        let b = pos(&g, &out, "B");
        let c = pos(&g, &out, "C");
        assert!(a.0 < b.0 && b.0 < c.0, "LR ranks should increase in x");
    }

    #[test]
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let out = DagreLayout::solve(&DagreSettings::default(), &g).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn all_nodes_get_finite_positions_with_isolated_node() {
        let mut g = chain();
        g.add_node(Node::new("Z")); // isolated
        let out = DagreLayout::solve(&DagreSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 4 * 3);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
