//! Change drivers that reload the active importer.
//!
//! Filesystem importers use `notify-debouncer-mini`; polling importers use a
//! Tokio interval; static importers install no change driver. Filesystem bursts
//! are coalesced into one reload, then:
//!
//!   1. Re-runs `vault_loader::load_with_progress` (emits "Scanning
//!      vault", "Computing graph metrics", "Seeding layout positions"
//!      task bars to `ProgressLog`).
//!   2. Builds a fresh `GraphSnapshot` and `ArcSwap`s it into the
//!      `AppState`. In-flight readers keep the previous `Arc` valid.
//!   3. For Obsidian only, refreshes or respawns the `vault-search` subprocess
//!      `--rebuild` so BM25 search returns up-to-date hits. (See the
//!      GUESS note in `subprocess.rs::spawn_rebuild` — no in-place
//!      refresh API exists today.)
//!
//! ## Filter
//!
//! Events for paths whose components contain `.git`, `node_modules`,
//! `.obsidian`, or a leading-dot dotfile are ignored. Obsidian additionally
//! accepts only `.md`; other filesystem importers react to their bound input.
//!
//! ## Container caveats
//!
//! On Linux, inotify events for a bind-mounted directory only fire when
//! the *guest* container's kernel sees the write. Edits made on the
//! host fs are propagated through the OCI bind mount on most engines
//! (podman, docker on Linux). On macOS Docker Desktop / Lima, fs
//! events are heavily debounced by the virtualization layer — a 1-2s
//! lag is normal.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use data_loader::{Effect, HostedImporter, ImporterDescriptor, Transport, WatchPlan};

use crate::state::{AppState, GraphSnapshot};

/// Spawn the filesystem watcher + reload task. Returns immediately; the
/// watcher and reload loop run for the lifetime of the process.
///
/// `state` is the live `AppState`; the watcher swaps new snapshots into
/// it under `state.inner.snapshot`.
pub fn spawn(state: AppState) {
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
            let markdown_only = descriptor.id == "obsidian";
            spawn_filesystem(state, root, markdown_only);
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

fn spawn_filesystem(state: AppState, vault_root: PathBuf, markdown_only: bool) {
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
                if !is_relevant(&watch_root, &e.path, markdown_only) {
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
        Ok(d) => d,
        Err(e) => {
            progress.error("watch", format!("failed to start watcher: {e}"));
            tracing::error!("watcher start failed: {e}");
            return;
        }
    };

    if let Err(e) = debouncer
        .watcher()
        .watch(&vault_root, RecursiveMode::Recursive)
    {
        progress.error("watch", format!("watch({}): {e}", vault_root.display()));
        tracing::error!(path = %vault_root.display(), "watch failed: {e}");
        return;
    }

    progress.info(
        "watch",
        format!("watching importer source: {}", vault_root.display()),
    );

    // Move the debouncer into the background task so it lives as long
    // as the task does (dropping it stops the watch).
    tokio::spawn(async move {
        let _debouncer = debouncer; // keep alive
        while let Some(mut paths) = rx.recv().await {
            // Drain any additional pending batches that arrived while
            // we were blocked. try_recv loop coalesces a burst of
            // edits into a single reload, unioning their path sets so
            // nothing is lost.
            while let Ok(more) = rx.try_recv() {
                paths.extend(more);
            }

            reload_with_paths(&state, &paths).await;
        }
    });
}

/// Run one reload with a known set of changed `.md` paths (vault-relative).
/// Rebuilds the graph snapshot, then attempts an incremental refresh of the
/// vault-search index against just those paths. If refresh fails (no child
/// running, HTTP error, lock poisoned, etc.) we fall back to a full
/// `spawn_rebuild`, logging a warning so regressions are visible.
pub async fn reload_with_paths(state: &AppState, paths: &HashSet<String>) {
    if !rebuild_snapshot(state).await {
        return;
    }

    if state.inner.importer.descriptor().id != "obsidian" {
        return;
    }

    let progress = state.inner.progress.clone();
    let path_vec: Vec<String> = paths.iter().cloned().collect();
    let n = path_vec.len();

    // Try incremental refresh first. We hold the current `Arc<VaultSearch>`
    // across the await — the worst case is we POST to a child that's
    // already been swapped out, the request fails, and we fall back.
    let current = state.inner.vault_search.load();
    if let Some(vs) = current.as_ref() {
        let refresh_id = progress.start("ingest", format!("Refreshing search index ({n})"));
        match vs.refresh(&path_vec).await {
            Ok((updated, deleted, skipped)) => {
                progress.info(
                    "ingest",
                    format!(
                        "search refresh: {updated} upserted, {deleted} deleted, {skipped} skipped"
                    ),
                );
                progress.finish(refresh_id);
                return;
            }
            Err(e) => {
                progress.warn(
                    "ingest",
                    format!("incremental refresh failed, falling back to rebuild: {e}"),
                );
                progress.fail(refresh_id, "refresh failed");
                tracing::warn!(error = %e, "vault-search refresh failed; respawning");
            }
        }
    }

    // Fallback: full respawn with --rebuild.
    respawn_search(state).await;
}

/// Run one full reload: rebuild snapshot + respawn vault-search. Used at
/// startup-style code paths where there's no known change set.
pub async fn reload(state: &AppState) {
    if !rebuild_snapshot(state).await {
        return;
    }
    if state.inner.importer.descriptor().id == "obsidian" {
        respawn_search(state).await;
    }
}

/// Reload the in-memory `GraphSnapshot` from disk and atomically swap it
/// into `state`. Does NOT touch vault-search.
async fn rebuild_snapshot(state: &AppState) -> bool {
    let progress = state.inner.progress.clone();

    let descriptor = state.inner.importer.descriptor();
    let reload_id = progress.start("ingest", format!("Reloading {}", descriptor.name));
    progress.info("ingest", format!("{} change detected", descriptor.id));

    let new_graph =
        match crate::vault_loader::load_with_progress(&state.inner.importer, Some(&progress)).await
        {
            Ok(graph) => graph,
            Err(error) => {
                progress.fail(reload_id, error.to_string());
                return false;
            }
        };

    let snap_id = progress.start("ingest", "Building snapshot");
    let snapshot = tokio::task::spawn_blocking(move || GraphSnapshot::build(new_graph))
        .await
        .map(Arc::new);
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(e) => {
            progress.fail(snap_id, format!("snapshot panic: {e}"));
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

/// Full respawn of the vault-search subprocess with `--rebuild`. Used as
/// the fallback when incremental refresh fails or at startup-style paths.
async fn respawn_search(state: &AppState) {
    let progress = state.inner.progress.clone();
    let search_id = progress.start("ingest", "Rebuilding search index");
    let vault_root = state.inner.vault_root.clone();
    match crate::subprocess::VaultSearch::spawn_rebuild(&vault_root).await {
        Ok(vs) => {
            state.inner.vault_search.store(Some(Arc::new(vs)));
            progress.finish(search_id);
        }
        Err(e) => {
            progress.fail(search_id, format!("vault-search respawn: {e}"));
        }
    }
}

/// Convert an absolute event path to the vault-relative form vault-search
/// expects (forward slashes, with `.md` extension preserved). Returns
/// `None` if the path can't be made relative.
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

/// Filter: only `.md` files under `vault_root` whose path doesn't
/// traverse a hidden / ignored directory.
fn is_relevant(vault_root: &Path, path: &Path, markdown_only: bool) -> bool {
    // Strip the vault root prefix for component inspection so we don't
    // false-trigger on something like `/home/.config/...`.
    let rel: PathBuf = path
        .strip_prefix(vault_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf());

    for comp in rel.components() {
        let s = comp.as_os_str().to_string_lossy();
        if s == ".git"
            || s == "node_modules"
            || s == ".obsidian"
            || (s.starts_with('.') && s != "." && s != "..")
        {
            return false;
        }
    }

    !markdown_only || matches!(path.extension().and_then(|s| s.to_str()), Some("md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_loader::{ImportError, ImportFuture, Importer, ImporterDescriptor, LoadResult};
    use vault_data::{VaultGraph, VaultNode};

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
            )
            .with_watch(WatchPlan::Poll { interval_ms: 100 })
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
            Box::pin(async {
                Ok(LoadResult {
                    graph: VaultGraph::new(),
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
    async fn failed_reload_keeps_last_good_snapshot() {
        let mut graph = VaultGraph::new();
        graph.add_node(VaultNode {
            id: "last-good".into(),
            ..Default::default()
        });
        let raw: Box<dyn Importer> = Box::new(FailingImporter);
        let grants = raw.descriptor().capabilities;
        let importer = HostedImporter::new(raw, grants).unwrap();
        let state = crate::AppState::new(
            PathBuf::new(),
            importer,
            graph,
            None,
            None,
            crate::compute_broker::ComputeBroker::new(),
            Arc::new(crate::progress::ProgressLog::new()),
        );
        assert!(!rebuild_snapshot(&state).await);
        let snapshot = state.snapshot();
        assert!(snapshot.graph.nodes.contains_key("last-good"));
    }

    #[test]
    fn filter_accepts_markdown() {
        let root = Path::new("/v");
        assert!(is_relevant(root, Path::new("/v/notes/foo.md"), true));
        assert!(is_relevant(root, Path::new("/v/foo.md"), true));
    }

    #[test]
    fn filter_rejects_non_markdown() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/foo.txt"), true));
        assert!(!is_relevant(root, Path::new("/v/notes/img.png"), true));
        assert!(is_relevant(root, Path::new("/v/foo.txt"), false));
    }

    #[test]
    fn filter_rejects_dotdirs() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/.git/HEAD.md"), true));
        assert!(!is_relevant(
            root,
            Path::new("/v/.obsidian/cache/x.md"),
            true
        ));
        assert!(!is_relevant(root, Path::new("/v/node_modules/x.md"), true));
    }
}
