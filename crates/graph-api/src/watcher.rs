//! Filesystem watcher that drives live vault reloads.
//!
//! Wires `notify-debouncer-mini` to a tokio mpsc channel. When `.md`
//! files under `$VAULT_ROOT` change, the watcher coalesces a burst of
//! events into a single reload, then:
//!
//!   1. Re-runs `vault_loader::load_with_progress` (emits "Scanning
//!      vault", "Computing graph metrics", "Seeding layout positions"
//!      task bars to `ProgressLog`).
//!   2. Builds a fresh `GraphSnapshot` and `ArcSwap`s it into the
//!      `AppState`. In-flight readers keep the previous `Arc` valid.
//!   3. Drops the old `vault-search` subprocess and respawns it with
//!      `--rebuild` so BM25 search returns up-to-date hits. (See the
//!      GUESS note in `subprocess.rs::spawn_rebuild` — no in-place
//!      refresh API exists today.)
//!
//! ## Filter
//!
//! Events for paths whose components contain `.git`, `node_modules`,
//! `.obsidian`, or a leading-dot dotfile are ignored, as are
//! non-`.md` files. This avoids reloading on every git index update.
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

use crate::state::{AppState, GraphSnapshot};

/// Spawn the filesystem watcher + reload task. Returns immediately; the
/// watcher and reload loop run for the lifetime of the process.
///
/// `state` is the live `AppState`; the watcher swaps new snapshots into
/// it under `state.inner.snapshot`.
pub fn spawn(state: AppState) {
    let vault_root = state.inner.vault_root.clone();
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
                if !is_relevant(&watch_root, &e.path) {
                    continue;
                }
                if let Some(rel) = relative_md_path(&watch_root, &e.path) {
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
        format!("watching vault: {}", vault_root.display()),
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
    rebuild_snapshot(state).await;

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
                    format!("search refresh: {updated} upserted, {deleted} deleted, {skipped} skipped"),
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
    rebuild_snapshot(state).await;
    respawn_search(state).await;
}

/// Reload the in-memory `GraphSnapshot` from disk and atomically swap it
/// into `state`. Does NOT touch vault-search.
async fn rebuild_snapshot(state: &AppState) {
    let vault_root = state.inner.vault_root.clone();
    let progress = state.inner.progress.clone();

    let reload_id = progress.start("ingest", "Reloading vault");
    progress.info("ingest", "vault change detected");

    let progress_load = progress.clone();
    let new_graph = tokio::task::spawn_blocking(move || {
        crate::vault_loader::load_with_progress(&vault_root, Some(&progress_load))
    })
    .await;

    let new_graph = match new_graph {
        Ok(g) => g,
        Err(e) => {
            progress.fail(reload_id, format!("reload panic: {e}"));
            return;
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
            return;
        }
    };
    progress.finish(snap_id);

    state.inner.snapshot.store(snapshot);
    progress.finish(reload_id);
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
fn relative_md_path(vault_root: &Path, path: &Path) -> Option<String> {
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
fn is_relevant(vault_root: &Path, path: &Path) -> bool {
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

    matches!(path.extension().and_then(|s| s.to_str()), Some("md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_accepts_markdown() {
        let root = Path::new("/v");
        assert!(is_relevant(root, Path::new("/v/notes/foo.md")));
        assert!(is_relevant(root, Path::new("/v/foo.md")));
    }

    #[test]
    fn filter_rejects_non_markdown() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/foo.txt")));
        assert!(!is_relevant(root, Path::new("/v/notes/img.png")));
    }

    #[test]
    fn filter_rejects_dotdirs() {
        let root = Path::new("/v");
        assert!(!is_relevant(root, Path::new("/v/.git/HEAD.md")));
        assert!(!is_relevant(root, Path::new("/v/.obsidian/cache/x.md")));
        assert!(!is_relevant(root, Path::new("/v/node_modules/x.md")));
    }
}
