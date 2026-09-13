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
//! ## Alternate sources
//!
//! A lazily built alternate ([`crate::source_host`]) gets its driver from
//! [`spawn_gated`]: its periodic rebuilds run only when a request resolved to
//! that source since the previous tick, and never overlap each other. The
//! deployment default is ungated.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use data_loader::{Effect, HostedImporter, ImporterDescriptor, Transport, WatchPlan};

use crate::state::{AppState, GraphSnapshot, SnapshotSource};

/// Gate on a lazily built alternate source's periodic rescan. `SourceHost`
/// marks the gate used whenever a request resolves to that alternate; the
/// alternate's watcher consults it on every tick and skips the rebuild when
/// nothing asked for the source since the previous tick. Rebuilding a large
/// corpus (graph + metrics + search index) on a timer for a source nobody is
/// reading starves the requests that source exists to serve.
///
/// The deployment default has no gate: its change driver is unchanged.
#[derive(Debug, Default)]
pub struct RescanGate {
    requested: AtomicBool,
    rebuilding: AtomicBool,
}

/// Outcome of consulting a [`RescanGate`] on one periodic tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanDecision {
    /// Rebuild now; the caller holds this source's rebuild slot until
    /// [`RescanGate::finish`].
    Run,
    /// Nothing requested this source since the previous tick.
    SkipIdle,
    /// A rebuild for this source is still running. Ticks are dropped, never
    /// queued, so a rebuild slower than the interval cannot pile up.
    SkipInFlight,
}

impl RescanGate {
    /// Record a request resolving to this source.
    pub fn mark_used(&self) {
        self.requested.store(true, Ordering::Release);
    }

    /// Decide one periodic tick, claiming the rebuild slot on
    /// [`RescanDecision::Run`]. The requested flag survives `SkipInFlight`,
    /// so the tick after the in-flight rebuild still runs.
    pub(crate) fn begin_periodic(&self) -> RescanDecision {
        if self
            .rebuilding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return RescanDecision::SkipInFlight;
        }
        if !self.requested.swap(false, Ordering::AcqRel) {
            self.rebuilding.store(false, Ordering::Release);
            return RescanDecision::SkipIdle;
        }
        RescanDecision::Run
    }

    /// Claim the rebuild slot for a change-driven reload. A real source
    /// change must always land, so this takes the slot unconditionally; its
    /// only effect is suppressing a periodic tick that races it.
    fn begin_change(&self) {
        self.rebuilding.store(true, Ordering::Release);
    }

    /// Release the rebuild slot.
    fn finish(&self) {
        self.rebuilding.store(false, Ordering::Release);
    }
}

/// Ungated (default source) watchers always run their tick.
fn periodic_decision(gate: Option<&RescanGate>) -> RescanDecision {
    gate.map_or(RescanDecision::Run, RescanGate::begin_periodic)
}

/// One periodic rebuild, subject to the source's gate.
async fn periodic_reload(state: &AppState, gate: Option<&RescanGate>, driver: &'static str) {
    match periodic_decision(gate) {
        RescanDecision::Run => {
            reload(state).await;
            if let Some(gate) = gate {
                gate.finish();
            }
        }
        decision => {
            tracing::debug!(driver, ?decision, "skipped periodic rebuild");
        }
    }
}

/// One change-driven rebuild, holding the source's rebuild slot so a racing
/// periodic tick is skipped instead of doubling the work.
async fn change_reload(state: &AppState, gate: Option<&RescanGate>, paths: &HashSet<String>) {
    if let Some(gate) = gate {
        gate.begin_change();
    }
    reload_with_paths(state, paths).await;
    if let Some(gate) = gate {
        gate.finish();
    }
}

/// Spawn the change driver (filesystem watcher, poll loop, or push trigger)
/// for the deployment default source. Returns immediately; the driver runs
/// for the lifetime of the process, unless the caller aborts the returned
/// handle.
///
/// `state` is the live `AppState`; the driver swaps new snapshots into it
/// under `state.inner.snapshot`.
pub fn spawn(
    state: AppState,
    filesystem_rescan_seconds: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    spawn_with_gate(state, filesystem_rescan_seconds, None)
}

/// [`spawn`] for a lazily built alternate source: periodic rescans are gated
/// on `gate` (see [`RescanGate`]), change-driven reloads are not. The caller
/// aborts the returned handle when the alternate is evicted.
pub fn spawn_gated(
    state: AppState,
    filesystem_rescan_seconds: u64,
    gate: Arc<RescanGate>,
) -> Option<tokio::task::JoinHandle<()>> {
    spawn_with_gate(state, filesystem_rescan_seconds, Some(gate))
}

fn spawn_with_gate(
    state: AppState,
    filesystem_rescan_seconds: u64,
    gate: Option<Arc<RescanGate>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let descriptor = state.inner.importer.descriptor();
    if !watch_is_authorized(&state.inner.importer, &descriptor) {
        let message = format!(
            "{} declares a change driver without an exact watch grant; change driver disabled",
            descriptor.id
        );
        state.inner.progress.warn("watch", &message);
        tracing::warn!(importer = %descriptor.id, "{message}");
        return None;
    }
    match descriptor.watch {
        WatchPlan::Static => {
            tracing::info!(importer = %descriptor.id, "importer is static; change driver disabled");
            None
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
                gate,
            )
        }
        WatchPlan::Poll { interval_ms } => Some(spawn_poll(state, interval_ms, gate)),
        WatchPlan::Push => spawn_push(state),
    }
}

/// Push-driven reload: each tick on the trigger receiver wired via
/// [`AppState::with_push_trigger`] runs one full snapshot rebuild. Without a
/// wired trigger the driver stays disabled, matching the historical
/// warn-and-continue behavior for push sources.
fn spawn_push(state: AppState) -> Option<tokio::task::JoinHandle<()>> {
    let Some(mut trigger) = state.inner.push_trigger.clone() else {
        let message = format!(
            "{} requests push changes, but no push trigger is wired; change driver disabled",
            state.inner.importer.descriptor().id
        );
        state.inner.progress.warn("watch", &message);
        tracing::warn!(importer = %state.inner.importer.descriptor().id, "{message}");
        return None;
    };
    state.inner.progress.info(
        "watch",
        format!(
            "{} reloads on push trigger",
            state.inner.importer.descriptor().id
        ),
    );
    Some(tokio::spawn(async move {
        while trigger.changed().await.is_ok() {
            reload(&state).await;
        }
    }))
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

fn spawn_poll(
    state: AppState,
    interval_ms: u64,
    gate: Option<Arc<RescanGate>>,
) -> tokio::task::JoinHandle<()> {
    let interval_ms = interval_ms.max(100);
    let progress = state.inner.progress.clone();
    progress.info("watch", format!("polling importer every {interval_ms} ms"));
    tokio::spawn(async move {
        let mut interval = polling_interval(interval_ms);
        interval.tick().await;
        loop {
            interval.tick().await;
            periodic_reload(&state, gate.as_deref(), "poll").await;
        }
    })
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
    gate: Option<Arc<RescanGate>>,
) -> Option<tokio::task::JoinHandle<()>> {
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
        return None;
    } else {
        progress.warn(
            "watch",
            "filesystem notifications unavailable; using periodic rescan only",
        );
    }
    if filesystem_rescan_seconds == 0 {
        progress.info("watch", "periodic filesystem rescan disabled");
    } else if gate.is_some() {
        progress.info(
            "watch",
            format!(
                "full filesystem rescan every {filesystem_rescan_seconds} seconds \
                 while this source is being requested"
            ),
        );
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
    Some(tokio::spawn(async move {
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

                    change_reload(&state, gate.as_deref(), &paths).await;
                    // Always wait one complete quiet period after a reload;
                    // an already-due timer must not trigger back-to-back work.
                    reset_filesystem_rescan(&mut rescan_interval);
                }
                () = next_filesystem_rescan(&mut rescan_interval) => {
                    periodic_reload(&state, gate.as_deref(), "filesystem-rescan").await;
                    reset_filesystem_rescan(&mut rescan_interval);
                }
            }
        }
    }))
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
/// An [`data_loader::ImportOutcome::Unchanged`] response keeps the current
/// snapshot silently — no progress events, no rebuild, no swap.
async fn rebuild_snapshot(state: &AppState) -> bool {
    let progress = state.inner.progress.clone();
    let descriptor = state.inner.importer.descriptor();

    let loaded =
        match crate::vault_loader::load_with_progress(&state.inner.importer, Some(&progress)).await
        {
            Ok(data_loader::ImportOutcome::Unchanged) => {
                tracing::debug!(
                    source = %descriptor.id,
                    "source unchanged; keeping snapshot"
                );
                return false;
            }
            Ok(data_loader::ImportOutcome::Loaded(loaded)) => loaded,
            Err(error) => {
                // load_with_progress already failed its scan stage; the
                // reload stage below only opens once fresh data exists.
                tracing::warn!(
                    source = %descriptor.id,
                    %error,
                    "reload failed; keeping last snapshot"
                );
                return false;
            }
        };

    // Only now is a reload actually happening: the source produced fresh
    // bytes and a replacement snapshot is imminent.
    let reload_id = progress.start("ingest", format!("Reloading {}", descriptor.name));
    progress.info("ingest", format!("{} change detected", descriptor.id));

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
        ImportOutcome, ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument,
        TagHierarchySchema,
    };
    use vault_data::{VaultGraph, VaultNode};

    fn test_schema() -> ImporterSchema {
        ImporterSchema::new(
            "generate",
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(4),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(2)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed("reference", "test edge")],
            TagHierarchySchema::slash(),
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

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<ImportOutcome, ImportError>> {
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

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<ImportOutcome, ImportError>> {
            Box::pin(async {
                Ok(ImportOutcome::Loaded(LoadResult {
                    graph: VaultGraph::new(),
                    search_documents: Vec::new(),
                    unresolved: Vec::new(),
                }))
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

    /// The deployment default has no gate; every tick must still rescan.
    #[test]
    fn ungated_source_rescans_on_every_tick() {
        assert_eq!(periodic_decision(None), RescanDecision::Run);
        assert_eq!(periodic_decision(None), RescanDecision::Run);
    }

    #[test]
    fn idle_alternate_tick_skips_the_rebuild() {
        let gate = RescanGate::default();

        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::SkipIdle);
        assert_eq!(
            periodic_decision(Some(&gate)),
            RescanDecision::SkipIdle,
            "a skipped tick leaves the rebuild slot free"
        );
    }

    #[test]
    fn requested_alternate_rescans_once_per_request() {
        let gate = RescanGate::default();
        gate.mark_used();

        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::Run);
        gate.finish();
        assert_eq!(
            periodic_decision(Some(&gate)),
            RescanDecision::SkipIdle,
            "the served rescan consumes the request signal"
        );

        gate.mark_used();
        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::Run);
    }

    /// A rebuild slower than the rescan interval must drop ticks, not queue
    /// them, or a large corpus rebuilds back to back forever.
    #[test]
    fn tick_during_a_rebuild_is_dropped_not_queued() {
        let gate = RescanGate::default();
        gate.mark_used();
        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::Run);

        gate.mark_used();
        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::SkipInFlight);
        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::SkipInFlight);

        gate.finish();
        assert_eq!(
            periodic_decision(Some(&gate)),
            RescanDecision::Run,
            "requests during the suppressed ticks are honored by the next one"
        );
    }

    #[test]
    fn tick_during_a_change_driven_rebuild_is_skipped() {
        let gate = RescanGate::default();
        gate.mark_used();
        gate.begin_change();

        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::SkipInFlight);

        gate.finish();
        assert_eq!(periodic_decision(Some(&gate)), RescanDecision::Run);
    }

    #[tokio::test]
    async fn failed_reload_keeps_last_good_snapshot() {
        let mut graph = VaultGraph::new();
        graph.add_node(VaultNode {
            id: "generate:test:last-good".into(),
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
        assert!(snapshot.graph.nodes.contains_key("generate:test:last-good"));
    }

    struct UnchangedImporter;

    impl Importer for UnchangedImporter {
        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new(
                "unchanged",
                "Unchanged",
                "1",
                vec![data_loader::Capability::new(
                    Effect::Read,
                    Transport::InMemory,
                    "unchanged",
                )],
                test_schema(),
            )
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<ImportOutcome, ImportError>> {
            Box::pin(async { Ok(ImportOutcome::Unchanged) })
        }
    }

    /// A poll tick that resolves to [`ImportOutcome::Unchanged`] must keep
    /// the current snapshot and stay completely silent on the progress log:
    /// no reload stage, no "change detected" info, no snapshot build.
    #[tokio::test]
    async fn unchanged_reload_keeps_snapshot_and_emits_no_progress() {
        let mut graph = VaultGraph::new();
        graph.add_node(VaultNode {
            id: "generate:test:kept".into(),
            meta: vault_data::NodeMeta {
                source_id: "test".into(),
                title: "Kept".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let raw: Box<dyn Importer> = Box::new(UnchangedImporter);
        let grants = raw.descriptor().capabilities;
        let importer = HostedImporter::new(raw, grants).unwrap();
        let progress = Arc::new(crate::progress::ProgressLog::new());
        let state = crate::AppState::new(
            PathBuf::new(),
            importer,
            test_result(graph),
            None,
            crate::compute_broker::ComputeBroker::new(),
            progress.clone(),
        )
        .unwrap();
        let revision = state.snapshot().revision;
        let events_before = progress.since(0).next_seq;

        assert!(!rebuild_snapshot(&state).await);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.graph.nodes.contains_key("generate:test:kept"));
        assert_eq!(
            progress.since(0).next_seq,
            events_before,
            "an unchanged tick must not emit any progress event"
        );
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
