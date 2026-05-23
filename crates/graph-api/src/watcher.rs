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
    // bounded mpsc. `capacity = 1` because all we need is "dirty"
    // signaling — bursts of events still collapse to a single reload.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

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
            let any_md = events.iter().any(|e| is_relevant(&watch_root, &e.path));
            if any_md {
                let _ = tx_evt.try_send(());
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
        while rx.recv().await.is_some() {
            // Drain any additional pending signals that arrived while
            // we were blocked. try_recv loop coalesces a burst of
            // edits into a single reload.
            while rx.try_recv().is_ok() {}

            reload(&state).await;
        }
    });
}

/// Run one full reload: rebuild snapshot + respawn vault-search.
pub async fn reload(state: &AppState) {
    let vault_root = state.inner.vault_root.clone();
    let progress = state.inner.progress.clone();

    let reload_id = progress.start("ingest", "Reloading vault");
    progress.info("ingest", "vault change detected");

    // Heavy work — block on a worker thread so we don't stall the
    // tokio runtime.
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

    // Respawn vault-search against the new vault state. We can't keep
    // the old child running (its index is stale) and vault-search has
    // no in-place refresh hook today.
    let search_id = progress.start("ingest", "Rebuilding search index");
    let vault_root2 = state.inner.vault_root.clone();
    match crate::subprocess::VaultSearch::spawn_rebuild(&vault_root2).await {
        Ok(vs) => {
            state.inner.vault_search.store(Some(Arc::new(vs)));
            progress.finish(search_id);
        }
        Err(e) => {
            progress.fail(search_id, format!("vault-search respawn: {e}"));
        }
    }

    progress.finish(reload_id);
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
