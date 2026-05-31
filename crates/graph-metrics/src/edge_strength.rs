//! Per-edge **structural strength** — the "is this edge intra-cluster or a global
//! shortcut?" signal that keeps small-world graphs from collapsing into a
//! hairball under a force layout.
//!
//! Both metrics are built on `T(u,v)` = the number of **common neighbours** of an
//! edge's endpoints (equivalently, the number of triangles the edge sits in). A
//! high score means the edge is *embedded* in a dense neighbourhood (local, keeps
//! a cluster tight); a low score means a *global shortcut* (responsible for the
//! small diameter, and the thing that drags distinct communities on top of each
//! other). Feeding the score into a layout as a per-edge spring rest length
//! (strong → short, weak → long) lets clusters separate.
//!
//! See `docs/small-world-layout-research.md` for the cited derivation. Cost is
//! `O(Σ_e min(deg u, deg v))` ≈ `O(m·a(G))` with arboricity `a(G) ≤ √m` — cheap,
//! and the per-edge intersection is embarrassingly parallel.

use std::collections::{HashMap, HashSet};

use vault_data::VaultGraph;

/// Which structural edge-strength metric to compute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeStrengthKind {
    /// Jaccard / topological overlap of the endpoint neighbourhoods:
    /// `T / ((deg u − 1) + (deg v − 1) − T)`. Cheapest; tends to over-emphasise
    /// edges inside tiny dense subgraphs (Batagelj). Already in `[0, 1]`.
    Jaccard,
    /// Batagelj's corrected overlap: `T / (μ + M(e) − T)` with `μ = max_e T(e)`
    /// and `M(e) = max(deg u, deg v) − 1`. Normalised to `[0, 1]`; damps the
    /// small-dense-subgraph over-emphasis of plain Jaccard.
    CorrectedOverlap,
}

/// Per-edge structural strength, **parallel to `graph.edges`** (one value per
/// edge entry, in the graph's edge order — so it lines up with any downstream
/// adjacency walk that iterates `graph.edges` the same way).
///
/// Each value is in `[0, 1]`: `≈1` = embedded / local (high neighbourhood
/// overlap), `≈0` = global shortcut (low overlap). Edges with an unknown endpoint
/// or self-loops score `0.0`.
pub struct EdgeStrength {
    pub kind: EdgeStrengthKind,
    pub strength: Vec<f32>,
}

impl EdgeStrength {
    /// Map each strength `s ∈ [0, 1]` to a spring **rest length** for a layout
    /// engine: strong edges target `base`, weak shortcuts stretch up to
    /// `base · (1 + spread)`.
    ///
    /// `target_len(e) = base · (1 + spread · (1 − s))`.
    ///
    /// With `spread = 0` this is uniform `base` (no effect); a typical `spread`
    /// of 2–4 makes pure shortcuts want to be 3×–5× longer than fully-embedded
    /// edges, which is what pushes communities apart along the shortcuts.
    pub fn to_rest_lengths(&self, base: f32, spread: f32) -> Vec<f32> {
        self.strength
            .iter()
            .map(|&s| base * (1.0 + spread * (1.0 - s)))
            .collect()
    }
}

/// Compute the per-edge structural strength for `graph` under `kind`.
///
/// The graph is treated as **undirected** (matching `compute_louvain` /
/// `compute_kcore`): parallel edges and direction are collapsed when building
/// neighbour sets, but the returned vector still has one entry per original
/// `graph.edges` entry (parallel edges receive the same score).
pub fn compute_edge_strength(graph: &VaultGraph, kind: EdgeStrengthKind) -> EdgeStrength {
    let n = graph.nodes.len();
    let m = graph.edges.len();

    // id → dense index, in the graph's node order.
    let idx: HashMap<&str, usize> = graph
        .nodes
        .keys()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    // Undirected neighbour sets (dedup parallel edges + self-loops).
    let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for edge in &graph.edges {
        if let (Some(&s), Some(&t)) = (idx.get(edge.source.as_str()), idx.get(edge.target.as_str()))
        {
            if s != t {
                adj[s].insert(t);
                adj[t].insert(s);
            }
        }
    }

    // First pass: common-neighbour count T(e) per edge, parallel to graph.edges.
    // `None` marks an invalid edge (unknown endpoint or self-loop) → strength 0.
    let mut t_counts: Vec<Option<u32>> = Vec::with_capacity(m);
    let mut max_t: u32 = 0;
    for edge in &graph.edges {
        let entry = match (idx.get(edge.source.as_str()), idx.get(edge.target.as_str())) {
            (Some(&s), Some(&t)) if s != t => {
                let t_count = common_neighbours(&adj[s], &adj[t]);
                max_t = max_t.max(t_count);
                Some(t_count)
            }
            _ => None,
        };
        t_counts.push(entry);
    }

    // Second pass: derive the chosen metric. CorrectedOverlap needs μ = max_t,
    // which is why T is materialised first.
    let mu = max_t as f32;
    let mut strength = Vec::with_capacity(m);
    for (edge, t_opt) in graph.edges.iter().zip(&t_counts) {
        let s = match t_opt {
            None => 0.0,
            Some(t_count) => {
                let (&su, &tv) = (
                    idx.get(edge.source.as_str()).unwrap(),
                    idx.get(edge.target.as_str()).unwrap(),
                );
                let du = adj[su].len() as f32;
                let dv = adj[tv].len() as f32;
                let t = *t_count as f32;
                match kind {
                    EdgeStrengthKind::Jaccard => {
                        // Union of N(u)\{v} and N(v)\{u}: (du-1)+(dv-1)-T.
                        let denom = (du - 1.0) + (dv - 1.0) - t;
                        if denom > 0.0 {
                            (t / denom).clamp(0.0, 1.0)
                        } else {
                            // Both endpoints have only each other → not embedded.
                            0.0
                        }
                    }
                    EdgeStrengthKind::CorrectedOverlap => {
                        let m_e = du.max(dv) - 1.0;
                        let denom = mu + m_e - t;
                        if denom > 0.0 {
                            (t / denom).clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    }
                }
            }
        };
        strength.push(s);
    }

    EdgeStrength { kind, strength }
}

/// `|A ∩ B|`, iterating the smaller set against the larger for `O(min(|A|,|B|))`.
fn common_neighbours(a: &HashSet<usize>, b: &HashSet<usize>) -> u32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small.iter().filter(|w| large.contains(*w)).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_data::{VaultEdge, VaultNode};

    fn node(id: &str) -> VaultNode {
        VaultNode {
            id: id.to_string(),
            ..Default::default()
        }
    }

    /// Two triangles {a,b,c} and {d,e,f} joined by a single shortcut edge c–d.
    /// The triangle edges are embedded (share a common neighbour) and should
    /// score high; the shortcut shares no common neighbour and should score 0.
    fn two_triangles_bridge() -> (VaultGraph, usize, usize) {
        let mut g = VaultGraph::default();
        for id in ["a", "b", "c", "d", "e", "f"] {
            g.add_node(node(id));
        }
        let edges = [
            ("a", "b"),
            ("b", "c"),
            ("c", "a"), // triangle 1
            ("d", "e"),
            ("e", "f"),
            ("f", "d"), // triangle 2
            ("c", "d"), // global shortcut
        ];
        for (s, t) in edges {
            g.add_edge(VaultEdge {
                source: s.to_string(),
                target: t.to_string(),
            });
        }
        // indices of the triangle edge a-b (0) and the shortcut c-d (6)
        (g, 0, 6)
    }

    #[test]
    fn shortcut_scores_below_triangle_edges_jaccard() {
        let (g, tri_idx, shortcut_idx) = two_triangles_bridge();
        let es = compute_edge_strength(&g, EdgeStrengthKind::Jaccard);
        assert_eq!(es.strength.len(), g.edges.len());

        // a-b: common neighbour c. deg(a)=deg(b)=2. J = 1/((2-1)+(2-1)-1) = 1/1 = 1.
        assert!(
            es.strength[tri_idx] > 0.9,
            "triangle edge should be near-fully embedded, got {}",
            es.strength[tri_idx]
        );
        // c-d: no common neighbour → 0.
        assert_eq!(
            es.strength[shortcut_idx], 0.0,
            "shortcut edge should score 0"
        );
        assert!(es.strength[tri_idx] > es.strength[shortcut_idx]);
    }

    #[test]
    fn corrected_overlap_in_unit_range_and_orders_the_same() {
        let (g, tri_idx, shortcut_idx) = two_triangles_bridge();
        let es = compute_edge_strength(&g, EdgeStrengthKind::CorrectedOverlap);
        for &s in &es.strength {
            assert!((0.0..=1.0).contains(&s), "strength {s} out of [0,1]");
        }
        assert!(
            es.strength[tri_idx] > es.strength[shortcut_idx],
            "embedded edge must outrank shortcut"
        );
    }

    #[test]
    fn rest_length_mapping_stretches_shortcuts() {
        let (g, tri_idx, shortcut_idx) = two_triangles_bridge();
        let es = compute_edge_strength(&g, EdgeStrengthKind::Jaccard);
        let lens = es.to_rest_lengths(1.0, 3.0);
        // Embedded (s≈1) ≈ base; shortcut (s=0) = base*(1+spread) = 4.0.
        assert!((lens[tri_idx] - 1.0).abs() < 1e-3);
        assert!((lens[shortcut_idx] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn handles_unknown_endpoints_and_self_loops() {
        let mut g = VaultGraph::default();
        g.add_node(node("a"));
        g.add_node(node("b"));
        g.add_edge(VaultEdge {
            source: "a".into(),
            target: "ghost".into(),
        }); // unknown endpoint
        g.add_edge(VaultEdge {
            source: "a".into(),
            target: "a".into(),
        }); // self-loop
        g.add_edge(VaultEdge {
            source: "a".into(),
            target: "b".into(),
        });
        let es = compute_edge_strength(&g, EdgeStrengthKind::Jaccard);
        assert_eq!(es.strength.len(), 3);
        assert_eq!(es.strength[0], 0.0);
        assert_eq!(es.strength[1], 0.0);
        // a-b has no common neighbours either → 0, but must not panic.
        assert_eq!(es.strength[2], 0.0);
    }

    #[test]
    fn empty_graph_is_empty() {
        let g = VaultGraph::default();
        let es = compute_edge_strength(&g, EdgeStrengthKind::Jaccard);
        assert!(es.strength.is_empty());
    }
}
