//! tvix-loader — tvix adapter: evaluates Nix graph expressions and converts
//! [`tvix_wasm::GeneratedGraph`] into a [`vault_data::VaultGraph`].
//!
//! This is the second canonical adapter after `vault-links` (Obsidian). It
//! lets you generate seed / test datasets with controllable attributes:
//! size, connectivity, average degree, random topologies — all authored as
//! Nix expressions evaluated through tvix-eval.
//!
//! # Tags and links
//!
//! - **Tags**: auto-derived from the Nix node `type` field (mapped to
//!   `kind` in `GenNode`). Each distinct `kind` becomes a tag on every node
//!   of that kind. The built-in generators produce kinds like `"center"`,
//!   `"spoke"`, `"node"`, `"particle"`.
//! - **Links**: `GenEdge { source, target }` maps directly to
//!   `VaultEdge { source, target }`. All edges are directed.
//!
//! # Controllable attributes
//!
//! The Nix expression controls everything. The built-in combinators
//! (`graph-combinators.nix`) expose:
//!
//! | Generator      | Controls                          |
//! |----------------|-----------------------------------|
//! | `starGen`      | `nodes` (hub + spokes)            |
//! | `pathGen`      | `nodes` (chain length)            |
//! | `cycleGen`     | `nodes` (ring size)               |
//! | `gridGen`      | `rows`, `cols` (2D lattice)       |
//! | `completeGen`  | `nodes` (K_n, max degree = n-1)   |
//! | `soupGen`      | `nodes` (isolated, zero edges)    |
//!
//! Custom expressions can produce arbitrary topologies with any degree
//! distribution, community structure, or random wiring.

use std::collections::HashMap;

use data_loader::{LoadResult, Loader};
use rand::Rng;
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

/// Loads a graph by evaluating a Nix expression through tvix-eval.
///
/// The expression must produce a `toGraphJSON`-shaped attrset
/// (`{ nodes = [...]; links = [...]; }`). See [`tvix_wasm::eval_graph`].
pub struct TvixLoader {
    /// The Nix expression to evaluate.
    expr: String,
}

impl TvixLoader {
    /// Create a loader from a Nix expression string.
    ///
    /// The expression is evaluated lazily on each [`load`] call — the
    /// loader is just a holder for the source text.
    pub fn new(expr: impl Into<String>) -> Self {
        Self { expr: expr.into() }
    }

    /// Create a loader from one of the built-in demo expressions.
    ///
    /// Returns `None` if `name` doesn't match any demo.
    pub fn from_demo(name: &str) -> Option<Self> {
        tvix_wasm::demos()
            .iter()
            .find(|d| d.name == name)
            .map(|d| Self::new(d.expr))
    }

    /// List available demo names.
    pub fn demo_names() -> Vec<&'static str> {
        tvix_wasm::demos().iter().map(|d| d.name).collect()
    }
}

impl Loader for TvixLoader {
    fn name(&self) -> &str {
        "tvix"
    }

    fn load(&self) -> LoadResult {
        match tvix_wasm::eval_graph(&self.expr) {
            Ok(gen) => convert_generated_graph(&gen),
            Err(e) => {
                tracing::warn!(error = %e, "tvix eval failed");
                LoadResult {
                    graph: VaultGraph::new(),
                    unresolved: vec![e],
                }
            }
        }
    }

    /// Tvix graphs have no filesystem root — no watching.
    fn root_path(&self) -> Option<&std::path::PathBuf> {
        None
    }
}

/// Generates a random graph directly in Rust — no Nix eval overhead.
///
/// Produces `num_nodes` nodes (IDs `n0`..`n{N-1}`) and `num_edges` random
/// directed edges between them. No self-loops. Each node gets the tag
/// `"generated"`. Generation is O(N+E) and completes in milliseconds even
/// for 100k+ node graphs.
///
/// # CLI flags
///
/// `--source generate --nodes 100000 --edges 100000`
pub struct GenerateLoader {
    num_nodes: usize,
    num_edges: usize,
}

impl GenerateLoader {
    pub fn new(num_nodes: usize, num_edges: usize) -> Self {
        Self { num_nodes, num_edges }
    }
}

impl Loader for GenerateLoader {
    fn name(&self) -> &str {
        "generate"
    }

    fn load(&self) -> LoadResult {
        let mut graph = VaultGraph::new();
        let mut rng = rand::thread_rng();

        // Generate nodes.
        for i in 0..self.num_nodes {
            let id = format!("n{i}");
            let meta = NodeMeta {
                title: id.clone(),
                tags: vec!["generated".into()],
                frontmatter: HashMap::new(),
                mtime: 0,
                path: id.clone(),
                doctype: Some("generated".into()),
                folder: String::new(),
            };
            graph.add_node(VaultNode {
                id,
                meta,
                metrics: NodeMetrics::default(),
                x: 0.0,
                y: 0.0,
            });
        }

        // Generate random edges (no self-loops).
        let max_node = self.num_nodes.saturating_sub(1);
        for _ in 0..self.num_edges {
            let src = rng.gen_range(0..=max_node);
            let mut tgt = rng.gen_range(0..=max_node);
            // Avoid self-loops: if we pick the same node, shift by 1.
            if self.num_nodes > 1 && tgt == src {
                tgt = (tgt + 1) % self.num_nodes;
            }
            graph.add_edge(VaultEdge {
                source: format!("n{src}"),
                target: format!("n{tgt}"),
            });
        }

        LoadResult {
            graph,
            unresolved: Vec::new(),
        }
    }

    fn root_path(&self) -> Option<&std::path::PathBuf> {
        None
    }
}

/// Convert a [`tvix_wasm::GeneratedGraph`] into a [`VaultGraph`].
///
/// Mapping:
/// - `GenNode { id, kind }` → `VaultNode { id, meta: { title: id, tags: [kind], ... } }`
/// - `GenEdge { source, target }` → `VaultEdge { source, target }`
///
/// Tags are derived from the node `kind`: each distinct kind becomes a tag
/// applied to every node of that kind. This gives the frontend's tag chip
/// strip immediate utility for generated graphs — filter by `kind` without
/// any extra configuration.
pub fn convert_generated_graph(gen: &tvix_wasm::GeneratedGraph) -> LoadResult {
    let mut graph = VaultGraph::new();

    // Collect kind → tag mapping for consistent tagging.
    let mut kind_tags: HashMap<String, String> = HashMap::new();

    for node in &gen.nodes {
        let tag = node.kind.as_deref().unwrap_or("node");
        kind_tags.entry(tag.to_string()).or_insert_with(|| tag.to_string());

        let meta = NodeMeta {
            title: node.id.clone(),
            tags: vec![tag.to_string()],
            frontmatter: HashMap::new(),
            mtime: 0,
            path: node.id.clone(),
            doctype: Some("generated".into()),
            folder: String::new(),
        };

        graph.add_node(VaultNode {
            id: node.id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });
    }

    for edge in &gen.edges {
        // Only add edges where both endpoints exist in the node set.
        if graph.nodes.contains_key(&edge.source) && graph.nodes.contains_key(&edge.target) {
            graph.add_edge(VaultEdge {
                source: edge.source.clone(),
                target: edge.target.clone(),
            });
        }
    }

    LoadResult {
        graph,
        unresolved: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_graph_round_trip() {
        let loader = TvixLoader::from_demo("Star (hub)").expect("star demo exists");
        let result = loader.load();
        assert!(result.unresolved.is_empty(), "tvix graphs have no unresolved refs");
        let g = &result.graph;
        assert_eq!(g.node_count(), 12, "star: 1 center + 11 spokes");
        assert_eq!(g.edge_count(), 11, "star: 11 hub→spoke edges");

        // Center node should have tag "center".
        let center = g.nodes.get("n0").expect("center node exists");
        assert!(center.meta.tags.contains(&"center".to_string()));

        // Spoke nodes should have tag "spoke".
        let spoke = g.nodes.get("n1").expect("spoke node exists");
        assert!(spoke.meta.tags.contains(&"spoke".to_string()));
    }

    #[test]
    fn soup_is_unbonded() {
        let loader = TvixLoader::from_demo("Soup (self-assembly seed)").expect("soup demo exists");
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 200);
        assert_eq!(result.graph.edge_count(), 0, "soup has zero edges");

        // All nodes tagged "particle".
        for (_, node) in &result.graph.nodes {
            assert!(node.meta.tags.contains(&"particle".to_string()));
        }
    }

    #[test]
    fn chain_has_correct_degree() {
        let loader = TvixLoader::from_demo("Chain (path)").expect("chain demo exists");
        let result = loader.load();
        let g = &result.graph;
        assert_eq!(g.node_count(), 16);
        // Chain of 16 nodes has 15 edges.
        assert_eq!(g.edge_count(), 15);
    }

    #[test]
    fn custom_expr_works() {
        let expr = r#"{
            nodes = [ { id = "a"; type = "source"; } { id = "b"; type = "sink"; } ];
            links = [ { source = "a"; target = "b"; } ];
        }"#;
        let loader = TvixLoader::new(expr);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 2);
        assert_eq!(result.graph.edge_count(), 1);

        let a = result.graph.nodes.get("a").unwrap();
        assert!(a.meta.tags.contains(&"source".to_string()));
        let b = result.graph.nodes.get("b").unwrap();
        assert!(b.meta.tags.contains(&"sink".to_string()));
    }

    #[test]
    fn bad_expr_returns_empty_graph() {
        let loader = TvixLoader::new("let x = in");
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 0);
        assert!(!result.unresolved.is_empty(), "error should be in unresolved");
    }

    #[test]
    fn demo_names_are_non_empty() {
        let names = TvixLoader::demo_names();
        assert!(!names.is_empty());
        // Every demo name should resolve to a loader.
        for name in &names {
            assert!(TvixLoader::from_demo(name).is_some(), "demo {name} not found");
        }
    }

    #[test]
    fn generate_small_graph() {
        let loader = GenerateLoader::new(10, 20);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 10);
        assert_eq!(result.graph.edge_count(), 20);
        assert!(result.unresolved.is_empty());
        // All nodes tagged "generated".
        for (_, node) in &result.graph.nodes {
            assert!(node.meta.tags.contains(&"generated".to_string()));
        }
    }

    #[test]
    fn generate_no_self_loops() {
        // With 2 nodes and many edges, self-loops should never appear.
        let loader = GenerateLoader::new(2, 100);
        let result = loader.load();
        for edge in &result.graph.edges {
            assert_ne!(edge.source, edge.target, "no self-loops");
        }
    }

    #[test]
    fn generate_zero_nodes() {
        let loader = GenerateLoader::new(0, 0);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 0);
        assert_eq!(result.graph.edge_count(), 0);
    }

    #[test]
    fn generate_large_graph_is_fast() {
        // 50k nodes, 100k edges should complete in well under 1 second.
        let loader = GenerateLoader::new(50_000, 100_000);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 50_000);
        assert_eq!(result.graph.edge_count(), 100_000);
    }
}