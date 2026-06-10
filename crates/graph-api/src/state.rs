//! Shared application state.
//!
//! `AppState` is cloneable (single `Arc`) and stable for the server's
//! lifetime. The *graph data itself* (nodes/edges/derived caches) lives
//! inside an [`ArcSwap<GraphSnapshot>`] so the watcher task (see
//! `watcher.rs`) can atomically swap in a fresh snapshot after a vault
//! reload without coordinating with in-flight HTTP handlers.
//!
//! Handlers read by calling `state.snapshot()` (a single atomic load) at
//! the top of the function. The returned `Arc<GraphSnapshot>` is then
//! held for the duration of the request — even if a swap happens
//! mid-handler, the old snapshot stays alive until the last reader drops
//! its `Arc`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use vault_data::VaultGraph;

use crate::compute_broker::ComputeBroker;
use crate::progress::ProgressLog;
use crate::subprocess::VaultSearch;

/// Everything derived from the on-disk vault. Built by
/// [`GraphSnapshot::build`] and swapped in atomically by the watcher
/// after each reload.
pub struct GraphSnapshot {
    pub graph: VaultGraph,
    /// id (relative path, vault-links convention) -> dense index used for
    /// the binary buffer routes.
    pub id_to_idx: HashMap<String, u32>,
    /// Dense-index ordered list of node ids (parallel to id_to_idx).
    pub idx_to_id: Vec<String>,
    /// Precomputed bulk-numeric binary buffers. Built once per snapshot;
    /// per-request handlers do an `Arc` clone instead of re-walking
    /// `graph.nodes` and re-allocating. Keys: "positions", "edges",
    /// "degree", "pagerank", "kcore", "community", "wcc", "indegree",
    /// "outdegree", "betweenness", "meta_summary".
    pub binary_cache: HashMap<String, Arc<[u8]>>,
}

impl GraphSnapshot {
    /// Build a fresh snapshot from a loaded `VaultGraph`. Recomputes all
    /// derived caches (id_to_idx, idx_to_id, binary buffers).
    pub fn build(graph: VaultGraph) -> Self {
        let mut id_to_idx = HashMap::with_capacity(graph.nodes.len());
        let mut idx_to_id = Vec::with_capacity(graph.nodes.len());
        for (i, (id, _)) in graph.nodes.iter().enumerate() {
            id_to_idx.insert(id.clone(), i as u32);
            idx_to_id.push(id.clone());
        }

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
            "degree",
            "indegree",
            "outdegree",
            "pagerank",
            "betweenness",
            "kcore",
            "community",
            "wcc",
        ] {
            if let Some(buf) = crate::binary::metric_buffer(&graph, name) {
                binary_cache.insert(name.to_string(), Arc::from(buf));
            }
        }
        binary_cache.insert(
            "meta_summary".into(),
            Arc::from(crate::server::build_meta_summary_bytes(&graph)),
        );

        Self {
            graph,
            id_to_idx,
            idx_to_id,
            binary_cache,
        }
    }
}

/// Cloneable handle to the shared application state.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub vault_root: PathBuf,
    /// Atomically swappable graph + derived caches. The watcher task
    /// publishes new snapshots after each vault reload.
    pub snapshot: ArcSwap<GraphSnapshot>,
    /// Optional vault-search subprocess. Swappable so reloads can drop
    /// the old child + respawn against the rebuilt index.
    pub vault_search: ArcSwapOption<VaultSearch>,
    /// When `Some`, /assets/* and / are read from this directory at request
    /// time (dev mode: edit JS/CSS/HTML, refresh browser, no rebuild).
    pub assets_dir: Option<PathBuf>,
    /// gRPC client to a `graph-compute` worker.
    pub compute_broker: ComputeBroker,
    /// Append-only event log mirrored by the frontend's `Progress` UI
    /// (poll via `GET /progress?since=<seq>`). Used by the watcher to
    /// surface "Scanning vault / Loading graph / Rebuilding search
    /// index" task bars in the footer.
    pub progress: Arc<ProgressLog>,
}

impl AppState {
    pub fn new(
        vault_root: PathBuf,
        graph: VaultGraph,
        vault_search: Option<Arc<VaultSearch>>,
        assets_dir: Option<PathBuf>,
        compute_broker: ComputeBroker,
        progress: Arc<ProgressLog>,
    ) -> Self {
        let snapshot = GraphSnapshot::build(graph);
        Self {
            inner: Arc::new(AppStateInner {
                vault_root,
                snapshot: ArcSwap::new(Arc::new(snapshot)),
                vault_search: ArcSwapOption::new(vault_search),
                assets_dir,
                compute_broker,
                progress,
            }),
        }
    }

    /// Single atomic load of the current snapshot. Hold the returned
    /// `Arc` for the duration of the request — swaps elsewhere won't
    /// invalidate it.
    #[inline]
    pub fn snapshot(&self) -> Arc<GraphSnapshot> {
        self.inner.snapshot.load_full()
    }
}
