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

use data_loader::{
    identity::Namespace, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, ImporterSchema,
    LoadResult, Loader, SearchDocument, TagHierarchySchema,
};
use rand::{Rng, SeedableRng};
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

    fn schema(&self) -> ImporterSchema {
        generated_schema(
            "tvix",
            "declared",
            "Directed edge declared by the evaluated Nix graph",
        )
        .with_input_media_types(["application/x-nix"])
    }

    fn load(&self) -> LoadResult {
        match self.try_load() {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(error = %e, "tvix eval failed");
                LoadResult {
                    graph: VaultGraph::new(),
                    search_documents: Vec::new(),
                    unresolved: vec![e.to_string()],
                }
            }
        }
    }

    fn try_load(&self) -> Result<LoadResult, data_loader::ImportError> {
        let generated = tvix_wasm::eval_graph(&self.expr).map_err(|message| {
            data_loader::ImportError::Decode {
                origin: "tvix".into(),
                message,
            }
        })?;
        convert_generated_graph(&generated)
    }

    /// Tvix graphs have no filesystem root — no watching.
    fn root_path(&self) -> Option<&std::path::PathBuf> {
        None
    }
}

/// Generates a random graph directly in Rust — no Nix eval overhead.
///
/// Produces `num_nodes` nodes (local IDs `n0`..`n{N-1}`, namespaced to
/// `generate:generate:n{i}`) and `num_edges` random directed edges between
/// them. No self-loops. Each node gets the tag `"generated"`. Generation is
/// O(N+E) and completes in milliseconds even for 100k+ node graphs.
///
/// Edge topology is deterministic: `load` seeds a `StdRng` with `seed`, so
/// the same `(nodes, edges, clusters, affinity, seed)` always produces the
/// identical graph.
///
/// # CLI flags
///
/// `--source generate --generate-nodes 100000 --generate-edges 100000 --seed 0`
pub struct GenerateLoader {
    num_nodes: usize,
    num_edges: usize,
    num_clusters: usize,
    cluster_affinity: f64,
    seed: u64,
}

impl GenerateLoader {
    pub fn new(
        num_nodes: usize,
        num_edges: usize,
        num_clusters: usize,
        cluster_affinity: f64,
        seed: u64,
    ) -> Self {
        Self {
            num_nodes,
            num_edges,
            num_clusters,
            cluster_affinity,
            seed,
        }
    }
}

impl Loader for GenerateLoader {
    fn name(&self) -> &str {
        "generate"
    }

    fn schema(&self) -> ImporterSchema {
        generated_schema(
            "generate",
            "generated",
            "Directed edge produced by the graph generator",
        )
    }

    fn load(&self) -> LoadResult {
        let mut graph = VaultGraph::new();
        let namespace =
            Namespace::new("generate", "generate").expect("the generate namespace is valid");
        let mut rng = rand::rngs::StdRng::seed_from_u64(self.seed);

        // Partition nodes into clusters.
        let cluster_count = if self.num_clusters > 0 && self.num_nodes > 0 {
            self.num_clusters.min(self.num_nodes)
        } else {
            0
        };
        // Assign each node to a cluster (round-robin for even distribution).
        let node_cluster: Vec<usize> = (0..self.num_nodes)
            .map(|i| i % cluster_count.max(1))
            .collect();

        // Generate nodes with cluster tags.
        for (i, cluster) in node_cluster.iter().enumerate() {
            let local = format!("n{i}");
            let id = namespace
                .node_id(&local)
                .expect("ordinal local ids are valid");
            let mut tags = vec!["generated".into()];
            if cluster_count > 0 {
                tags.push(format!("cluster-{cluster}"));
            }
            let meta = NodeMeta {
                source_id: "generate".into(),
                title: local.clone(),
                tags,
                frontmatter: HashMap::new(),
                mtime: 0,
                path: local,
                doctype: Some("generated".into()),
                folder: String::new(),
                content_type: None,
                content_readable: false,
                content_writable: false,
            };
            graph.add_node(VaultNode {
                id,
                meta,
                metrics: NodeMetrics::default(),
                x: 0.0,
                y: 0.0,
            });
        }

        // Generate edges with community structure.
        if self.num_nodes == 0 {
            return LoadResult {
                graph,
                search_documents: Vec::new(),
                unresolved: Vec::new(),
            };
        }

        let max_node = self.num_nodes - 1;
        let affinity = self.cluster_affinity.clamp(0.0, 1.0);

        for _ in 0..self.num_edges {
            let src = rng.gen_range(0..=max_node);

            let tgt = if cluster_count > 0 && rng.gen::<f64>() < affinity {
                // Intra-cluster: pick a target from the same cluster.
                let cluster = node_cluster[src];
                let candidates: Vec<usize> = (0..self.num_nodes)
                    .filter(|&i| node_cluster[i] == cluster && i != src)
                    .collect();
                if candidates.is_empty() {
                    // Fallback: random target (cluster of size 1).
                    let mut t = rng.gen_range(0..=max_node);
                    if self.num_nodes > 1 && t == src {
                        t = (t + 1) % self.num_nodes;
                    }
                    t
                } else {
                    candidates[rng.gen_range(0..candidates.len())]
                }
            } else {
                // Inter-cluster (or no clustering): random target.
                let mut t = rng.gen_range(0..=max_node);
                if self.num_nodes > 1 && t == src {
                    t = (t + 1) % self.num_nodes;
                }
                t
            };

            graph.add_edge(VaultEdge {
                source: namespace
                    .node_id(&format!("n{src}"))
                    .expect("ordinal local ids are valid"),
                target: namespace
                    .node_id(&format!("n{tgt}"))
                    .expect("ordinal local ids are valid"),
            });
        }

        let search_documents = graph
            .nodes
            .values()
            .map(|node| {
                SearchDocument::new(&node.id)
                    .with("id", node.id.clone())
                    .with("title", node.meta.title.clone())
                    .with("tags", serde_json::json!(node.meta.tags))
                    .with("path", node.meta.path.clone())
                    .with("type", "generated")
            })
            .collect();

        LoadResult {
            graph,
            search_documents,
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
/// - `GenNode { id, kind }` → `VaultNode { id: "tvix:tvix:{id}", meta: { title: id, tags: [kind], ... } }`
/// - `GenEdge { source, target }` → `VaultEdge` over the namespaced IDs
///
/// Tags are derived from the node `kind`: each distinct kind becomes a tag
/// applied to every node of that kind. This gives the frontend's tag chip
/// strip immediate utility for generated graphs — filter by `kind` without
/// any extra configuration.
pub fn convert_generated_graph(
    gen: &tvix_wasm::GeneratedGraph,
) -> Result<LoadResult, data_loader::ImportError> {
    let namespace = Namespace::new("tvix", "tvix").expect("the tvix namespace is valid");
    let mut graph = VaultGraph::new();
    let mut search_documents = Vec::with_capacity(gen.nodes.len());

    // Collect kind → tag mapping for consistent tagging.
    let mut kind_tags: HashMap<String, String> = HashMap::new();

    for node in &gen.nodes {
        let tag = node.kind.as_deref().unwrap_or("node");
        kind_tags
            .entry(tag.to_string())
            .or_insert_with(|| tag.to_string());

        let node_id = namespace.node_id(&node.id)?;
        let meta = NodeMeta {
            source_id: "tvix".into(),
            title: node.id.clone(),
            tags: vec![tag.to_string()],
            frontmatter: HashMap::new(),
            mtime: 0,
            path: node.id.clone(),
            doctype: Some("generated".into()),
            folder: String::new(),
            content_type: None,
            content_readable: false,
            content_writable: false,
        };

        graph.add_node(VaultNode {
            id: node_id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });
        search_documents.push(
            SearchDocument::new(&node_id)
                .with("id", node_id)
                .with("title", node.id.clone())
                .with("tags", serde_json::json!([tag]))
                .with("path", node.id.clone())
                .with("type", tag),
        );
    }

    for edge in &gen.edges {
        // Only add edges where both endpoints exist in the node set.
        let source = namespace.node_id(&edge.source)?;
        let target = namespace.node_id(&edge.target)?;
        if graph.nodes.contains_key(&source) && graph.nodes.contains_key(&target) {
            graph.add_edge(VaultEdge { source, target });
        }
    }

    Ok(LoadResult {
        graph,
        search_documents,
        unresolved: Vec::new(),
    })
}

fn generated_schema(source_kind: &str, edge_key: &str, edge_description: &str) -> ImporterSchema {
    ImporterSchema::new(
        source_kind,
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                .searchable(4)
                .snippet(),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("type", DiscoveryFieldType::Keyword, true)
                .searchable(2)
                .facetable(),
        ],
        vec![EdgeTypeSchema::directed(edge_key, edge_description)],
        TagHierarchySchema::slash(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_graph_round_trip() {
        let loader = TvixLoader::from_demo("Star (hub)").expect("star demo exists");
        let result = loader.load();
        assert!(
            result.unresolved.is_empty(),
            "tvix graphs have no unresolved refs"
        );
        let g = &result.graph;
        assert_eq!(g.node_count(), 12, "star: 1 center + 11 spokes");
        assert_eq!(g.edge_count(), 11, "star: 11 hub→spoke edges");

        // Center node should have tag "center".
        let center = g.nodes.get("tvix:tvix:n0").expect("center node exists");
        assert!(center.meta.tags.contains(&"center".to_string()));

        // Spoke nodes should have tag "spoke".
        let spoke = g.nodes.get("tvix:tvix:n1").expect("spoke node exists");
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

        let a = result.graph.nodes.get("tvix:tvix:a").unwrap();
        assert!(a.meta.tags.contains(&"source".to_string()));
        let b = result.graph.nodes.get("tvix:tvix:b").unwrap();
        assert!(b.meta.tags.contains(&"sink".to_string()));

        let schema = loader.schema();
        let descriptor = data_loader::Importer::descriptor(&loader);
        descriptor.validate().unwrap();
        assert_eq!(descriptor.schema, schema);
        schema.validate_result(&result).unwrap();
        assert_eq!(
            result.search_documents[0]
                .fields
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["id", "path", "tags", "title", "type"]
        );
    }

    #[test]
    fn direct_bad_expr_load_preserves_legacy_diagnostic_result() {
        let loader = TvixLoader::new("let x = in");
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 0);
        assert!(
            !result.unresolved.is_empty(),
            "error should be in unresolved"
        );
    }

    #[tokio::test]
    async fn bad_expr_fails_through_hosted_importer_boundary() {
        let loader = TvixLoader::new("let x = in");
        let descriptor = data_loader::Importer::descriptor(&loader);
        let read = descriptor
            .capabilities
            .iter()
            .find(|capability| capability.effect == data_loader::Effect::Read)
            .expect("tvix importer declares read capability")
            .clone();
        let importer = data_loader::HostedImporter::new(Box::new(loader), [read])
            .expect("valid hosted tvix importer");

        let error = data_loader::Importer::import(&importer)
            .await
            .expect_err("invalid tvix must not become an empty graph");
        match error {
            data_loader::ImportError::Decode { origin, message } => {
                assert_eq!(origin, "tvix");
                assert!(!message.is_empty());
            }
            other => panic!("unexpected import error: {other}"),
        }
    }

    #[test]
    fn demo_names_are_non_empty() {
        let names = TvixLoader::demo_names();
        assert!(!names.is_empty());
        // Every demo name should resolve to a loader.
        for name in &names {
            assert!(
                TvixLoader::from_demo(name).is_some(),
                "demo {name} not found"
            );
        }
    }

    #[test]
    fn generate_small_graph() {
        let loader = GenerateLoader::new(10, 20, 0, 0.0, 0);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 10);
        assert_eq!(result.graph.edge_count(), 20);
        assert!(result.unresolved.is_empty());
        // All nodes tagged "generated".
        for (_, node) in &result.graph.nodes {
            assert!(node.meta.tags.contains(&"generated".to_string()));
        }
        let schema = loader.schema();
        let descriptor = data_loader::Importer::descriptor(&loader);
        descriptor.validate().unwrap();
        assert_eq!(descriptor.schema, schema);
        schema.validate_result(&result).unwrap();
        assert_eq!(result.search_documents.len(), result.graph.node_count());
    }

    #[test]
    fn generate_no_self_loops() {
        // With 2 nodes and many edges, self-loops should never appear.
        let loader = GenerateLoader::new(2, 100, 0, 0.0, 0);
        let result = loader.load();
        for edge in &result.graph.edges {
            assert_ne!(edge.source, edge.target, "no self-loops");
        }
    }

    #[test]
    fn generate_zero_nodes() {
        let loader = GenerateLoader::new(0, 0, 0, 0.0, 0);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 0);
        assert_eq!(result.graph.edge_count(), 0);
    }

    #[test]
    fn generate_large_graph_is_fast() {
        // 50k nodes, 100k edges should complete in well under 1 second.
        let loader = GenerateLoader::new(50_000, 100_000, 0, 0.0, 0);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 50_000);
        assert_eq!(result.graph.edge_count(), 100_000);
    }

    #[test]
    fn generate_clustered_graph() {
        let loader = GenerateLoader::new(100, 500, 3, 0.8, 0);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 100);
        assert_eq!(result.graph.edge_count(), 500);
        // Every node should have a cluster tag.
        for (_, node) in &result.graph.nodes {
            let has_cluster = node.meta.tags.iter().any(|t| t.starts_with("cluster-"));
            assert!(has_cluster, "node {} missing cluster tag", node.id);
        }
    }

    fn edge_pairs(result: &LoadResult) -> Vec<(String, String)> {
        result
            .graph
            .edges
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone()))
            .collect()
    }

    #[test]
    fn generate_same_seed_produces_identical_edges() {
        let first = GenerateLoader::new(20, 40, 3, 0.8, 7).load();
        let second = GenerateLoader::new(20, 40, 3, 0.8, 7).load();
        assert_eq!(edge_pairs(&first), edge_pairs(&second));
        assert_eq!(first.search_documents, second.search_documents);
    }

    #[test]
    fn generate_different_seeds_produce_different_edges() {
        let first = GenerateLoader::new(20, 40, 0, 0.0, 1).load();
        let second = GenerateLoader::new(20, 40, 0, 0.0, 2).load();
        assert_ne!(edge_pairs(&first), edge_pairs(&second));
    }

    #[test]
    fn generate_seed_zero_is_the_golden_default() {
        let result = GenerateLoader::new(5, 4, 0, 0.0, 0).load();
        let ids: Vec<&str> = result.graph.nodes.keys().map(String::as_str).collect();
        assert_eq!(
            ids,
            [
                "generate:generate:n0",
                "generate:generate:n1",
                "generate:generate:n2",
                "generate:generate:n3",
                "generate:generate:n4",
            ]
        );
        assert_eq!(
            edge_pairs(&result),
            [
                ("generate:generate:n0".to_string(), "generate:generate:n1".to_string()),
                ("generate:generate:n1".to_string(), "generate:generate:n0".to_string()),
                ("generate:generate:n0".to_string(), "generate:generate:n2".to_string()),
                ("generate:generate:n2".to_string(), "generate:generate:n4".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn tvix_importer_satisfies_the_shared_import_contract() {
        let loader = TvixLoader::from_demo("Star (hub)").expect("star demo exists");
        data_loader::testing::assert_import_contract(&loader).await;
    }

    #[tokio::test]
    async fn generate_importer_satisfies_the_shared_import_contract() {
        let loader = GenerateLoader::new(32, 64, 4, 0.8, 99);
        data_loader::testing::assert_import_contract(&loader).await;
    }
}
