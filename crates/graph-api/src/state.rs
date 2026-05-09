//! Shared application state.
//
// Future: AppState may move behind an Arc<RwLock<>> if we add live vault
// reload via filesystem watcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use vault_data::VaultGraph;

use crate::compute_broker::ComputeBroker;
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
    /// When `Some`, /assets/* and / are read from this directory at request
    /// time (dev mode: edit JS/CSS/HTML, refresh browser, no rebuild).
    /// When `None`, served from the embedded `graph-renderer` bundle.
    pub assets_dir: Option<PathBuf>,
    /// Precomputed bulk-numeric binary buffers. Built once at AppState
    /// construction so per-request handlers do an `Arc` clone instead of
    /// re-walking `graph.nodes` and re-allocating the byte vec on every
    /// hit. Keys: "positions", "edges", "degree", "pagerank", "kcore",
    /// "community", "wcc", "indegree", "outdegree", "betweenness".
    pub binary_cache: HashMap<String, Arc<[u8]>>,
    /// Optional gRPC client to a `graph-compute` worker. When the dial at
    /// boot fails, this is still present but `subscribe()` returns `None`,
    /// and `/graph/layout/stream` returns 503.
    pub compute_broker: ComputeBroker,
}

impl AppState {
    pub fn new(
        vault_root: PathBuf,
        graph: VaultGraph,
        vault_search: Option<Arc<VaultSearch>>,
        assets_dir: Option<PathBuf>,
        compute_broker: ComputeBroker,
    ) -> Self {
        let mut id_to_idx = HashMap::with_capacity(graph.nodes.len());
        let mut idx_to_id = Vec::with_capacity(graph.nodes.len());
        for (i, (id, _)) in graph.nodes.iter().enumerate() {
            id_to_idx.insert(id.clone(), i as u32);
            idx_to_id.push(id.clone());
        }

        // Precompute every bulk-numeric buffer once. The graph is
        // immutable for the server's lifetime, so per-request handlers
        // become `Arc<[u8]>::clone()` (one ptr inc) + a Cache-Control
        // header. This collapses /graph/metrics/community from "walk
        // 50k IndexMap entries + 200KB realloc" to a pointer copy.
        let mut binary_cache: HashMap<String, Arc<[u8]>> = HashMap::new();
        binary_cache.insert(
            "positions".into(),
            Arc::from(crate::binary::positions_buffer(&graph)),
        );
        binary_cache.insert(
            "edges".into(),
            Arc::from(crate::binary::edges_buffer(&graph, &id_to_idx)),
        );
        for name in [
            "degree", "indegree", "outdegree", "pagerank",
            "betweenness", "kcore", "community", "wcc",
        ] {
            if let Some(buf) = crate::binary::metric_buffer(&graph, name) {
                binary_cache.insert(name.to_string(), Arc::from(buf));
            }
        }

        // meta_summary protobuf — same Arc<[u8]> serve path as the
        // metric buffers above, but content-type is x-protobuf.
        binary_cache.insert(
            "meta_summary".into(),
            Arc::from(crate::server::build_meta_summary_bytes(&graph)),
        );

        Self {
            inner: Arc::new(AppStateInner {
                vault_root,
                graph,
                id_to_idx,
                idx_to_id,
                vault_search,
                assets_dir,
                binary_cache,
                compute_broker,
            }),
        }
    }
}
