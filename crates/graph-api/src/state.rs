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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use data_loader::{HostedImporter, ImportError, ImporterSchema, LoadResult, SearchDocument};
use vault_data::VaultGraph;

use crate::compute_broker::ComputeBroker;
use crate::gpu_session::GpuSessionHandle;
use crate::importer_catalog::ImporterCatalog;
use crate::progress::ProgressLog;
use crate::search_index::SearchIndex;

/// Sanitized importer identity associated with one exact snapshot revision.
/// Capability scopes stay in the host descriptor and are never exposed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSource {
    pub id: String,
    pub name: String,
    pub version: String,
}

impl SnapshotSource {
    pub fn new(id: impl Into<String>, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
        }
    }
}

/// Everything derived from the on-disk vault. Built by
/// [`GraphSnapshot::build`] and swapped in atomically by the watcher
/// after each reload.
pub struct GraphSnapshot {
    /// Process-local identity for this complete graph materialization. Every
    /// rebuild receives a fresh non-zero value, even when the node count is
    /// unchanged, so independently fetched buffers and remote layout frames
    /// can prove that they belong to the same snapshot.
    pub revision: u64,
    pub graph: VaultGraph,
    /// Sanitized identity of the source that produced this exact graph.
    pub source: SnapshotSource,
    /// Importer-owned discovery schema associated with this exact revision.
    pub schema: ImporterSchema,
    /// Full-text index built from validated importer documents before the
    /// snapshot becomes visible.
    pub search_index: SearchIndex,
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
    pub fn build(
        graph: VaultGraph,
        source: SnapshotSource,
        schema: ImporterSchema,
        search_documents: Vec<SearchDocument>,
    ) -> Result<Self, ImportError> {
        static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
        schema.validate_output(&graph, &search_documents)?;
        graph.validate().map_err(|error| ImportError::Map {
            message: format!("invalid graph snapshot: {error}"),
        })?;
        let search_index =
            SearchIndex::build(&schema, &search_documents).map_err(|error| ImportError::Map {
                message: format!("build discovery index: {error:#}"),
            })?;
        let revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        assert_ne!(revision, 0, "graph revision counter exhausted");

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
            Arc::from(crate::server::build_meta_summary_bytes(
                &graph,
                &schema,
                &search_documents,
            )?),
        );

        Ok(Self {
            revision,
            graph,
            source,
            schema,
            search_index,
            id_to_idx,
            idx_to_id,
            binary_cache,
        })
    }
}

/// Cloneable handle to the shared application state.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub vault_root: PathBuf,
    /// The active importer pipeline. Existing synchronous loaders are adapted
    /// through `data_loader`'s compatibility implementation.
    pub importer: HostedImporter,
    /// Atomically swappable graph + derived caches. The watcher task
    /// publishes new snapshots after each vault reload.
    pub snapshot: ArcSwap<GraphSnapshot>,
    /// When `Some`, /assets/* and / are read from this directory at request
    /// time (dev mode: edit JS/CSS/HTML, refresh browser, no rebuild).
    pub assets_dir: Option<PathBuf>,
    /// gRPC client to a `graph-compute` worker.
    pub compute_broker: ComputeBroker,
    /// On-demand GPU session controller (Kueue/KubeRay RayCluster
    /// lifecycle). `None` when the feature is disabled — no template mounted
    /// or no kube client — so local dev is completely unaffected (R11).
    pub gpu_session: Option<GpuSessionHandle>,
    /// Append-only event log mirrored by the frontend's `Progress` UI
    /// (poll via `GET /progress?since=<seq>`). Used by the watcher to
    /// surface "Scanning vault / Loading graph / Rebuilding search
    /// index" task bars in the footer.
    pub progress: Arc<ProgressLog>,
    /// Bounded, non-secret deployment source catalog exposed read-only at
    /// `GET /importers`. This never grants effects or changes the active
    /// process-lifetime importer.
    pub importer_catalog: ImporterCatalog,
    /// Push reload trigger for `WatchPlan::Push` importers. `Some` only when
    /// the host wired one via [`AppState::with_push_trigger`]; each received
    /// tick runs a full snapshot rebuild (see `watcher.rs`). Standalone
    /// graph-api never sets this — push sources without a trigger keep the
    /// old warn-and-disable behavior.
    pub push_trigger: Option<tokio::sync::watch::Receiver<u64>>,
}

impl AppState {
    pub fn new(
        vault_root: PathBuf,
        importer: HostedImporter,
        loaded: LoadResult,
        assets_dir: Option<PathBuf>,
        compute_broker: ComputeBroker,
        progress: Arc<ProgressLog>,
    ) -> Result<Self, ImportError> {
        Self::new_with_importer_catalog(
            vault_root,
            importer,
            loaded,
            assets_dir,
            compute_broker,
            progress,
            ImporterCatalog::default(),
        )
    }

    /// Construct application state with a validated deployment-owned source
    /// catalog. The legacy [`Self::new`] constructor intentionally remains so
    /// embedders and tests that do not need catalog metadata keep working.
    pub fn new_with_importer_catalog(
        vault_root: PathBuf,
        importer: HostedImporter,
        loaded: LoadResult,
        assets_dir: Option<PathBuf>,
        compute_broker: ComputeBroker,
        progress: Arc<ProgressLog>,
        importer_catalog: ImporterCatalog,
    ) -> Result<Self, ImportError> {
        let descriptor = importer.descriptor();
        let source = SnapshotSource::new(&descriptor.id, &descriptor.name, &descriptor.version);
        let snapshot = GraphSnapshot::build(
            loaded.graph,
            source,
            descriptor.schema,
            loaded.search_documents,
        )?;
        Ok(Self {
            inner: Arc::new(AppStateInner {
                vault_root,
                importer,
                snapshot: ArcSwap::new(Arc::new(snapshot)),
                assets_dir,
                compute_broker,
                gpu_session: None,
                progress,
                importer_catalog,
                push_trigger: None,
            }),
        })
    }

    /// Attach a push-trigger receiver for a `WatchPlan::Push` importer.
    /// Consumes + returns self so the host can chain it onto the constructor
    /// before the state is shared; if the `Arc` is already shared the
    /// receiver is dropped with a warning (a startup ordering bug, not a
    /// runtime hazard).
    pub fn with_push_trigger(mut self, rx: tokio::sync::watch::Receiver<u64>) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => inner.push_trigger = Some(rx),
            None => tracing::warn!("push trigger dropped: AppState already shared"),
        }
        self
    }

    /// Attach the GPU session controller handle. Consumes + returns self so
    /// `main` can chain it onto the constructor before the state is shared;
    /// if the `Arc` is already shared the handle is dropped with a warning
    /// (a startup ordering bug, not a runtime hazard).
    pub fn with_gpu_session(mut self, handle: Option<GpuSessionHandle>) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => inner.gpu_session = handle,
            None => tracing::warn!("gpu session handle dropped: AppState already shared"),
        }
        self
    }

    /// Single atomic load of the current snapshot. Hold the returned
    /// `Arc` for the duration of the request — swaps elsewhere won't
    /// invalidate it.
    #[inline]
    pub fn snapshot(&self) -> Arc<GraphSnapshot> {
        self.inner.snapshot.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilt_snapshots_receive_distinct_nonzero_revisions() {
        let schema = test_schema();
        let first = GraphSnapshot::build(
            VaultGraph::default(),
            SnapshotSource::new("test", "Test", "1"),
            schema.clone(),
            Vec::new(),
        )
        .unwrap();
        let second = GraphSnapshot::build(
            VaultGraph::default(),
            SnapshotSource::new("test", "Test", "1"),
            schema,
            Vec::new(),
        )
        .unwrap();
        assert_ne!(first.revision, 0);
        assert_ne!(second.revision, 0);
        assert_ne!(first.revision, second.revision);
    }

    fn test_schema() -> ImporterSchema {
        use data_loader::{DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, TagHierarchySchema};

        ImporterSchema::new(
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(3),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(2)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed("relationship", "test")],
            TagHierarchySchema::slash(),
        )
    }
}
