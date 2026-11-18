//! Native water-sheet importer: serves the `waterSheet` topology from
//! `crates/native-graph-generator` as a graph-api source, with no tvix
//! evaluation, vault claim, or filesystem root.
//!
//! The generator is the pure topology builder (dense node ids, SoA edges);
//! this crate is the importer shell — identity, discovery schema, and the
//! projection into `VaultGraph` + `SearchDocument`s. Node IDs mirror the
//! `waterSheet` Nix generators' local names (`w{m}_O` / `w{m}_H1` /
//! `w{m}_H2`, sheet cells `s{i}`) under the `native:{source_id}:` namespace,
//! so a graph is deterministic for a given `(molecules, rows, cols, source_id)`.
//!
//! The source is generated in-process: there is no remote endpoint, no
//! filesystem read, and no watch — the descriptor advertises exactly one
//! in-memory `Read` capability and a static watch plan.

use std::collections::HashSet;

use data_loader::identity::Namespace;
use data_loader::{
    Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportFuture,
    ImportOutcome, ImportProgress, Importer, ImporterDescriptor, ImporterSchema, LoadResult,
    SearchDocument, TagHierarchySchema, Transport, WatchPlan,
};
use native_graph_generator::{WaterSheetGenerator, WATER_H, WATER_O};
use vault_data::{NodeMeta, VaultEdge, VaultGraph, VaultNode};

/// Upper bound on generated nodes. The generator is unbounded by design
/// (it is a benchmark primitive); the importer shell refuses configs that
/// would exhaust memory instead of aborting inside an allocation.
pub const MAX_NATIVE_NODES: u64 = 500_000_000;

/// Upper bound on generated edges (sheet edges alone approach `2 * rows * cols`).
pub const MAX_NATIVE_EDGES: u64 = 1_000_000_000;

/// Configuration for one native water-sheet source instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSourceConfig {
    /// Stable source-instance identifier namespacing every node ID
    /// (`[a-z0-9._-]{1,128}` per the identity contract).
    pub source_id: String,
    /// Number of disconnected 3-node water molecules.
    pub molecules: u64,
    /// Sheet lattice rows.
    pub rows: u64,
    /// Sheet lattice columns.
    pub cols: u64,
}

impl NativeSourceConfig {
    /// Reject configurations that could never produce a conformant import.
    pub fn validate(&self) -> Result<(), data_loader::ImportError> {
        data_loader::identity::validate_source_id(&self.source_id).map_err(|message| {
            data_loader::ImportError::InvalidDescriptor {
                message: format!("native {message}"),
            }
        })?;
        // u64 inputs can overflow the count arithmetic; checked_* fail the
        // import loudly instead of wrapping into a bogus bound check.
        let generator = WaterSheetGenerator::new(self.molecules, self.rows, self.cols);
        let nodes = generator
            .molecules
            .checked_mul(3)
            .and_then(|molecules| molecules.checked_add(generator.sheet_cells()))
            .ok_or_else(|| data_loader::ImportError::InvalidDescriptor {
                message: "native generator node count overflows u64".into(),
            })?;
        if nodes > MAX_NATIVE_NODES {
            return Err(data_loader::ImportError::InvalidDescriptor {
                message: format!(
                    "native generator would build {} nodes; the bound is {MAX_NATIVE_NODES}",
                    generator.expected_nodes()
                ),
            });
        }
        if generator.expected_edges() > MAX_NATIVE_EDGES {
            return Err(data_loader::ImportError::InvalidDescriptor {
                message: format!(
                    "native generator would build {} edges; the bound is {MAX_NATIVE_EDGES}",
                    generator.expected_edges()
                ),
            });
        }
        Ok(())
    }
}

/// An [`Importer`] that publishes the deterministic water-sheet graph under
/// the `native:{source_id}:` namespace.
pub struct NativeImporter {
    config: NativeSourceConfig,
    namespace: Namespace,
    generator: WaterSheetGenerator,
}

impl NativeImporter {
    /// Build an importer over the given source configuration.
    pub fn new(config: NativeSourceConfig) -> Result<Self, data_loader::ImportError> {
        config.validate()?;
        let namespace = Namespace::new("native", &config.source_id)?;
        let generator = WaterSheetGenerator::new(config.molecules, config.rows, config.cols);
        Ok(Self {
            config,
            namespace,
            generator,
        })
    }

    /// The validated identity namespace (`native:{source_id}:`).
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// The local-ID name of generator node `index`, mirroring the `waterSheet`
    /// Nix generators: `w{m}_O` / `w{m}_H1` / `w{m}_H2` for the molecule
    /// triples, `s{i}` for the sheet cells.
    fn local_name(&self, index: u64) -> String {
        let molecule_nodes = self.generator.molecules * 3;
        if index < molecule_nodes {
            let m = index / 3;
            match index % 3 {
                0 => format!("w{m}_O"),
                1 => format!("w{m}_H1"),
                _ => format!("w{m}_H2"),
            }
        } else {
            format!("s{}", index - molecule_nodes)
        }
    }

    /// Build the full import result in one pass. Pure over the config: the
    /// same `(molecules, rows, cols)` always yields the same graph.
    fn build_result(&self) -> Result<LoadResult, data_loader::ImportError> {
        let graph = self.generator.generate();

        // Canonical node IDs first so edge endpoints reference them without
        // re-formatting: one String per node, reused for the edge lists.
        let mut node_ids = Vec::with_capacity(graph.node_ids.len());
        let mut nodes = VaultGraph::new();
        let mut documents = Vec::with_capacity(graph.node_ids.len());
        for (index, &type_tag) in graph.node_types.iter().enumerate() {
            let index = index as u64;
            let local = self.local_name(index);
            let node_id = self.namespace.node_id(&local)?;
            let (title, tags, doc_type) = match type_tag {
                WATER_O => (local, vec!["water".into(), "water-O".into()], "water-O"),
                WATER_H => (local, vec!["water".into(), "water-H".into()], "water-H"),
                _ => (local, vec!["sheet".into()], "sheet"),
            };
            let meta = NodeMeta {
                source_id: self.config.source_id.clone(),
                title: title.clone(),
                tags: tags.clone(),
                ..Default::default()
            };
            nodes
                .try_add_node(VaultNode {
                    id: node_id.clone(),
                    meta,
                    ..Default::default()
                })
                .map_err(|error| data_loader::ImportError::Map {
                    message: error.to_string(),
                })?;
            let mut document = SearchDocument::new(node_id.clone());
            document.insert("id", node_id.clone());
            document.insert("title", title);
            document.insert("tags", tags.clone());
            document.insert("type", doc_type);
            documents.push(document);
            node_ids.push(node_id);
        }

        // Molecule bonds are O-H pairs and sheet bonds right/down neighbours,
        // so every generator edge is unique and self-loop-free — but the
        // importer contract is defensive: dedupe unordered endpoint pairs and
        // drop self-loops rather than trusting the generator's ordering.
        let mut seen = HashSet::with_capacity(graph.edge_sources.len());
        for (&source, &target) in graph
            .edge_sources
            .iter()
            .zip(graph.edge_targets.iter())
        {
            if source == target {
                continue;
            }
            let key = if source < target {
                (source, target)
            } else {
                (target, source)
            };
            if !seen.insert(key) {
                continue;
            }
            let (a, b) = (node_ids[source as usize].clone(), node_ids[target as usize].clone());
            nodes.edges.push(VaultEdge::new(a, b));
        }
        debug_assert_eq!(nodes.node_count(), self.generator.expected_nodes() as usize);
        debug_assert_eq!(nodes.edge_count(), self.generator.expected_edges() as usize);

        Ok(LoadResult {
            graph: nodes,
            search_documents: documents,
            // Generated graphs are self-consistent: no unresolved references.
            unresolved: Vec::new(),
        })
    }
}

impl Importer for NativeImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        // Generated at startup with no external source: one in-memory Read
        // grant, no watch, no content. Content schema stays non-readable and
        // non-writable, matching the absent ContentRead/Write capabilities.
        ImporterDescriptor::new(
            format!("native.{}", self.config.source_id),
            format!(
                "Native water sheet ({} molecules, {}x{} sheet)",
                self.config.molecules, self.config.rows, self.config.cols
            ),
            env!("CARGO_PKG_VERSION"),
            vec![Capability::new(
                Effect::Read,
                Transport::InMemory,
                format!("native:{}", self.config.source_id),
            )],
            native_schema(),
        )
        .with_watch(WatchPlan::Static)
    }

    fn import<'a>(
        &'a self,
        progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<ImportOutcome, data_loader::ImportError>> {
        Box::pin(async move {
            let stage = progress.stage("Building native water sheet");
            let result = self.build_result()?;
            progress.finish(stage);
            Ok(ImportOutcome::Loaded(result))
        })
    }
}

/// The native discovery schema. `type` carries the waterSheet node type
/// (water-O / water-H / sheet) as the facetable keyword, mirroring the Nix
/// generator's `type` metadata. Edges are the conventional `generated`
/// vocabulary and undirected: molecule bonds and lattice neighbours have no
/// direction.
fn native_schema() -> ImporterSchema {
    ImporterSchema::new(
        "native",
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                .searchable(4)
                .snippet(),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("type", DiscoveryFieldType::Keyword, false)
                .searchable(2)
                .facetable(),
        ],
        vec![EdgeTypeSchema {
            key: "generated".into(),
            directed: false,
            description: Some(
                "Water molecule bond or sheet-lattice neighbour produced by the native generator"
                    .into(),
            ),
        }],
        TagHierarchySchema::slash(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_loader::ImportError;

    fn config(molecules: u64, rows: u64, cols: u64) -> NativeSourceConfig {
        NativeSourceConfig {
            source_id: "default".into(),
            molecules,
            rows,
            cols,
        }
    }

    #[test]
    fn default_config_produces_expected_counts() {
        let importer = NativeImporter::new(config(20_000, 256, 256)).unwrap();
        let result = importer.build_result().unwrap();
        // 3 nodes per molecule plus one per sheet cell.
        assert_eq!(result.graph.node_count(), 20_000 * 3 + 256 * 256);
        // 2 bonds per molecule plus right+down lattice bonds.
        assert_eq!(result.graph.edge_count(), 20_000 * 2 + 256 * 256 * 2 - 256 - 256);
        assert_eq!(result.search_documents.len(), result.graph.node_count());
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn descriptor_and_result_satisfy_the_full_contract() {
        let importer = NativeImporter::new(config(7, 3, 4)).unwrap();
        let descriptor = importer.descriptor();
        descriptor.validate().expect("descriptor validates");

        let result = importer.build_result().unwrap();
        descriptor
            .schema
            .validate_result(&result)
            .expect("import satisfies the discovery schema");

        // Node IDs are exactly native:{source_id}:{local} with waterSheet
        // local names.
        let namespace = Namespace::new("native", "default").unwrap();
        assert_eq!(
            namespace.node_id("w0_O").unwrap(),
            "native:default:w0_O"
        );
        let oxygen = namespace.node_id("w0_O").unwrap();
        let hydrogen = namespace.node_id("w0_H1").unwrap();
        assert!(
            result
                .graph
                .edges
                .iter()
                .any(|edge| edge.source == oxygen && edge.target == hydrogen)
        );
        // Untyped edges: typed kinds collapse to untyped on the canvas.
        assert!(result.graph.edges.iter().all(|edge| edge.kind.is_none()));

        // One document per node; water nodes carry both facets, sheet one.
        let sheet = result
            .search_documents
            .iter()
            .find(|doc| doc.node_id.ends_with(":s0"))
            .expect("sheet cell document");
        assert_eq!(
            sheet.fields.get("tags").unwrap(),
            &serde_json::json!(["sheet"])
        );
    }

    #[test]
    fn invalid_source_id_is_rejected_loudly() {
        let built = NativeImporter::new(NativeSourceConfig {
            source_id: "has:colon".into(),
            ..config(1, 1, 1)
        });
        assert!(matches!(
            built,
            Err(ImportError::InvalidDescriptor { .. })
        ));
    }

    #[test]
    fn over_bound_configs_fail_instead_of_truncating() {
        let built = NativeImporter::new(config(u64::MAX / 2, 1, 1));
        assert!(matches!(
            built,
            Err(ImportError::InvalidDescriptor { .. })
        ));
    }
}
