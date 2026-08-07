//! Change drivers that reload the active importer.
//!
//! Filesystem importers use `notify-debouncer-mini` plus an optional periodic
//! full-rescan fallback; polling importers use a Tokio interval; static
//! importers install no change driver. Filesystem bursts are coalesced into one
//! reload, then:
//!
//!   1. Re-runs `vault_loader::load_with_progress` (emits "Scanning
//!      vault", "Computing graph metrics", "Seeding layout positions"
//!      task bars to `ProgressLog`).
//!   2. Builds a fresh `GraphSnapshot` and `ArcSwap`s it into the
//!      `AppState`. In-flight readers keep the previous `Arc` valid.
//!   3. The importer-driven Tantivy index and schema-driven facets are built
//!      into that same snapshot before the single atomic swap.
//!
//! ## Filter
//!
//! Obsidian ignores `.git`, `node_modules`, `.obsidian`, and leading-dot paths.
//! Obsidian accepts only `.md`; schema-driven importers otherwise retain their
//! own event and path semantics rather than inheriting Obsidian exclusions.
//!
//! ## Container caveats
//!
//! On Linux, inotify events for a bind-mounted directory only fire when
//! the *guest* container's kernel sees the write. Edits made on the
//! host fs are propagated through the OCI bind mount on most engines
//! (podman, docker on Linux). On macOS Docker Desktop / Lima, fs
//! events are heavily debounced by the virtualization layer — a 1-2s
//! lag is normal. Writes through another pod or mount may not produce an
//! event at all, so filesystem importers can also run a periodic full rescan.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use data_loader::{Effect, HostedImporter, ImporterDescriptor, Transport, WatchPlan};

use crate::state::{AppState, GraphSnapshot, SnapshotSource};

/// Spawn the filesystem watcher + reload task. Returns immediately; the
/// watcher and reload loop run for the lifetime of the process.
///
/// `state` is the live `AppState`; the watcher swaps new snapshots into
/// it under `state.inner.snapshot`.
pub fn spawn(state: AppState, filesystem_rescan_seconds: u64) {
    let descriptor = state.inner.importer.descriptor();
    if !watch_is_authorized(&state.inner.importer, &descriptor) {
        let message = format!(
            "{} declares a change driver without an exact watch grant; change driver disabled",
            descriptor.id
        );
        state.inner.progress.warn("watch", &message);
        tracing::warn!(importer = %descriptor.id, "{message}");
        return;
    }
    match descriptor.watch {
        WatchPlan::Static => {
            tracing::info!(importer = %descriptor.id, "importer is static; change driver disabled");
        }
        WatchPlan::Filesystem { root } => {
            let obsidian_conventions = descriptor.id == "obsidian";
            let markdown_only = obsidian_conventions;
            spawn_filesystem(
                state,
                root,
                markdown_only,
                obsidian_conventions,
                filesystem_rescan_seconds,
            );
        }
        WatchPlan::Poll { interval_ms } => spawn_poll(state, interval_ms),
        WatchPlan::Push => {
            state.inner.progress.warn(
                "watch",
                format!(
                    "{} requests push changes, but push streams are not wired yet",
                    descriptor.id
                ),
            );
        }
    }
}

fn watch_is_authorized(importer: &HostedImporter, descriptor: &ImporterDescriptor) -> bool {
    match &descriptor.watch {
        WatchPlan::Static => true,
        WatchPlan::Filesystem { root } => {
            let capability = data_loader::Capability::new(
                Effect::Watch,
                Transport::Filesystem,
                root.to_string_lossy().into_owned(),
            );
            descriptor.capabilities.contains(&capability) && importer.is_authorized(&capability)
        }
        WatchPlan::Poll { .. } | WatchPlan::Push => {
            let watches = descriptor
                .capabilities
                .iter()
                .filter(|capability| capability.effect == Effect::Watch)
                .collect::<Vec<_>>();
            !watches.is_empty()
                && watches
                    .into_iter()
                    .all(|capability| importer.is_authorized(capability))
        }
    }
}

fn spawn_poll(state: AppState, interval_ms: u64) {
    let interval_ms = interval_ms.max(100);
    let progress = state.inner.progress.clone();
    progress.info("watch", format!("polling importer every {interval_ms} ms"));
    tokio::spawn(async move {
        let mut interval = polling_interval(interval_ms);
        interval.tick().await;
        loop {
            interval.tick().await;
            reload(&state).await;
        }
    });
}

fn polling_interval(interval_ms: u64) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}

fn filesystem_rescan_interval(seconds: u64) -> Option<tokio::time::Interval> {
    if seconds == 0 {
        return None;
    }

    let period = Duration::from_secs(seconds);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    Some(interval)
}

async fn next_filesystem_rescan(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn reset_filesystem_rescan(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval {
        interval.reset();
    }
}

fn spawn_filesystem(
    state: AppState,
    vault_root: PathBuf,
    markdown_only: bool,
    obsidian_conventions: bool,
    filesystem_rescan_seconds: u64,
) {
    let progress = state.inner.progress.clone();

    // Reload signal channel. The notify callback runs on the notify
    // worker thread (sync); we hop into the tokio runtime via a
    // bounded mpsc. Each message is the set of relevant vault-relative
    // `.md` paths that changed in this debounce window. A second burst
    // arriving while the reload task is busy gets unioned in via the
    // `try_recv` drain at the top of the reload loop so no edits are
    // dropped.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<HashSet<String>>(8);

    // The debouncer's handler runs on notify's worker thread. Keep it
    // tiny: filter, then try_send (drop if the channel already has a
    // pending signal — the reload task will pick up everything that
    // arrived in the meantime when it actually runs).
    let watch_root = vault_root.clone();
    let tx_evt = tx.clone();
    let progress_evt = progress.clone();
    let debouncer_res = new_debouncer(
        Duration::from_millis(400),
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
            let events = match res {
                Ok(e) => e,
                Err(e) => {
                    progress_evt.warn("watch", format!("watcher error: {e}"));
                    return;
                }
            };
            let mut paths: HashSet<String> = HashSet::new();
            for e in &events {
                if !is_relevant(&watch_root, &e.path, markdown_only, obsidian_conventions) {
                    continue;
                }
                if let Some(rel) = relative_path(&watch_root, &e.path) {
                    paths.insert(rel);
                }
            }
            if !paths.is_empty() {
                let _ = tx_evt.try_send(paths);
            }
        },
    );

    let mut debouncer = match debouncer_res {
        Ok(debouncer) => Some(debouncer),
        Err(e) => {
            progress.warn("watch", format!("failed to start filesystem watcher: {e}"));
            tracing::warn!("filesystem watcher start failed: {e}");
            None
        }
    };

    if let Some(active) = &mut debouncer {
        if let Err(e) = active
            .watcher()
            .watch(&vault_root, RecursiveMode::Recursive)
        {
            progress.warn("watch", format!("watch({}): {e}", vault_root.display()));
            tracing::warn!(path = %vault_root.display(), "filesystem watch failed: {e}");
            debouncer = None;
        }
    }

    let notifications_enabled = debouncer.is_some();
    if notifications_enabled {
        progress.info(
            "watch",
            format!("watching importer source: {}", vault_root.display()),
        );
    } else if filesystem_rescan_seconds == 0 {
        progress.error(
            "watch",
            "filesystem notifications unavailable and periodic rescan disabled",
        );
        return;
    } else {
        progress.warn(
            "watch",
            "filesystem notifications unavailable; using periodic rescan only",
        );
    }
    if filesystem_rescan_seconds == 0 {
        progress.info("watch", "periodic filesystem rescan disabled");
    } else {
        progress.info(
            "watch",
            format!("full filesystem rescan every {filesystem_rescan_seconds} seconds"),
        );
    }

    drop(tx);

    // Move the optional debouncer into the background task so it lives as long
    // as the task does (dropping it stops the watch). Notification-driven and
    // timer-driven reloads share this task, so they cannot run concurrently.
    tokio::spawn(async move {
        let _debouncer = debouncer; // keep alive
        let mut notifications_enabled = notifications_enabled;
        let mut rescan_interval = filesystem_rescan_interval(filesystem_rescan_seconds);
        loop {
            tokio::select! {
                maybe_paths = rx.recv(), if notifications_enabled => {
                    let Some(mut paths) = maybe_paths else {
                        notifications_enabled = false;
                        if rescan_interval.is_none() {
                            break;
                        }
                        continue;
                    };

                    // Drain any additional pending batches that arrived while
                    // we were blocked. try_recv loop coalesces a burst of
                    // edits into a single reload, unioning their path sets so
                    // nothing is lost.
                    while let Ok(more) = rx.try_recv() {
                        paths.extend(more);
                    }

                    reload_with_paths(&state, &paths).await;
                    // Always wait one complete quiet period after a reload;
                    // an already-due timer must not trigger back-to-back work.
                    reset_filesystem_rescan(&mut rescan_interval);
                }
                () = next_filesystem_rescan(&mut rescan_interval) => {
                    tracing::debug!(
                        seconds = filesystem_rescan_seconds,
                        "running periodic filesystem rescan"
                    );
                    reload(&state).await;
                    reset_filesystem_rescan(&mut rescan_interval);
                }
            }
        }
    });
}

/// Run one reload with a known set of changed paths. The current vertical
/// slice still rebuilds a complete graph/search/facet snapshot; retaining the
/// path set makes the future delta boundary explicit without a second index.
pub async fn reload_with_paths(state: &AppState, _paths: &HashSet<String>) {
    rebuild_snapshot(state).await;
}

/// Run one full graph/search/facet reload.
pub async fn reload(state: &AppState) {
    rebuild_snapshot(state).await;
}

/// Reload graph, search index, and facets, then atomically swap them together.
async fn rebuild_snapshot(state: &AppState) -> bool {
    let progress = state.inner.progress.clone();

    let descriptor = state.inner.importer.descriptor();
    let reload_id = progress.start("ingest", format!("Reloading {}", descriptor.name));
    progress.info("ingest", format!("{} change detected", descriptor.id));

    let loaded =
        match crate::vault_loader::load_with_progress(&state.inner.importer, Some(&progress)).await
        {
            Ok(loaded) => loaded,
            Err(error) => {
                progress.fail(reload_id, error.to_string());
                return false;
            }
        };

    let snap_id = progress.start("ingest", "Building snapshot");
    let schema = descriptor.schema;
    let source = SnapshotSource::new(descriptor.id, descriptor.name, descriptor.version);
    let snapshot = tokio::task::spawn_blocking(move || {
        GraphSnapshot::build(loaded.graph, source, schema, loaded.search_documents)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|snapshot| snapshot.map(Arc::new).map_err(|error| error.to_string()));
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(e) => {
            progress.fail(snap_id, format!("snapshot build: {e}"));
            progress.fail(reload_id, "snapshot build failed");
            return false;
        }
    };
    progress.finish(snap_id);

    state.inner.snapshot.store(snapshot);
    progress.finish(reload_id);

    // Keep the compute worker simulating THIS graph (no-op when the broker
    // is disabled or disconnected).
    crate::server::push_graph_to_worker(state).await;
    true
}

/// Convert an absolute event path to a stable vault-relative path.
fn relative_path(vault_root: &Path, path: &Path) -> Option<String> {
    if vault_root == path {
        return path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    let rel = path.strip_prefix(vault_root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Filter source events without applying Obsidian conventions to generic
/// filesystem importers.
fn is_relevant(
    vault_root: &Path,
    path: &Path,
    markdown_only: bool,
    obsidian_conventions: bool,
) -> bool {
    // Strip the vault root prefix for component inspection so we don't
    // false-trigger on something like `/home/.config/...`.
    let rel: PathBuf = path
        .strip_prefix(vault_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf());

    for comp in rel.components() {
        let s = comp.as_os_str().to_string_lossy();
        if obsidian_conventions
            && (s == ".git"
                || s == "node_modules"
                || s == ".obsidian"
                || (s.starts_with('.') && s != "." && s != ".."))
        {
            return false;
        }
    }

    !markdown_only || matches!(path.extension().and_then(|s| s.to_str()), Some("md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_loader::{
        DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, ImportError, ImportFuture, Importer,
        ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument,
    };
    use vault_data::{VaultGraph, VaultNode};

    fn test_schema() -> ImporterSchema {
        ImporterSchema::new(
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(4),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true).searchable(2),
            ],
            vec![EdgeTypeSchema::directed("reference", "test edge")],
        )
    }

    fn test_result(mut graph: VaultGraph) -> LoadResult {
        let search_documents = graph
            .nodes
            .values_mut()
            .map(|node| {
                if node.meta.source_id.is_empty() {
                    node.meta.source_id = "test".into();
                }
                if node.meta.title.is_empty() {
                    node.meta.title = node.id.clone();
                }
                SearchDocument::new(&node.id)
                    .with("id", node.id.clone())
                    .with("title", node.meta.title.clone())
                    .with("tags", serde_json::json!(node.meta.tags))
            })
            .collect();
        LoadResult {
            graph,
            search_documents,
            unresolved: Vec::new(),
        }
    }

    struct FailingImporter;

    impl Importer for FailingImporter {
        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new(
                "failing",
                "Failing",
                "1",
                vec![data_loader::Capability::new(
                    Effect::Read,
                    Transport::InMemory,
                    "failing",
                )],
                test_schema(),
            )
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
            Box::pin(async {
                Err(ImportError::SourceRead {
                    origin: "test".into(),
                    message: "expected failure".into(),
                })
            })
        }
    }

    struct DeclaredButUngrantedWatch;

    impl Importer for DeclaredButUngrantedWatch {
        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new(
                "ungranted-watch",
                "Ungranted watch",
                "1",
                vec![
                    data_loader::Capability::new(
                        Effect::Read,
                        Transport::Kubernetes,
                        "cluster-a/apps/deployments:default",
                    ),
                    data_loader::Capability::new(
                        Effect::Watch,
                        Transport::Kubernetes,
                        "cluster-a/apps/deployments:default",
                    ),
                ],
                test_schema(),
            )
            .with_watch(WatchPlan::Poll { interval_ms: 100 })
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
            Box::pin(async {
                Ok(LoadResult {
                    graph: VaultGraph::new(),
                    search_documents: Vec::new(),
                    unresolved: Vec::new(),
                })
            })
        }
    }

    #[test]
    fn declared_but_ungranted_watch_cannot_start_change_driver() {
        let raw = DeclaredButUngrantedWatch;
        let descriptor = raw.descriptor();
        let importer = HostedImporter::new(
            Box::new(raw),
            descriptor
                .capabilities
                .iter()
                .filter(|capability| capability.effect == Effect::Read)
                .cloned(),
        )
        .unwrap();

        assert!(!watch_is_authorized(&importer, &descriptor));
    }

    #[tokio::test]
    async fn polling_skips_missed_ticks_instead_of_bursting_reloads() {
        assert_eq!(
            polling_interval(100).missed_tick_behavior(),
            tokio::time::MissedTickBehavior::Skip
        );
    }

    #[tokio::test]
    async fn filesystem_rescan_can_be_disabled() {
        assert!(filesystem_rescan_interval(0).is_none());
    }

    #[tokio::test]
    async fn filesystem_rescan_waits_before_first_tick_and_skips_missed_ticks() {
        let mut interval = filesystem_rescan_interval(1).unwrap();
        assert_eq!(
            interval.missed_tick_behavior(),
            tokio::time::MissedTickBehavior::Skip
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), interval.tick())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_reload_keeps_last_good_snapshot() {
        let mut graph = VaultGraph::new();
        graph.add_node(VaultNode {
            id: "last-good".into(),
            meta: vault_data::NodeMeta {
                source_id: "test".into(),
                title: "Last good".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let raw: Box<dyn Importer> = Box::new(FailingImporter);
        let grants = raw.descriptor().capabilities;
        let importer = HostedImporter::new(raw, grants).unwrap();
        let state = crate::AppState::new(
            PathBuf::new(),
            importer,
            test_result(graph),
            None,
            crate::compute_broker::ComputeBroker::new(),
            Arc::new(crate::progress::ProgressLog::new()),
        )
        .unwrap();
        let revision = state.snapshot().revision;

        assert!(!rebuild_snapshot(&state).await);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.graph.nodes.contains_key("last-good"));
    }

    #[test]
    fn filter_accepts_markdown() {
        let root = Path::new("/v");
        assert!(is_relevant(root, Path::new("/v/notes/foo.md"), true, true));
        assert!(is_relevant(root, Path::new("/v/foo.md"), true, true));
    }

    #[test]
    fn filter_rejects_non_markdown() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/foo.txt"), true, true));
        assert!(!is_relevant(
            root,
            Path::new("/v/notes/img.png"),
            true,
            true
        ));
        assert!(is_relevant(root, Path::new("/v/foo.txt"), false, false));
    }

    #[test]
    fn filter_rejects_dotdirs() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/.git/HEAD.md"), true, true));
        assert!(!is_relevant(
            root,
            Path::new("/v/.obsidian/cache/x.md"),
            true,
            true
        ));
        assert!(!is_relevant(
            root,
            Path::new("/v/node_modules/x.md"),
            true,
            true
        ));
        assert!(!is_relevant(root, Path::new("/v/.hidden/x.md"), true, true));
        assert!(is_relevant(root, Path::new("/v/.hidden/x.md"), true, false));
        assert!(is_relevant(
            root,
            Path::new("/v/.git/concept.md"),
            true,
            false
        ));
    }
}
