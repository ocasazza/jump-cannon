//! Shared application state.
//
// Future: AppState may move behind an Arc<RwLock<>> if we add live vault
// reload via filesystem watcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use vault_data::VaultGraph;

use crate::subprocess::VaultSearch;

/// Immutable, shared backend state. Cloning `AppState` clones the inner Arc.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub vault_root: PathBuf,
    pub graph: VaultGraph,
    /// id (relative path, vault-links convention) -> dense index used for
    /// the binary buffer routes.
    pub id_to_idx: HashMap<String, u32>,
    /// Dense-index ordered list of node ids (parallel to id_to_idx).
    pub idx_to_id: Vec<String>,
    /// Optional vault-search subprocess. When `Some`, the `/search` handler
    /// proxies to its `/ids` endpoint; when `None`, falls back to a naive
    /// title-contains scan.
    pub vault_search: Option<Arc<VaultSearch>>,
}

impl AppState {
    pub fn new(
        vault_root: PathBuf,
        graph: VaultGraph,
        vault_search: Option<Arc<VaultSearch>>,
    ) -> Self {
        let mut id_to_idx = HashMap::with_capacity(graph.nodes.len());
        let mut idx_to_id = Vec::with_capacity(graph.nodes.len());
        for (i, (id, _)) in graph.nodes.iter().enumerate() {
            id_to_idx.insert(id.clone(), i as u32);
            idx_to_id.push(id.clone());
        }
        Self {
            inner: Arc::new(AppStateInner {
                vault_root,
                graph,
                id_to_idx,
                idx_to_id,
                vault_search,
            }),
        }
    }
}
