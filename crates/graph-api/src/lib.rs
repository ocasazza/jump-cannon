//! graph-api — axum backend exposing the vault graph as protobuf + binary endpoints.
//!
//! Wire format split:
//!   - bulk numeric (positions, edges, metrics): raw little-endian arrays
//!   - structured (init manifest, node metadata, search results): protobuf
//
// Future: this crate's lib surface is consumed by integration tests; `main.rs`
// is the CLI entry point.

pub mod binary;
pub mod browser;
pub mod compute_broker;
pub mod gpu_session;
pub mod importer_catalog;
pub mod importer_editor;
pub mod importer_package;
pub mod progress;
pub mod proto;
pub mod search_index;
pub mod server;
pub mod source_host;
pub mod state;
pub mod vault_loader;
pub mod watcher;

pub use server::{api_router, api_router_with_host, router, router_with_host};
pub use state::AppState;
pub mod attribute_resolver;

/// Build the [`AppState`] for one hosted importer: pair the importer with the
/// host-selected exact grants, run the initial load with progress, and
/// construct the state with no assets dir and a fresh (unconnected) compute
/// broker — the same sequence graph-api's `main.rs` performs for its single
/// tenant. The session-manager server uses this to build one serving state
/// per world without duplicating the wiring.
pub async fn build_world_state(
    importer: Box<dyn data_loader::Importer>,
    grants: std::collections::HashSet<data_loader::Capability>,
    vault_root: std::path::PathBuf,
    progress: std::sync::Arc<progress::ProgressLog>,
) -> Result<AppState, data_loader::ImportError> {
    let importer = data_loader::HostedImporter::new(importer, grants)?;
    let loaded = match vault_loader::load_with_progress(&importer, Some(&progress)).await? {
        data_loader::ImportOutcome::Loaded(loaded) => loaded,
        // Defensive: an initial build has no prior snapshot to keep, and a
        // cold importer cannot report Unchanged. If one ever does, fail the
        // build loudly rather than installing an empty world.
        data_loader::ImportOutcome::Unchanged => {
            return Err(data_loader::ImportError::Map {
                message: format!(
                    "importer {} reported the source unchanged before the first load; \
                     no snapshot exists to keep",
                    importer.descriptor().id
                ),
            });
        }
    };
    // The snapshot build (Tantivy index, binary caches, metrics) is CPU-heavy
    // and synchronous; keep it off the async runtime so it doesn't starve HTTP
    // handlers during a large import.
    let vault_root2 = vault_root.clone();
    let progress2 = std::sync::Arc::clone(&progress);
    tokio::task::spawn_blocking(move || {
        AppState::new(
            vault_root2,
            importer,
            loaded,
            None,
            compute_broker::ComputeBroker::new(),
            progress2,
        )
    })
    .await
    .map_err(|error| data_loader::ImportError::Map {
        message: format!("snapshot build panicked: {error}"),
    })?
}
