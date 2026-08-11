//! Re-namespacing of vault-links extraction results.
//!
//! [`vault_links::try_extract_vault`] publishes under the fixed
//! `obsidian:obsidian:` namespace with content flags for a mounted,
//! user-editable vault. The GitHub transport keeps every local part
//! byte-identical (same parser, same vault-relative paths) but moves the
//! graph into the configured `github:{source_id}:` namespace and drops the
//! content flags: the corpus lives in a remote repository cache, so graph-api
//! cannot resolve or persist note content for it.

use data_loader::{identity::Namespace, ImportError, LoadResult, SearchDocument};
use vault_data::VaultGraph;
use vault_links::ExtractionResult;

/// The fixed namespace vault-links publishes under (see its extractor).
const OBSIDIAN_PREFIX: &str = "obsidian:obsidian:";

/// Move one extraction result from the `obsidian:obsidian:` namespace into
/// `namespace`, preserving local parts, edge topology, and search fields.
pub fn renamespace(
    result: ExtractionResult,
    namespace: &Namespace,
) -> Result<LoadResult, ImportError> {
    let remap_id = |id: &str| -> Result<String, ImportError> {
        let Some(local) = id.strip_prefix(OBSIDIAN_PREFIX) else {
            return Err(ImportError::Map {
                message: format!(
                    "vault-links emitted node ID {id:?} outside its {OBSIDIAN_PREFIX:?} namespace"
                ),
            });
        };
        namespace.node_id(local)
    };

    let mut graph = VaultGraph::new();
    for (key, mut node) in result.graph.nodes {
        let new_id = remap_id(&key)?;
        node.id = new_id;
        node.meta.source_id = namespace.source_id().to_string();
        node.meta.content_readable = false;
        node.meta.content_writable = false;
        graph.try_add_node(node).map_err(|error| ImportError::Map {
            message: format!("re-namespaced GitHub node collision: {error}"),
        })?;
    }
    for mut edge in result.graph.edges {
        edge.source = remap_id(&edge.source)?;
        edge.target = remap_id(&edge.target)?;
        graph.add_edge(edge);
    }

    let mut search_documents: Vec<SearchDocument> = Vec::with_capacity(result.search_documents.len());
    for mut document in result.search_documents {
        let new_id = remap_id(&document.node_id)?;
        document.node_id = new_id.clone();
        if let Some(value) = document.fields.get_mut("id") {
            *value = serde_json::Value::String(new_id);
        }
        search_documents.push(document);
    }

    Ok(LoadResult {
        graph,
        search_documents,
        unresolved: result.unresolved,
    })
}
