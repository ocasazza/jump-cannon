//! `WorldImporter`: a [`data_loader::Importer`] whose source is a world's
//! [`graph_vcs`] store (feature `server`, native only).
//!
//! Every `import()` materializes the head of one branch and converts the
//! snapshot into a [`LoadResult`]: nodes become graph nodes (with the
//! normalizations the discovery contract requires — non-empty `source_id`
//! and `title`), edges become `link` edges (dangling endpoints are dropped
//! into `unresolved`, since commits like "add an edge before its target
//! node" are legal in the VCS but not in a published graph), and every node
//! gets one [`SearchDocument`] projecting the fixed world schema.
//!
//! The descriptor declares `WatchPlan::Push`: the serving side rebuilds when
//! the session-manager server fires the world's push trigger after a
//! head-moving VCS mutation.
//!
//! Frontmatter values are NOT projected into search documents in this
//! milestone: the discovery schema is fixed per importer, so arbitrary
//! user-authored frontmatter keys cannot be declared. They remain available
//! on `NodeMeta.frontmatter` via `/node/*id`. Declaring a bounded frontmatter
//! projection is deferred.

use data_loader::{
    Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportError,
    ImportFuture, Importer, ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument,
    TagHierarchySchema, Transport, WatchPlan,
};
use graph_vcs::{Snapshot, VcsStore};
use std::sync::Arc;
use vault_data::{VaultEdge, VaultGraph};

use crate::WorldId;

/// The importer id / snapshot source id every world serves under.
pub const WORLD_SOURCE_ID: &str = "world";

/// A graph importer reading a world's versioned snapshot.
pub struct WorldImporter {
    store: Arc<dyn VcsStore>,
    branch: String,
    scope: String,
}

impl WorldImporter {
    /// An importer over `store`'s `branch` head. `world` namespaces the
    /// declared capability scopes (`world:<slug>`).
    pub fn new(store: Arc<dyn VcsStore>, branch: impl Into<String>, world: &WorldId) -> Self {
        Self {
            store,
            branch: branch.into(),
            scope: format!("world:{}", world.0),
        }
    }

    /// The fixed discovery schema every world serves: the mandatory core
    /// (`id` keyword, `title` text, `tags` keyword list — required,
    /// searchable, tags facetable) plus `path`/`type` keywords and an
    /// optional facetable `folder`, with one directed `link` edge type.
    pub fn schema() -> ImporterSchema {
        ImporterSchema::new(
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
                // Optional: keyword fields must be non-empty, so nodes with
                // no folder simply omit the document field.
                DiscoveryField::new("folder", DiscoveryFieldType::Keyword, false).facetable(),
            ],
            vec![EdgeTypeSchema::directed("link", "World graph link")],
            TagHierarchySchema::slash(),
        )
    }

    fn descriptor_for(&self) -> ImporterDescriptor {
        ImporterDescriptor::new(
            WORLD_SOURCE_ID,
            "Shared World",
            "1",
            // Minimal capability set: an in-memory read of the world's store
            // plus the watch grant the push change driver preflights.
            vec![
                Capability::new(Effect::Read, Transport::InMemory, self.scope.clone()),
                Capability::new(Effect::Watch, Transport::InMemory, self.scope.clone()),
            ],
            Self::schema(),
        )
        .with_watch(WatchPlan::Push)
    }
}

impl Importer for WorldImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        self.descriptor_for()
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move {
            // A head-less world (no commits) still serves one valid empty
            // snapshot.
            let snapshot = match self.store.head(&self.branch).await {
                Ok(Some(head)) => {
                    self.store
                        .materialize(&head)
                        .await
                        .map_err(|error| ImportError::SourceRead {
                            origin: self.scope.clone(),
                            message: format!("materialize {}: {error}", head.0),
                        })?
                }
                Ok(None) => Snapshot::default(),
                Err(error) => {
                    return Err(ImportError::SourceRead {
                        origin: self.scope.clone(),
                        message: error.to_string(),
                    })
                }
            };
            Ok(snapshot_to_load_result(snapshot))
        })
    }
}

/// Convert a VCS snapshot into a validated [`LoadResult`].
fn snapshot_to_load_result(snapshot: Snapshot) -> LoadResult {
    let mut graph = VaultGraph::new();
    let mut search_documents = Vec::with_capacity(snapshot.nodes.len());
    for (_id, mut node) in snapshot.nodes {
        // Normalizations the discovery contract requires of every published
        // node. Committed nodes may leave these empty.
        if node.meta.source_id.trim().is_empty() {
            node.meta.source_id = WORLD_SOURCE_ID.to_string();
        }
        if node.meta.title.trim().is_empty() {
            node.meta.title = node.id.clone();
        }
        if node.meta.path.trim().is_empty() {
            node.meta.path = node.id.clone();
        }
        // World content is not filesystem-backed; never advertise source
        // content effects the serving side cannot honor.
        node.meta.content_readable = false;
        node.meta.content_writable = false;

        let mut document = SearchDocument::new(&node.id)
            .with("id", node.id.clone())
            .with("title", node.meta.title.clone())
            .with("tags", serde_json::json!(node.meta.tags))
            .with("path", node.meta.path.clone())
            .with(
                "type",
                node.meta
                    .doctype
                    .clone()
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| "note".to_string()),
            );
        if !node.meta.folder.trim().is_empty() {
            document.insert("folder", node.meta.folder.clone());
        }
        search_documents.push(document);
        graph.add_node(node);
    }

    // Edges whose endpoints are not both present cannot be published
    // (`VaultGraph::validate` rejects them); report them as unresolved.
    let mut unresolved = Vec::new();
    for edge in snapshot.edges {
        if graph.nodes.contains_key(&edge.source) && graph.nodes.contains_key(&edge.target) {
            graph.add_edge(VaultEdge {
                source: edge.source,
                target: edge.target,
            });
        } else {
            unresolved.push(format!("edge {} -> {}", edge.source, edge.target));
        }
    }

    LoadResult {
        graph,
        search_documents,
        unresolved,
    }
}
