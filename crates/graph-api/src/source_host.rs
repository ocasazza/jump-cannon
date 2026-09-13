//! Runtime per-viewer importer source hosting.
//!
//! [`SourceHost`] owns the deployment-default [`AppState`] plus a lazily-built
//! map of alternate serving states, one per runnable catalog source — the same
//! pattern `session-manager`'s `ensure_serving` uses to run one `AppState` per
//! world. Each alternate gets its own importer, progress log, and watcher
//! built through the same `build_world_state` path as the default; its compute
//! broker is a fresh, unconnected one, so alternates are read-only graph views
//! (the layout WS 503s there and the client falls back to in-browser layout).
//!
//! Selection arrives as the `x-jump-cannon-source` request header (or the
//! `source` query parameter on the layout WebSocket, whose browser API cannot
//! set headers). Selecting a non-default source is gated: runtime switching
//! must be enabled (`JUMP_CANNON_IMPORTER_SWITCH_GROUP`) AND the caller's
//! groups header must contain the configured group. When the gate is unset the
//! header is ignored entirely and every request is served the default —
//! exactly today's behavior. Writes and compute endpoints stay default-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::importer_catalog::{
    CatalogSourceKind, ImporterCatalog, ImporterSourceDefinition, RuntimeSwitchStatus,
};
use crate::progress::{ProgressLog, ProgressResponse};
use crate::state::AppState;
use crate::watcher::RescanGate;

/// Header carrying the viewer's selected source id on plain HTTP calls.
pub const SOURCE_HEADER: &str = "x-jump-cannon-source";
/// Query parameter carrying the selected source id on the layout WebSocket.
pub const SOURCE_QUERY_PARAM: &str = "source";
/// Default header carrying the caller's comma-separated group memberships
/// (injected by the authenticating proxy; overridable via
/// `JUMP_CANNON_USER_GROUPS_HEADER`).
pub const DEFAULT_GROUPS_HEADER: &str = "x-netbird-groups";

/// Runtime switching configuration. Fail-closed: with no required group the
/// feature is disabled and source selection headers are ignored.
#[derive(Debug, Clone)]
pub struct SwitchConfig {
    required_group: Option<String>,
    groups_header: HeaderName,
}

impl Default for SwitchConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

impl SwitchConfig {
    pub fn disabled() -> Self {
        Self {
            required_group: None,
            groups_header: HeaderName::from_static(DEFAULT_GROUPS_HEADER),
        }
    }

    pub fn new(required_group: Option<String>, groups_header: &str) -> Self {
        let groups_header = HeaderName::from_bytes(groups_header.as_bytes())
            .unwrap_or_else(|_| HeaderName::from_static(DEFAULT_GROUPS_HEADER));
        let required_group = required_group.and_then(|group| {
            let trimmed = group.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        });
        Self {
            required_group,
            groups_header,
        }
    }

    pub fn enabled(&self) -> bool {
        self.required_group.is_some()
    }

    /// The caller's group memberships: the configured header split on commas.
    pub fn caller_groups(&self, headers: &HeaderMap) -> Vec<String> {
        headers
            .get(&self.groups_header)
            .and_then(|value| value.to_str().ok())
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|group| !group.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the caller may select a non-default source.
    pub fn authorize(&self, headers: &HeaderMap) -> bool {
        match &self.required_group {
            Some(required) => self.caller_groups(headers).iter().any(|g| g == required),
            None => false,
        }
    }

    /// The per-request `runtimeSwitch` block for `GET /importers`.
    pub fn status(&self, headers: &HeaderMap) -> RuntimeSwitchStatus {
        RuntimeSwitchStatus {
            enabled: self.enabled(),
            allowed: self.enabled() && self.authorize(headers),
            required_group: self.required_group.clone(),
        }
    }
}

/// Why a source selection could not be served. Maps onto the wire error
/// contract: unknown id → 404, not runnable → 400, unauthorized → 403,
/// a build in progress → 202 with live status, a cached failure → 503 JSON,
/// and a state conflict (retry while building/serving) → 409.
#[derive(Debug)]
pub enum SourceError {
    Forbidden,
    Unknown(String),
    NotRunnable(String),
    /// The selected alternate is still building; carries the live status the
    /// 202 body reports so the client can render a granular, non-error wait.
    Building(BuildStatus),
    /// A cached build failure. Graph routes answer 503 with a JSON body so a
    /// real failure is never masked; `/status` still reports it as `failed`
    /// and exposes retry.
    BuildFailed { source: String, error: String },
    /// A retry requested against a source that is already building or serving.
    Conflict(String),
}

impl IntoResponse for SourceError {
    fn into_response(self) -> Response {
        match self {
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "selecting a non-default importer source requires the configured group",
            )
                .into_response(),
            Self::Unknown(id) => (
                StatusCode::NOT_FOUND,
                format!("unknown importer source {id:?}"),
            )
                .into_response(),
            Self::NotRunnable(id) => (
                StatusCode::BAD_REQUEST,
                format!("importer source {id:?} is not runnable at runtime"),
            )
                .into_response(),
            Self::Building(status) => {
                let mut response = (StatusCode::ACCEPTED, axum::Json(status)).into_response();
                response.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    axum::http::HeaderValue::from_static("2"),
                );
                response
            }
            Self::BuildFailed { source, error } => (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "status": "failed",
                    "source": source,
                    "error": error,
                })),
            )
                .into_response(),
            Self::Conflict(message) => (StatusCode::CONFLICT, message).into_response(),
        }
    }
}

/// The 202 body for a graph route whose selected alternate is still building.
/// `status` is always `"building"`; the remaining fields are the live values
/// read from the alternate's progress log at request time.
#[derive(Debug, serde::Serialize)]
pub struct BuildStatus {
    pub status: &'static str,
    pub source: String,
    pub elapsed_ms: u64,
    pub stage: Option<String>,
    pub detail: Option<String>,
    pub fraction: Option<f32>,
}

/// The `GET /importers/sources/:id/status` body. Never blocks and never 503s:
/// every state (building, serving, failed, idle) maps to one 200 JSON shape.
#[derive(Debug, serde::Serialize)]
pub struct BuildStatusReport {
    pub status: &'static str,
    pub source: String,
    pub elapsed_ms: Option<u64>,
    pub stage: Option<String>,
    pub detail: Option<String>,
    pub fraction: Option<f32>,
    pub error: Option<String>,
}

impl BuildStatusReport {
    fn serving(source: &str) -> Self {
        Self {
            status: "serving",
            source: source.to_owned(),
            elapsed_ms: None,
            stage: None,
            detail: None,
            fraction: None,
            error: None,
        }
    }

    fn idle(source: &str) -> Self {
        Self {
            status: "idle",
            source: source.to_owned(),
            elapsed_ms: None,
            stage: None,
            detail: None,
            fraction: None,
            error: None,
        }
    }

    fn building(source: &str, progress: &ProgressLog, started: Instant) -> Self {
        let latest = progress.latest();
        Self {
            status: "building",
            source: source.to_owned(),
            elapsed_ms: Some(elapsed_ms(started)),
            stage: latest.stage,
            detail: latest.detail,
            fraction: latest.fraction,
            error: None,
        }
    }

    fn failed(source: &str, error: &str) -> Self {
        Self {
            status: "failed",
            source: source.to_owned(),
            elapsed_ms: None,
            stage: None,
            detail: None,
            fraction: None,
            error: Some(error.to_owned()),
        }
    }
}

/// Outcome of gating a `/status`, `/progress`, or `/retry` request: the
/// request either targets the always-serving deployment default or an
/// authorized, runnable alternate.
enum GateOutcome {
    Default,
    Alternate,
}

/// Which source a request resolves to, before any build is attempted.
enum Selection {
    Default,
    Alternate(String),
}

/// The outcome of resolving one request's source selection.
pub struct ResolvedSource {
    pub state: AppState,
    /// The alternate source being served; `None` on the deployment default.
    pub alternate: Option<String>,
}

impl std::fmt::Debug for ResolvedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSource")
            .field("alternate", &self.alternate)
            .finish_non_exhaustive()
    }
}
enum AlternateSource {
    /// Build in progress: no request awaits it. Graph routes resolving here
    /// answer 202 with live status read from `progress`; the spawned `task`
    /// swaps this entry to `Serving` or `Failed` when it finishes. Never
    /// evicted while building.
    Building {
        progress: Arc<ProgressLog>,
        started: Instant,
        task: tokio::task::JoinHandle<()>,
        /// Identifies this exact build so a stale task cannot resurrect an
        /// entry that was invalidated or retried while it ran.
        generation: u64,
    },
    Serving {
        state: AppState,
        /// Background rescan task for this alternate; aborted on eviction.
        watcher: Option<tokio::task::JoinHandle<()>>,
    },
    /// Cached build failure: graph routes answer 503 JSON without re-attempting
    /// the build on every request, and `/status` exposes retry. Evicted on the
    /// same idle TTL as a serving entry.
    Failed {
        message: String,
        progress: Arc<ProgressLog>,
        finished: Instant,
    },
}

/// One entry in the alternates map: its state plus how recently a request
/// resolved to it — `last_used` drives idle eviction, `gate` drives the
/// watcher's periodic rescan. Inert on a `Failed` entry, which has no
/// watcher.
struct AlternateEntry {
    source: AlternateSource,
    last_used: Instant,
    gate: Arc<RescanGate>,
}

/// Record a request resolving to this entry. Both signals move together: an
/// entry kept resident by traffic must also keep rescanning, and one that
/// stops being requested must stop rebuilding before the sweep evicts it.
fn mark_entry_used(entry: &mut AlternateEntry, now: Instant) {
    entry.last_used = now;
    entry.gate.mark_used();
}

/// How long a lazily-built alternate stays resident — and its background
/// rescan watcher keeps running — without being requested again. Without
/// this, one past request to an expensive alternate (e.g. a large corpus on
/// a short `filesystemRescanIntervalSeconds`) pins a second full graph/search
/// rebuild loop in memory for the life of the process.
const ALTERNATE_IDLE_TTL: Duration = Duration::from_secs(15 * 60);
/// How often the idle sweep runs.
const ALTERNATE_EVICTION_INTERVAL: Duration = Duration::from_secs(60);

struct SourceHostInner {
    default: AppState,
    /// Chart catalog plus runtime overlay entries. Swapped wholesale when
    /// `POST /importers` adds a source; readers never block.
    catalog: ArcSwap<ImporterCatalog>,
    switch: SwitchConfig,
    alternates: RwLock<HashMap<String, AlternateEntry>>,
    /// Constructs one alternate's serving state. Deployments use
    /// `default_builder`; tests inject a fake to drive the build state
    /// machine without a real importer.
    builder: AlternateBuilder,
    /// Monotonic build id so a completed build task only replaces the exact
    /// `Building` entry it belongs to (never one invalidated or retried since).
    build_generation: AtomicU64,
    idle_ttl: Duration,
    eviction_interval: Duration,
    /// Directory the chart mounts every httpjson catalog package into (shared
    /// by the rollout `--importer-manifest` binding and every catalog-declared
    /// httpjson alternate). Required to construct a runtime-switchable
    /// `httpjson` alternate; the rollout path still resolves its own full
    /// path via `--importer-manifest`.
    packages_dir: Option<PathBuf>,
}

/// Idle detection: ids whose entry hasn't been used since `now - idle_ttl`.
/// Pure and side-effect-free so the exact selection logic is unit-testable
/// without a real `AppState` or waiting on real timers.
fn expired_alternate_ids(
    alternates: &HashMap<String, AlternateEntry>,
    now: Instant,
    idle_ttl: Duration,
) -> Vec<String> {
    alternates
        .iter()
        .filter(|(_, entry)| !matches!(entry.source, AlternateSource::Building { .. }))
        .filter(|(_, entry)| now.duration_since(entry.last_used) >= idle_ttl)
        .map(|(id, _)| id.clone())
        .collect()
}
/// Cloneable handle owning the default serving state plus lazily-built
/// alternates. This is the axum router state for the standalone server.
#[derive(Clone)]
pub struct SourceHost {
    inner: Arc<SourceHostInner>,
}

impl SourceHost {
    /// Build a host that does not need to construct runtime-switchable
    /// `httpjson` alternates. Catalog-declared filesystem/OKF/Obsidian
    /// alternates still work; constructing an `httpjson` alternate will
    /// fail with a clear deployment error. Use [`Self::with_packages_dir`]
    /// (or one of the rollover entry points) when chart-side secret
    /// injection of a real `httpjson` alternate is needed.
    pub fn new(default: AppState, switch: SwitchConfig) -> Self {
        Self::with_packages_dir(default, switch, None)
    }

    /// Like [`Self::new`] but with an explicit `packages_dir` for resolving
    /// catalog-declared `httpjson` alternate package filenames.
    pub fn with_packages_dir(
        default: AppState,
        switch: SwitchConfig,
        packages_dir: Option<PathBuf>,
    ) -> Self {
        Self::with_ttl(
            default,
            switch,
            packages_dir,
            ALTERNATE_IDLE_TTL,
            ALTERNATE_EVICTION_INTERVAL,
        )
    }

    /// Like [`Self::with_packages_dir`] with an overridable idle TTL and sweep
    /// interval — exercised by tests so eviction doesn't require a real
    /// 15-minute wait.
    pub fn with_ttl(
        default: AppState,
        switch: SwitchConfig,
        packages_dir: Option<PathBuf>,
        idle_ttl: Duration,
        eviction_interval: Duration,
    ) -> Self {
        Self::with_ttl_and_builder(
            default,
            switch,
            packages_dir,
            idle_ttl,
            eviction_interval,
            default_builder(),
        )
    }

    /// Like [`Self::with_ttl`] but with an injectable per-source build
    /// closure. Deployments never call this directly; the unit tests use it
    /// to drive the Building/Serving/Failed state machine with a fake builder
    /// that sleeps or fails on demand without a real importer.
    fn with_ttl_and_builder(
        default: AppState,
        switch: SwitchConfig,
        packages_dir: Option<PathBuf>,
        idle_ttl: Duration,
        eviction_interval: Duration,
        builder: AlternateBuilder,
    ) -> Self {
        let host = Self {
            inner: Arc::new(SourceHostInner {
                catalog: ArcSwap::from_pointee(default.inner.importer_catalog.clone()),
                default,
                switch,
                alternates: RwLock::new(HashMap::new()),
                idle_ttl,
                eviction_interval,
                packages_dir,
                builder,
                build_generation: AtomicU64::new(0),
            }),
        };
        if host.inner.switch.enabled() {
            host.spawn_eviction_sweep();
        }
        host
    }

    /// Periodically evicts alternates idle longer than `ALTERNATE_IDLE_TTL`,
    /// aborting each evicted entry's background rescan watcher. Without this
    /// an alternate built by a single past request would rescan and rebuild
    /// its full graph/search index forever, alongside the default source.
    fn spawn_eviction_sweep(&self) {
        let inner = Arc::clone(&self.inner);
        let idle_ttl = inner.idle_ttl;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(inner.eviction_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let expired: Vec<(String, Option<tokio::task::JoinHandle<()>>)> = {
                    let mut alternates = inner
                        .alternates
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let expired_ids = expired_alternate_ids(&alternates, Instant::now(), idle_ttl);
                    expired_ids
                        .into_iter()
                        .filter_map(|id| {
                            alternates.remove(&id).map(|entry| {
                                let watcher = match entry.source {
                                    AlternateSource::Serving { watcher, .. } => watcher,
                                    AlternateSource::Building { task, .. } => Some(task),
                                    AlternateSource::Failed { .. } => None,
                                };
                                (id, watcher)
                            })
                        })
                        .collect()
                };
                for (source_id, watcher) in expired {
                    if let Some(handle) = watcher {
                        handle.abort();
                    }
                    tracing::info!(source = %source_id, "evicted idle alternate importer source");
                }
            }
        });
    }

    /// A host with switching disabled: every request serves the default.
    pub fn default_only(default: AppState) -> Self {
        Self::new(default, SwitchConfig::disabled())
    }

    pub fn default_state(&self) -> &AppState {
        &self.inner.default
    }

    pub fn switch(&self) -> &SwitchConfig {
        &self.inner.switch
    }

    /// The catalog as of this call. Runtime additions swap in a new
    /// snapshot; a held `Arc` keeps serving the old view consistently.
    pub fn catalog(&self) -> Arc<ImporterCatalog> {
        self.inner.catalog.load_full()
    }

    /// The directory catalog `httpJson.package` filenames resolve against.
    pub fn packages_dir(&self) -> Option<&Path> {
        self.inner.packages_dir.as_deref()
    }

    /// Drop a cached alternate (serving or failed) so the next selection
    /// rebuilds it from the current package file. No-op for unknown ids.
    pub fn invalidate_alternate(&self, source_id: &str) {
        match self.write_alternates().remove(source_id) {
            Some(AlternateEntry {
                source: AlternateSource::Serving {
                    watcher: Some(handle),
                    ..
                },
                ..
            }) => handle.abort(),
            Some(AlternateEntry {
                source: AlternateSource::Building { task, .. },
                ..
            }) => task.abort(),
            _ => {}
        }
    }

    /// Add a runtime-authored source to the live catalog (see
    /// [`ImporterCatalog::insert_runtime_source`]). Compare-and-swap: two
    /// concurrent adds of the same id resolve to one success and one clean
    /// "already exists".
    pub fn add_runtime_source(
        &self,
        id: String,
        definition: ImporterSourceDefinition,
    ) -> Result<(), String> {
        let mut outcome = Ok(());
        self.inner.catalog.rcu(|current| {
            let mut next = ImporterCatalog::clone(current);
            outcome = next.insert_runtime_source(id.clone(), definition.clone());
            next
        });
        outcome
    }

    /// Resolve a request's selection to the serving state. `requested` is the
    /// raw header/query value; `headers` carries the caller's group memberships.
    /// Never awaits a build: an alternate still building resolves to
    /// [`SourceError::Building`] (202), a cached failure to
    /// [`SourceError::BuildFailed`] (503).
    pub async fn select(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedSource, SourceError> {
        match self.resolve_selection(requested, headers)? {
            Selection::Default => Ok(self.default_resolution()),
            Selection::Alternate(id) => {
                let state = self.ensure_serving(&id)?;
                Ok(ResolvedSource {
                    state,
                    alternate: Some(id),
                })
            }
        }
    }

    /// Like [`Self::select`] but never builds: a non-default selection is
    /// reported as an alternate so write/compute routes can reject it with 400
    /// without ever triggering (or waiting on) a build.
    pub fn resolve_default(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedSource, SourceError> {
        match self.resolve_selection(requested, headers)? {
            Selection::Default => Ok(self.default_resolution()),
            Selection::Alternate(id) => Ok(ResolvedSource {
                state: self.inner.default.clone(),
                alternate: Some(id),
            }),
        }
    }

    /// The shared selection front matter for [`Self::select`] and
    /// [`Self::resolve_default`]: gate the raw header/query value to either the
    /// deployment default or a specific authorized, runnable alternate id,
    /// without building anything. A closed gate or the default id ignores the
    /// selection entirely (today's behavior).
    fn resolve_selection(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<Selection, SourceError> {
        let requested = requested.map(str::trim).filter(|id| !id.is_empty());
        let Some(id) = requested else {
            return Ok(Selection::Default);
        };
        // Gate closed: the header is ignored entirely — today's behavior.
        if !self.inner.switch.enabled() {
            return Ok(Selection::Default);
        }
        // The deployment default never requires authorization; the UI may send
        // the header unconditionally once a viewer has switched back.
        let catalog = self.inner.catalog.load();
        if catalog.selected() == Some(id) {
            return Ok(Selection::Default);
        }
        if !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = catalog.source(id) else {
            return Err(SourceError::Unknown(id.to_owned()));
        };
        if !definition.runnable() {
            return Err(SourceError::NotRunnable(id.to_owned()));
        }
        Ok(Selection::Alternate(id.to_owned()))
    }

    fn default_resolution(&self) -> ResolvedSource {
        ResolvedSource {
            state: self.inner.default.clone(),
            alternate: None,
        }
    }

    /// Resolve one runnable alternate to its serving state without ever
    /// awaiting a build. A map lookup is the only blocking work: a `Serving`
    /// entry returns its state, a `Building` entry returns
    /// [`SourceError::Building`] with live status, a `Failed` entry returns
    /// [`SourceError::BuildFailed`], and a missing entry spawns the build task
    /// and returns `Building` immediately. The background task swaps the entry
    /// to `Serving`/`Failed` when it completes (see [`Self::spawn_build`]).
    fn ensure_serving(&self, source_id: &str) -> Result<AppState, SourceError> {
        let mut alternates = self.write_alternates();
        if let Some(entry) = alternates.get_mut(source_id) {
            mark_entry_used(entry, Instant::now());
            return match &entry.source {
                AlternateSource::Serving { state, .. } => Ok(state.clone()),
                AlternateSource::Building {
                    progress, started, ..
                } => Err(SourceError::Building(build_status(
                    source_id, progress, *started,
                ))),
                AlternateSource::Failed { message, .. } => Err(SourceError::BuildFailed {
                    source: source_id.to_owned(),
                    error: message.clone(),
                }),
            };
        }
        // No entry yet: start the build in the background and report Building.
        let progress = Arc::new(ProgressLog::new());
        let gate = Arc::new(RescanGate::default());
        let started = Instant::now();
        let generation = self.next_generation();
        let task = self.spawn_build(
            source_id.to_owned(),
            generation,
            Arc::clone(&progress),
            Arc::clone(&gate),
        );
        let status = build_status(source_id, &progress, started);
        alternates.insert(
            source_id.to_owned(),
            AlternateEntry {
                source: AlternateSource::Building {
                    progress,
                    started,
                    task,
                    generation,
                },
                last_used: Instant::now(),
                gate,
            },
        );
        Err(SourceError::Building(status))
    }

    /// Spawn the background build for one alternate. On completion the task
    /// swaps the `Building` entry to `Serving` or `Failed` — but only if it is
    /// still the exact build it started (see [`finalize_build`]).
    fn spawn_build(
        &self,
        source_id: String,
        generation: u64,
        progress: Arc<ProgressLog>,
        gate: Arc<RescanGate>,
    ) -> tokio::task::JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        let builder = Arc::clone(&self.inner.builder);
        tokio::spawn(async move {
            let Some(definition) = inner.catalog.load().source(&source_id).cloned() else {
                // The source vanished from the catalog between selection and
                // build (invalidated or removed). Record it as a retryable
                // failure rather than silently dropping the entry.
                finalize_build(
                    &inner,
                    &source_id,
                    generation,
                    Err(format!("unknown importer source {source_id:?}")),
                    progress,
                    gate,
                );
                return;
            };
            let request = BuildRequest {
                source_id: source_id.clone(),
                definition,
                packages_dir: inner.packages_dir.clone(),
                gate: Arc::clone(&gate),
                progress: Arc::clone(&progress),
            };
            let result = (*builder)(request).await;
            finalize_build(&inner, &source_id, generation, result, progress, gate);
        })
    }

    fn next_generation(&self) -> u64 {
        self.inner.build_generation.fetch_add(1, Ordering::Relaxed)
    }

    /// Report one source's build state without ever blocking or 503-ing.
    /// `Default` and `Serving` are `serving`; a missing alternate is `idle`.
    pub fn status(
        &self,
        source_id: &str,
        headers: &HeaderMap,
    ) -> Result<BuildStatusReport, SourceError> {
        let id = source_id.trim();
        match self.gate_alternate(source_id, headers)? {
            GateOutcome::Default => Ok(BuildStatusReport::serving(id)),
            GateOutcome::Alternate => {
                let alternates = self.read_alternates();
                Ok(match alternates.get(id) {
                    None => BuildStatusReport::idle(id),
                    Some(entry) => match &entry.source {
                        AlternateSource::Building {
                            progress, started, ..
                        } => BuildStatusReport::building(id, progress, *started),
                        AlternateSource::Serving { .. } => BuildStatusReport::serving(id),
                        AlternateSource::Failed { message, .. } => {
                            BuildStatusReport::failed(id, message)
                        }
                    },
                })
            }
        }
    }

    /// The alternate's own progress log tail. Available in every state that has
    /// a log (building, serving, failed); an unbuilt alternate returns an empty
    /// response. Never blocks.
    pub fn progress(
        &self,
        source_id: &str,
        since: u64,
        headers: &HeaderMap,
    ) -> Result<ProgressResponse, SourceError> {
        let id = source_id.trim();
        match self.gate_alternate(source_id, headers)? {
            GateOutcome::Default => Ok(self.inner.default.inner.progress.since(since)),
            GateOutcome::Alternate => {
                let alternates = self.read_alternates();
                Ok(match alternates.get(id) {
                    None => ProgressLog::new().since(since),
                    Some(entry) => match &entry.source {
                        AlternateSource::Building { progress, .. } => progress.since(since),
                        AlternateSource::Serving { state, .. } => {
                            state.inner.progress.since(since)
                        }
                        AlternateSource::Failed { progress, .. } => progress.since(since),
                    },
                })
            }
        }
    }

    /// Clear a cached failure and start a fresh build. 409 if the source is
    /// already building or serving; the deployment default is always serving.
    pub fn retry(
        &self,
        source_id: &str,
        headers: &HeaderMap,
    ) -> Result<BuildStatusReport, SourceError> {
        let id = source_id.trim();
        match self.gate_alternate(source_id, headers)? {
            GateOutcome::Default => Err(SourceError::Conflict(format!(
                "importer source {id:?} is the deployment default and is always serving"
            ))),
            GateOutcome::Alternate => {
                let mut alternates = self.write_alternates();
                if let Some(entry) = alternates.get(id) {
                    match &entry.source {
                        AlternateSource::Building { .. } => {
                            return Err(SourceError::Conflict(format!(
                                "importer source {id:?} is already building"
                            )));
                        }
                        AlternateSource::Serving { .. } => {
                            return Err(SourceError::Conflict(format!(
                                "importer source {id:?} is already serving"
                            )));
                        }
                        AlternateSource::Failed { finished, .. } => {
                            tracing::info!(
                                source = %id,
                                failed_for_ms = elapsed_ms(*finished),
                                "retrying failed alternate importer source"
                            );
                        }
                    }
                }
                let progress = Arc::new(ProgressLog::new());
                let gate = Arc::new(RescanGate::default());
                let started = Instant::now();
                let generation = self.next_generation();
                let task = self.spawn_build(
                    id.to_owned(),
                    generation,
                    Arc::clone(&progress),
                    Arc::clone(&gate),
                );
                let report = BuildStatusReport::building(id, &progress, started);
                alternates.insert(
                    id.to_owned(),
                    AlternateEntry {
                        source: AlternateSource::Building {
                            progress,
                            started,
                            task,
                            generation,
                        },
                        last_used: Instant::now(),
                        gate,
                    },
                );
                Ok(report)
            }
        }
    }

    /// Gate a `/status`, `/progress`, or `/retry` request with the same
    /// authorization and validity checks as [`Self::select`]: the deployment
    /// default is always allowed; any other id requires the configured group
    /// and must name a known, runnable catalog source.
    fn gate_alternate(
        &self,
        source_id: &str,
        headers: &HeaderMap,
    ) -> Result<GateOutcome, SourceError> {
        let id = source_id.trim();
        if id.is_empty() {
            return Ok(GateOutcome::Default);
        }
        let catalog = self.inner.catalog.load();
        if catalog.selected() == Some(id) {
            return Ok(GateOutcome::Default);
        }
        if !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = catalog.source(id) else {
            return Err(SourceError::Unknown(id.to_owned()));
        };
        if !definition.runnable() {
            return Err(SourceError::NotRunnable(id.to_owned()));
        }
        Ok(GateOutcome::Alternate)
    }

    fn read_alternates(&self) -> RwLockReadGuard<'_, HashMap<String, AlternateEntry>> {
        self.inner
            .alternates
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_alternates(&self) -> RwLockWriteGuard<'_, HashMap<String, AlternateEntry>> {
        self.inner
            .alternates
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Inputs to one alternate build, bundled so the build step is a simple
/// `Fn(BuildRequest) -> Future` seam that tests replace with a fake.
struct BuildRequest {
    source_id: String,
    definition: crate::importer_catalog::ImporterSourceDefinition,
    packages_dir: Option<PathBuf>,
    gate: Arc<RescanGate>,
    progress: Arc<ProgressLog>,
}

/// The boxed future one build closure returns. Named so both the production
/// closure and the test fakes can annotate their return type, which is what
/// lets a concrete `Box::pin(async …)` coerce to the trait-object form.
type BuildFuture =
    Pin<Box<dyn Future<Output = Result<(AppState, Option<tokio::task::JoinHandle<()>>), String>> + Send>>;

/// Builds one alternate's serving state plus its rescan watcher handle.
/// Deployments use [`default_builder`] (which calls [`build_alternate`]);
/// tests inject a fake to exercise the build state machine without a real
/// importer.
type AlternateBuilder = Arc<dyn Fn(BuildRequest) -> BuildFuture + Send + Sync>;

/// The production build closure: dispatch to [`build_alternate`] on the
/// catalog definition, threading the shared progress log through so the live
/// build status and `/progress` tail reflect the importer's own reporting.
fn default_builder() -> AlternateBuilder {
    Arc::new(|request: BuildRequest| -> BuildFuture {
        Box::pin(async move {
            build_alternate(
                &request.source_id,
                &request.definition,
                request.packages_dir.as_deref(),
                request.gate,
                request.progress,
            )
            .await
        })
    })
}

/// Swap a completed build into its map entry — but only if it is still the
/// exact `Building` entry this task started. A build invalidated or retried
/// while it ran leaves a different (or absent) entry, which a stale task must
/// never resurrect.
fn finalize_build(
    inner: &SourceHostInner,
    source_id: &str,
    generation: u64,
    result: Result<(AppState, Option<tokio::task::JoinHandle<()>>), String>,
    progress: Arc<ProgressLog>,
    gate: Arc<RescanGate>,
) {
    let mut alternates = inner
        .alternates
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let still_ours = matches!(
        alternates.get(source_id),
        Some(AlternateEntry {
            source: AlternateSource::Building { generation: current, .. },
            ..
        }) if *current == generation
    );
    if !still_ours {
        return;
    }
    match result {
        Ok((state, watcher)) => {
            tracing::info!(source = %source_id, "alternate importer source serving");
            alternates.insert(
                source_id.to_owned(),
                AlternateEntry {
                    source: AlternateSource::Serving { state, watcher },
                    last_used: Instant::now(),
                    gate,
                },
            );
        }
        Err(message) => {
            tracing::warn!(source = %source_id, error = %message, "alternate source build failed");
            alternates.insert(
                source_id.to_owned(),
                AlternateEntry {
                    source: AlternateSource::Failed {
                        message,
                        progress,
                        finished: Instant::now(),
                    },
                    last_used: Instant::now(),
                    gate,
                },
            );
        }
    }
}

/// The live 202 status for a building alternate.
fn build_status(source_id: &str, progress: &ProgressLog, started: Instant) -> BuildStatus {
    let latest = progress.latest();
    BuildStatus {
        status: "building",
        source: source_id.to_owned(),
        elapsed_ms: elapsed_ms(started),
        stage: latest.stage,
        detail: latest.detail,
        fraction: latest.fraction,
    }
}

/// Milliseconds since `started`, saturating rather than wrapping.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Construct one alternate serving state from its catalog entry, plus its
/// background rescan watcher handle (if any), so the caller can abort it on
/// eviction. Runnable kinds are exactly the filesystem kinds constructible
/// from catalog metadata (OKF needs only root + sourceId, Obsidian only a
/// root) plus the `httpjson` kind whose package filename, endpoint, and
/// variables are all carried by the catalog profile. The entry's declared
/// rescan interval drives the filesystem alt's periodic full rescan
/// (0 = notifications only); poll-kind sources (httpjson) drive their own
/// cadence via the importer's `WatchPlan::Poll` and ignore that argument.
/// Every alternate's driver is gated on `gate`, so its periodic rebuilds
/// stop while no request resolves to this source.
async fn build_alternate(
    source_id: &str,
    definition: &crate::importer_catalog::ImporterSourceDefinition,
    packages_dir: Option<&Path>,
    gate: Arc<RescanGate>,
    progress: Arc<ProgressLog>,
) -> Result<(AppState, Option<tokio::task::JoinHandle<()>>), String> {
    match definition.kind {
        CatalogSourceKind::HttpJson => {
            let http_json = definition.http_json.as_ref().ok_or_else(|| {
                format!("httpjson source {source_id:?} has no httpJson contract")
            })?;
            let packages_dir = packages_dir.ok_or_else(|| {
                format!(
                    "httpjson source {source_id:?} cannot be served: \
                     JUMP_CANNON_IMPORTER_PACKAGES_DIR is not configured"
                )
            })?;
            let manifest_path = packages_dir.join(&http_json.package);
            // Package validation compiles the declared grammar and is
            // synchronous CPU work; keep it off the async runtime.
            let (package, _definition) = tokio::task::spawn_blocking(move || {
                crate::importer_package::load_importer_package(&manifest_path)
            })
            .await
            .map_err(|error| format!("httpjson source {source_id:?}: package load task failed: {error}"))?
            .map_err(|error| format!("httpjson source {source_id:?}: {error}"))?;
            let source_id_value = definition
                .source_id
                .clone()
                .unwrap_or_else(|| sanitize_source_id(&package.manifest().metadata.id));
            let token = http_json
                .token_env
                .as_deref()
                .and_then(|name| std::env::var(name).ok());
            let instance = importer::InstanceConfig {
                source_id: source_id_value,
                base_url: http_json.endpoint.clone(),
                variables: http_json.variables.clone(),
                token,
                poll_interval_ms: http_json.poll_interval_ms,
            };
            // Per `importer_catalog` validation the package filename is bounded
            // ASCII and contains no path separators, so the joined path is
            // always below `packages_dir`. The filesystem root is irrelevant
            // for a remote source; no capability here joins onto it.
            let root = PathBuf::new();
            let importer: Box<dyn data_loader::Importer> = Box::new(
                importer::build_importer(package, instance)
                    .map_err(|error| error.to_string())?,
            );
            let grants: std::collections::HashSet<data_loader::Capability> =
                importer.descriptor().capabilities.into_iter().collect();
            let state = crate::build_world_state(importer, grants, root, progress)
                .await
                .map_err(|error| {
                    format!("import alternate source {source_id:?}: {error}")
                })?;
            // The watcher dispatches on the importer's own `WatchPlan`
            // (Poll for httpjson, Filesystem for the OKF/Obsidian arms
            // below), so the rescan interval is ignored for this kind.
            let watcher = crate::watcher::spawn_gated(
                state.clone(),
                definition.filesystem_rescan_interval_seconds.unwrap_or(0),
                gate,
            );
            Ok((state, watcher))
        }
        CatalogSourceKind::Okf | CatalogSourceKind::Obsidian => {
            let filesystem = definition.source.as_ref().ok_or_else(|| {
                format!("importer source {source_id:?} has no filesystem source")
            })?;
            let root = PathBuf::from(&filesystem.path);
            let importer: Box<dyn data_loader::Importer> = match definition.kind {
                CatalogSourceKind::Okf => {
                    let okf_source_id = definition.source_id.clone().ok_or_else(|| {
                        format!("OKF source {source_id:?} must declare sourceId")
                    })?;
                    Box::new(
                        okf_importer::OkfImporter::new(root.clone(), okf_source_id)
                            .map_err(|error| error.to_string())?,
                    )
                }
                CatalogSourceKind::Obsidian => {
                    Box::new(vault_links::ObsidianLoader::new(root.clone()))
                }
                _ => unreachable!("filesystem match arm excludes HttpJson"),
            };
            let grants: std::collections::HashSet<data_loader::Capability> =
                importer.descriptor().capabilities.into_iter().collect();
            let state = crate::build_world_state(importer, grants, root, progress)
                .await
                .map_err(|error| format!("import alternate source {source_id:?}: {error}"))?;
            let watcher = crate::watcher::spawn_gated(
                state.clone(),
                definition.filesystem_rescan_interval_seconds.unwrap_or(0),
                gate,
            );
            Ok((state, watcher))
        }
        other => Err(format!(
            "importer source kind {other:?} is not constructible at runtime"
        )),
    }
}

/// Fold an arbitrary package id into the `[a-z0-9._-]` source-id charset.
/// Duplicates the (tiny) helper in `main.rs` because that file is a binary
/// and `source_host` is a library; the chart's bound catalog never reaches
/// for this branch because the package id is already validated upstream.
fn sanitize_source_id(package_id: &str) -> String {
    let sanitized: String = package_id
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .take(data_loader::identity::MAX_SOURCE_ID_BYTES)
        .collect();
    if sanitized.is_empty() {
        "httpjson".to_string()
    } else {
        sanitized
    }
}

/// Extract the requested source id: the header first, then the `source` query
/// parameter (the layout WebSocket cannot set headers). Catalog ids are
/// validated URL-safe ASCII, so no percent-decoding is needed.
fn requested_source(parts: &Parts) -> Option<String> {
    if let Some(value) = parts.headers.get(SOURCE_HEADER) {
        return value.to_str().ok().map(str::to_owned);
    }
    let query = parts.uri.query()?;
    for pair in query.split('&') {
        if let Some(value) = pair
            .strip_prefix(SOURCE_QUERY_PARAM)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(value.to_owned());
        }
    }
    None
}

/// axum extractor for read routes: resolves the request's source selection to
/// the serving [`AppState`] (default or lazily-built alternate).
pub struct SourceSelection(pub AppState);

#[axum::async_trait]
impl FromRequestParts<SourceHost> for SourceSelection {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        host: &SourceHost,
    ) -> Result<Self, Self::Rejection> {
        host.select(requested_source(parts).as_deref(), &parts.headers)
            .await
            .map(|resolved| Self(resolved.state))
            .map_err(IntoResponse::into_response)
    }
}

/// axum extractor for write/compute routes: like [`SourceSelection`] but
/// rejects a non-default selection with 400 — writes, generation, and the
/// compute broker stay on the deployment default source.
pub struct DefaultSource(pub AppState);

#[axum::async_trait]
impl FromRequestParts<SourceHost> for DefaultSource {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        host: &SourceHost,
    ) -> Result<Self, Self::Rejection> {
        let resolved = host
            .resolve_default(requested_source(parts).as_deref(), &parts.headers)
            .map_err(IntoResponse::into_response)?;
        if let Some(id) = resolved.alternate {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("writes and compute stay on the deployment default source; {id:?} is a read-only view"),
            )
                .into_response());
        }
        Ok(Self(resolved.state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Failed` entries need no `AppState`, so the idle-detection predicate
    /// can be exercised directly without spinning up a real importer —
    /// this is the exact selection logic that was missing before the fix,
    /// which pinned every lazily-built alternate in memory forever.
    fn entry(idle_for: Duration) -> AlternateEntry {
        AlternateEntry {
            source: AlternateSource::Failed {
                message: "unused".to_owned(),
                progress: Arc::new(ProgressLog::new()),
                finished: Instant::now(),
            },
            last_used: Instant::now() - idle_for,
            gate: Arc::new(RescanGate::default()),
        }
    }

    #[test]
    fn expired_alternate_ids_selects_only_entries_past_the_ttl() {
        let ttl = Duration::from_secs(60);
        let mut alternates = HashMap::new();
        alternates.insert("idle".to_owned(), entry(Duration::from_secs(120)));
        alternates.insert("fresh".to_owned(), entry(Duration::from_secs(1)));
        alternates.insert("at-boundary".to_owned(), entry(ttl));

        let mut expired = expired_alternate_ids(&alternates, Instant::now(), ttl);
        expired.sort();

        assert_eq!(
            expired,
            vec!["at-boundary".to_owned(), "idle".to_owned()],
            "only entries idle for at least the TTL are selected for eviction"
        );
    }

    #[test]
    fn expired_alternate_ids_is_empty_when_nothing_is_idle() {
        let ttl = Duration::from_secs(60);
        let mut alternates = HashMap::new();
        alternates.insert("fresh-a".to_owned(), entry(Duration::ZERO));
        alternates.insert("fresh-b".to_owned(), entry(Duration::from_secs(59)));

        assert!(expired_alternate_ids(&alternates, Instant::now(), ttl).is_empty());
    }

    /// A request between build and the next sweep must observe its own
    /// build immediately, not the pre-insert idle state — this is what
    /// `ensure_serving` relies on directly.
    #[test]
    fn marking_an_entry_used_resets_its_idle_clock() {
        let mut alternates = HashMap::new();
        alternates.insert("alt".to_owned(), entry(Duration::from_secs(120)));

        if let Some(entry) = alternates.get_mut("alt") {
            mark_entry_used(entry, Instant::now());
        }

        assert!(
            expired_alternate_ids(&alternates, Instant::now(), Duration::from_secs(60)).is_empty()
        );
    }

    /// The same touch that keeps an entry resident must arm its rescan gate;
    /// otherwise a source under active traffic is never refreshed.
    #[test]
    fn marking_an_entry_used_arms_its_rescan_gate() {
        let mut entry = entry(Duration::from_secs(120));
        let gate = Arc::clone(&entry.gate);
        assert_eq!(gate.begin_periodic(), crate::watcher::RescanDecision::SkipIdle);

        mark_entry_used(&mut entry, Instant::now());

        assert_eq!(gate.begin_periodic(), crate::watcher::RescanDecision::Run);
    }

    struct TinyImporter;

    fn tiny_schema() -> data_loader::ImporterSchema {
        use data_loader::{DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, TagHierarchySchema};
        data_loader::ImporterSchema::new(
            "generate",
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

    impl data_loader::Importer for TinyImporter {
        fn descriptor(&self) -> data_loader::ImporterDescriptor {
            data_loader::ImporterDescriptor::new(
                "tiny",
                "Tiny test importer",
                "1",
                vec![data_loader::Capability::new(
                    data_loader::Effect::Read,
                    data_loader::Transport::InMemory,
                    "tiny",
                )],
                tiny_schema(),
            )
        }

        fn import<'a>(
            &'a self,
            _progress: &'a dyn data_loader::ImportProgress,
        ) -> data_loader::ImportFuture<'a, Result<data_loader::LoadResult, data_loader::ImportError>>
        {
            Box::pin(async {
                let mut graph = vault_data::VaultGraph::new();
                graph.add_node(vault_data::VaultNode {
                    id: "generate:tiny:root".into(),
                    meta: vault_data::NodeMeta {
                        source_id: "tiny".into(),
                        title: "Root".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                Ok(data_loader::LoadResult {
                    graph,
                    search_documents: vec![data_loader::SearchDocument::new("generate:tiny:root")
                        .with("id", "generate:tiny:root")
                        .with("title", "Root")
                        .with("tags", serde_json::json!([]))],
                    unresolved: Vec::new(),
                })
            })
        }
    }

    /// A valid default [`AppState`] whose catalog declares a runnable
    /// `alt` obsidian source (never actually built — the tests inject a fake
    /// builder), so `select`/`status`/`retry` gate exactly like production.
    async fn default_state() -> AppState {
        let path = "/tmp/jump-cannon-source-host-test";
        let catalog_json = serde_json::json!({
            "selected": "default",
            "sources": {
                "default": {
                    "displayName": "Default",
                    "kind": "obsidian",
                    "source": {
                        "volumeName": "default-vol",
                        "existingClaim": "default-claim",
                        "mountPath": path,
                        "path": path,
                        "readOnly": false
                    }
                },
                "alt": {
                    "displayName": "Alt",
                    "kind": "obsidian",
                    "source": {
                        "volumeName": "alt-vol",
                        "existingClaim": "alt-claim",
                        "mountPath": path,
                        "path": path,
                        "readOnly": false
                    }
                }
            }
        })
        .to_string();
        let catalog = ImporterCatalog::parse_with_runtime_switch(
            Some(&catalog_json),
            data_loader::SourceKind::Generate,
            true,
        )
        .expect("switch catalog parses");
        let grants: std::collections::HashSet<data_loader::Capability> =
            std::iter::once(data_loader::Capability::new(
                data_loader::Effect::Read,
                data_loader::Transport::InMemory,
                "tiny",
            ))
            .collect();
        let importer =
            data_loader::HostedImporter::new(Box::new(TinyImporter), grants).expect("hosted importer");
        let progress = Arc::new(ProgressLog::new());
        let loaded = crate::vault_loader::load_with_progress(&importer, Some(&progress))
            .await
            .expect("initial load");
        AppState::new_with_importer_catalog(
            PathBuf::new(),
            importer,
            loaded,
            None,
            crate::compute_broker::ComputeBroker::new(),
            progress,
            catalog,
        )
        .expect("default app state")
    }

    fn test_switch() -> SwitchConfig {
        SwitchConfig::new(Some("switchers".to_owned()), "x-test-groups")
    }

    fn authed_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-test-groups"),
            axum::http::HeaderValue::from_static("switchers"),
        );
        headers
    }

    fn host_with_builder(default: AppState, builder: AlternateBuilder) -> SourceHost {
        SourceHost::with_ttl_and_builder(
            default,
            test_switch(),
            None,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            builder,
        )
    }

    /// Drive the runtime until the alternate reaches `want`, letting the
    /// spawned build task make progress on each await. Bounded so a stuck
    /// build fails the test instead of hanging.
    async fn wait_for_status(host: &SourceHost, id: &str, headers: &HeaderMap, want: &str) {
        for _ in 0..300 {
            if host.status(id, headers).expect("status").status == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("alternate {id:?} never reached status {want:?}");
    }

    /// (a) Selecting a slow alternate never awaits the build: the first
    /// request reports Building immediately, `/status` shows building with a
    /// nonzero elapsed, and once the build finishes it serves and resolves.
    #[tokio::test]
    async fn slow_alternate_reports_building_then_serving() {
        let default = default_state().await;
        let served = default.clone();
        let builder: AlternateBuilder = Arc::new(move |request: BuildRequest| -> BuildFuture {
            let served = served.clone();
            Box::pin(async move {
                let stage = request.progress.start("ingest", "Pulling ChEMBL");
                request.progress.set_progress(stage, 0.25);
                request
                    .progress
                    .update_label(stage, "Pulling ChEMBL — page 3, 6,000 records");
                tokio::time::sleep(Duration::from_millis(300)).await;
                request.progress.finish(stage);
                Ok((served, None))
            })
        });
        let host = host_with_builder(default, builder);
        let headers = authed_headers();

        let error = host
            .select(Some("alt"), &headers)
            .await
            .expect_err("first selection reports Building, never awaits the build");
        match error {
            SourceError::Building(status) => {
                assert_eq!(status.status, "building");
                assert_eq!(status.source, "alt");
            }
            other => panic!("expected Building, got {other:?}"),
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        let report = host.status("alt", &headers).expect("status");
        assert_eq!(report.status, "building");
        assert!(
            report.elapsed_ms.expect("elapsed present while building") > 0,
            "a build in flight has measurable elapsed time"
        );

        wait_for_status(&host, "alt", &headers, "serving").await;
        let resolved = host
            .select(Some("alt"), &headers)
            .await
            .expect("a completed build serves");
        assert_eq!(resolved.alternate.as_deref(), Some("alt"));
    }

    /// (b) A failing build caches as `failed`: `/status` reports it, `select`
    /// surfaces `BuildFailed` (503 JSON at the route), and `retry` transitions
    /// back to building — while a retry mid-build is a conflict.
    #[tokio::test]
    async fn failed_alternate_reports_failed_and_retry_rebuilds() {
        let default = default_state().await;
        let builder: AlternateBuilder = Arc::new(move |request: BuildRequest| -> BuildFuture {
            Box::pin(async move {
                let stage = request.progress.start("ingest", "Pulling ChEMBL");
                tokio::time::sleep(Duration::from_millis(30)).await;
                request.progress.fail(stage, "ChEMBL endpoint returned 500");
                Err("ChEMBL endpoint returned 500".to_owned())
            })
        });
        let host = host_with_builder(default, builder);
        let headers = authed_headers();

        host.select(Some("alt"), &headers)
            .await
            .expect_err("first selection reports Building");
        wait_for_status(&host, "alt", &headers, "failed").await;

        let report = host.status("alt", &headers).expect("status");
        assert_eq!(report.status, "failed");
        assert_eq!(report.error.as_deref(), Some("ChEMBL endpoint returned 500"));

        let error = host
            .select(Some("alt"), &headers)
            .await
            .expect_err("a cached failure surfaces as BuildFailed");
        assert!(matches!(error, SourceError::BuildFailed { .. }));

        let retried = host.retry("alt", &headers).expect("retry a failed build");
        assert_eq!(retried.status, "building");
        assert_eq!(
            host.status("alt", &headers).expect("status").status,
            "building",
            "retry transitions the entry back to building"
        );
        assert!(
            matches!(host.retry("alt", &headers), Err(SourceError::Conflict(_))),
            "retrying a build already in progress is a 409 conflict"
        );
    }

    /// (c) The alternate's own build stages are readable through `progress`
    /// while it builds — the log the client polls for granular status.
    #[tokio::test]
    async fn progress_exposes_the_alternates_build_stages() {
        let default = default_state().await;
        let served = default.clone();
        let builder: AlternateBuilder = Arc::new(move |request: BuildRequest| -> BuildFuture {
            let served = served.clone();
            Box::pin(async move {
                let stage = request.progress.start("ingest", "Pulling ChEMBL");
                request.progress.set_progress(stage, 0.5);
                request.progress.info("ingest", "page 12, 6,000 records");
                tokio::time::sleep(Duration::from_millis(80)).await;
                request.progress.finish(stage);
                Ok((served, None))
            })
        });
        let host = host_with_builder(default, builder);
        let headers = authed_headers();

        host.select(Some("alt"), &headers)
            .await
            .expect_err("first selection reports Building");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let response = host.progress("alt", 0, &headers).expect("progress");
        assert!(
            response.events.iter().any(|stamped| matches!(
                &stamped.event,
                crate::progress::ProgressEvent::Start { label, .. } if label == "Pulling ChEMBL"
            )),
            "the alternate's own build stages flow through its progress log"
        );
    }
}
