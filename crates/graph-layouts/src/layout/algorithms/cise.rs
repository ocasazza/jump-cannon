//! CiSE (Circular Spring Embedder) static layout.
//!
//! One-shot CPU solver on the System-B [`StaticLayout`] trait (replacing the
//! legacy `CiseLayoutEngine`). Nodes are grouped into clusters and each cluster
//! is placed on its own circle. Clusters may be supplied explicitly via
//! [`CiseSettings::clusters`]; when none are given they are derived from the
//! graph's connected components (union-find over edges), so the layout is
//! meaningful with no manual configuration. Cluster centers ride an outer ring;
//! nodes ride an inner circle of radius 100 around their cluster center.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CiseSettings {
    /// Explicit clusters (each a list of node ids). Empty ⇒ clusters are
    /// derived from connected components. Kept for API/wire compatibility; the
    /// interactive UI does not edit it.
    #[serde(default)]
    pub clusters: Vec<Vec<String>>,
    /// Gap between adjacent cluster circles.
    pub circle_spacing: f64,
}

impl Default for CiseSettings {
    fn default() -> Self {
        Self {
            clusters: Vec::new(),
            circle_spacing: 20.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CiseLayout;

/// Union-find root with path compression.
fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

impl StaticLayout for CiseLayout {
    type Settings = CiseSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "cise",
            kind: LayoutKind::Static,
            display_name: "Circular (CiSE)",
            description:
                "Circular Spring Embedder — groups nodes into clusters, each on its own circle.",
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

        // Build clusters as lists of node indices (id-sorted within each).
        let clusters: Vec<Vec<usize>> = if !settings.clusters.is_empty() {
            let mut out: Vec<Vec<usize>> = Vec::new();
            let mut placed = vec![false; n];
            for cluster in &settings.clusters {
                let mut members: Vec<usize> = cluster
                    .iter()
                    .filter_map(|id| index.get(id.as_str()).copied())
                    .collect();
                members.sort_unstable();
                for &m in &members {
                    placed[m] = true;
                }
                if !members.is_empty() {
                    out.push(members);
                }
            }
            // Any node not named in an explicit cluster becomes its own ring entry,
            // collected into one leftover cluster.
            let leftover: Vec<usize> = (0..n).filter(|&i| !placed[i]).collect();
            if !leftover.is_empty() {
                out.push(leftover);
            }
            out
        } else {
            // Derive clusters from connected components.
            let mut parent: Vec<usize> = (0..n).collect();
            for e in graph.edges.values() {
                if let (Some(&s), Some(&t)) =
                    (index.get(e.source.as_str()), index.get(e.target.as_str()))
                {
                    let rs = find(&mut parent, s);
                    let rt = find(&mut parent, t);
                    if rs != rt {
                        parent[rs] = rt;
                    }
                }
            }
            let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
            for i in 0..n {
                let r = find(&mut parent, i);
                by_root.entry(r).or_default().push(i);
            }
            // Deterministic cluster order: by each cluster's smallest member.
            let mut groups: Vec<Vec<usize>> = by_root.into_values().collect();
            for g in &mut groups {
                g.sort_unstable();
            }
            groups.sort_by_key(|g| g[0]);
            groups
        };

        let cluster_radius = 100.0_f64;
        let mut pos = vec![(0.0f64, 0.0f64); n];
        let tau = 2.0 * std::f64::consts::PI;

        if clusters.len() <= 1 {
            // Single cluster (the common connected-graph case): one circle at the
            // origin, matching the legacy single-circle arrangement.
            let members = clusters.into_iter().next().unwrap_or_default();
            let count = members.len().max(1) as f64;
            for (k, idx) in members.iter().enumerate() {
                let angle = tau * k as f64 / count;
                pos[*idx] = (cluster_radius * angle.cos(), cluster_radius * angle.sin());
            }
        } else {
            let circle_spacing = settings.circle_spacing;
            let outer_radius = cluster_radius * 2.0 + circle_spacing;
            let cluster_count = clusters.len() as f64;
            for (cluster_idx, members) in clusters.iter().enumerate() {
                let center_angle = tau * cluster_idx as f64 / cluster_count;
                let cx = outer_radius * center_angle.cos();
                let cy = outer_radius * center_angle.sin();
                let count = members.len().max(1) as f64;
                for (k, idx) in members.iter().enumerate() {
                    let inner = tau * k as f64 / count;
                    pos[*idx] = (cx + cluster_radius * inner.cos(), cy + cluster_radius * inner.sin());
                }
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

    fn radius(out: &[f32], i: usize) -> f32 {
        (out[i * 3] * out[i * 3] + out[i * 3 + 1] * out[i * 3 + 1]).sqrt()
    }

    #[test]
    fn empty_graph_yields_empty_output() {
        let g = Graph::new();
        let out = CiseLayout::solve(&CiseSettings::default(), &g).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn single_component_sits_on_one_circle() {
        let mut g = Graph::new();
        for i in 0..4 {
            g.add_node(Node::new(format!("n{i}")));
        }
        for i in 0..4 {
            g.add_edge(Edge::new(format!("e{i}"), format!("n{i}"), format!("n{}", (i + 1) % 4)));
        }
        let out = CiseLayout::solve(&CiseSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 4 * 3);
        for i in 0..4 {
            assert!((radius(&out, i) - 100.0).abs() < 1e-3, "node {i} off the unit circle");
        }
    }

    #[test]
    fn two_components_land_on_separate_circles() {
        let mut g = Graph::new();
        // Triangle A
        for c in ["a0", "a1", "a2"] {
            g.add_node(Node::new(c));
        }
        g.add_edge(Edge::new("ea0", "a0", "a1"));
        g.add_edge(Edge::new("ea1", "a1", "a2"));
        g.add_edge(Edge::new("ea2", "a2", "a0"));
        // Triangle B (disjoint)
        for c in ["b0", "b1", "b2"] {
            g.add_node(Node::new(c));
        }
        g.add_edge(Edge::new("eb0", "b0", "b1"));
        g.add_edge(Edge::new("eb1", "b1", "b2"));
        g.add_edge(Edge::new("eb2", "b2", "b0"));

        let out = CiseLayout::solve(&CiseSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 6 * 3);

        let mut ids: Vec<&String> = g.nodes.keys().collect();
        ids.sort();
        let centroid = |prefix: &str| -> (f32, f32) {
            let mut sx = 0.0;
            let mut sy = 0.0;
            let mut c = 0.0;
            for (i, id) in ids.iter().enumerate() {
                if id.starts_with(prefix) {
                    sx += out[i * 3];
                    sy += out[i * 3 + 1];
                    c += 1.0;
                }
            }
            (sx / c, sy / c)
        };
        let (ax, ay) = centroid("a");
        let (bx, by) = centroid("b");
        let sep = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt();
        assert!(sep > 100.0, "clusters not separated (sep = {sep})");
    }
}
