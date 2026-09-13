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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::importer_catalog::{
    CatalogSourceKind, ImporterCatalog, ImporterSourceDefinition, RuntimeSwitchStatus,
};
use crate::progress::ProgressLog;
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

    /// Whether the caller may select a non-default source. A required group
    /// of `"*"` authorizes any caller — the deployment explicitly opened
    /// runtime switching — while an unset group stays fail-closed.
    pub fn authorize(&self, headers: &HeaderMap) -> bool {
        match &self.required_group {
            Some(required) if required == "*" => true,
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
/// build in progress → 503 + `Retry-After` with a `building importer source`
/// body, build failure → 503 with the cached message.
#[derive(Debug)]
pub enum SourceError {
    Forbidden,
    Unknown(String),
    NotRunnable(String),
    /// The alternate's import build is running in the background; the caller
    /// should poll `/progress` with the same selection header and retry.
    Building(String),
    BuildFailed(String),
}

impl SourceError {
    fn status_and_message(&self) -> (StatusCode, String) {
        match self {
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "selecting a non-default importer source requires the configured group".into(),
            ),
            Self::Unknown(id) => (
                StatusCode::NOT_FOUND,
                format!("unknown importer source {id:?}"),
            ),
            Self::NotRunnable(id) => (
                StatusCode::BAD_REQUEST,
                format!("importer source {id:?} is not runnable at runtime"),
            ),
            Self::Building(id) => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "building importer source {id:?}: the import is running in the background; \
                     poll /progress with the same source header for stages, then retry"
                ),
            ),
            Self::BuildFailed(message) => (StatusCode::SERVICE_UNAVAILABLE, message.clone()),
        }
    }
}

impl IntoResponse for SourceError {
    fn into_response(self) -> Response {
        let (status, message) = self.status_and_message();
        if matches!(self, Self::Building(_)) {
            return (
                status,
                [("retry-after", "2"), ("content-type", "text/plain; charset=utf-8")],
                message,
            )
                .into_response();
        }
        (status, message).into_response()
    }
}

/// Internal tri-state of `ensure_serving`: the cached serving state, an
/// in-flight build (caller surfaces [`SourceError::Building`]), or a cached
/// failure (caller surfaces [`SourceError::BuildFailed`]).
enum EnsureError {
    Building,
    Failed(String),
}

/// The outcome of resolving one request's source selection.
pub struct ResolvedSource {
    pub state: AppState,
    /// The alternate source being served; `None` on the deployment default.
    pub alternate: Option<String>,
}

enum AlternateSource {
    /// Import build running in the background. Never idle-evicted — a build
    /// that outlived the TTL would race its own eviction; the completion
    /// handler resets `last_used` and the TTL applies from there.
    Building,
    Serving {
        state: AppState,
        /// Background rescan task for this alternate; aborted on eviction.
        watcher: Option<tokio::task::JoinHandle<()>>,
    },
    /// Cached build failure: surfaced as 503 without re-attempting the build
    /// on every request. Evicted on the same idle TTL as a serving entry.
    /// The build's progress log is kept on the entry for post-mortem reads.
    Failed(String),
}

/// One entry in the alternates map: its state plus how recently a request
/// resolved to it — `last_used` drives idle eviction, `gate` drives the
/// watcher's periodic rescan. `progress` is created when the build starts
/// and survives into `Serving` and `Failed` so `/progress` can always serve
/// the source's live or final event log. `build` holds the in-flight build
/// task (abortable on invalidation); inert once the build completes.
struct AlternateEntry {
    source: AlternateSource,
    last_used: Instant,
    gate: Arc<RescanGate>,
    progress: Arc<ProgressLog>,
    build: Option<tokio::task::JoinHandle<()>>,
}

/// Record a request resolving to this entry. Both signals move together: an
/// entry kept resident by traffic must also keep rescanning, and one that
/// stops being requested must stop rebuilding before the sweep evicts it.
/// Inert on `Building`/`Failed` entries, which have no rescan watcher.
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
    /// Serializes lazy builds so a concurrent burst builds one alternate once.
    build_lock: tokio::sync::Mutex<()>,
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
/// `Building` entries are exempt — a build in flight must not race its own
/// eviction; the completion handler resets `last_used` and the TTL applies
/// from there. Pure and side-effect-free so the exact selection logic is
/// unit-testable without a real `AppState` or waiting on real timers.
fn expired_alternate_ids(
    alternates: &HashMap<String, AlternateEntry>,
    now: Instant,
    idle_ttl: Duration,
) -> Vec<String> {
    alternates
        .iter()
        .filter(|(_, entry)| {
            !matches!(entry.source, AlternateSource::Building { .. })
                && now.duration_since(entry.last_used) >= idle_ttl
        })
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
        let host = Self {
            inner: Arc::new(SourceHostInner {
                catalog: ArcSwap::from_pointee(default.inner.importer_catalog.clone()),
                default,
                switch,
                alternates: RwLock::new(HashMap::new()),
                build_lock: tokio::sync::Mutex::new(()),
                idle_ttl,
                eviction_interval,
                packages_dir,
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
                let expired: Vec<(String, Vec<tokio::task::JoinHandle<()>>)> = {
                    let mut alternates = inner
                        .alternates
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let expired_ids = expired_alternate_ids(&alternates, Instant::now(), idle_ttl);
                    expired_ids
                        .into_iter()
                        .filter_map(|id| {
                            // Building entries are never selected for
                            // eviction; treat an unexpected one as abortable
                            // rather than leaking its build.
                            alternates.remove(&id).map(|entry| {
                                let mut tasks: Vec<tokio::task::JoinHandle<()>> =
                                    entry.build.into_iter().collect();
                                if let AlternateSource::Serving {
                                    watcher: Some(watcher),
                                    ..
                                } = entry.source
                                {
                                    tasks.push(watcher);
                                }
                                (id, tasks)
                            })
                        })
                        .collect()
                };
                for (source_id, tasks) in expired {
                    for handle in tasks {
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

    pub fn invalidate_alternate(&self, source_id: &str) {
        let removed = self.write_alternates().remove(source_id);
        if let Some(entry) = removed {
            // A build in flight is aborted — the caller changed the
            // definition out from under it — and a serving entry's rescan
            // watcher stops with it.
            if let Some(handle) = entry.build {
                handle.abort();
            }
            if let AlternateSource::Serving {
                watcher: Some(handle),
                ..
            } = entry.source
            {
                handle.abort();
            }
        }
    }

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

    /// Replace one httpjson catalog source's instance variables (`PUT
    /// /importers/:id/variables`), swapping in a fresh catalog snapshot the
    /// same way [`Self::add_runtime_source`] publishes an addition. Callers
    /// pair this with [`Self::invalidate_alternate`] so a running alternate
    /// rebuilds lazily with the new variables on its next request.
    pub fn set_source_variables(
        &self,
        id: &str,
        variables: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut outcome = Ok(());
        self.inner.catalog.rcu(|current| {
            let mut next = ImporterCatalog::clone(current);
            outcome = next.set_source_variables(id, variables.clone());
            next
        });
        outcome
    }

    /// Resolve a request's selection to the serving state. `requested` is the
    /// raw header/query value; `headers` carries the caller's group memberships.
    pub async fn select(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedSource, SourceError> {
        // Starts the build if none is cached/running and reports in-flight
        // builds as a retryable 503 rather than awaiting them.
        match self.authorize_selection(requested, headers)? {
            None => Ok(self.default_resolution()),
            Some(id) => match self.ensure_serving(&id).await {
                Ok(state) => Ok(ResolvedSource {
                    state,
                    alternate: Some(id),
                }),
                Err(EnsureError::Building) => Err(SourceError::Building(id)),
                Err(EnsureError::Failed(message)) => Err(SourceError::BuildFailed(message)),
            },
        }
    }

    /// Gate + catalog validation shared by every selection consumer: `None`
    /// means serve the deployment default; `Some(id)` names an alternate the
    /// caller is authorized to select. Never builds — write/compute rejection
    /// uses this to 400 an alternate selection regardless of build state.
    fn authorize_selection(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<Option<String>, SourceError> {
        let requested = requested.map(str::trim).filter(|id| !id.is_empty());
        let Some(id) = requested else {
            return Ok(None);
        };
        // Gate closed: the header is ignored entirely.
        if !self.inner.switch.enabled() {
            return Ok(None);
        }
        // The deployment default never requires authorization; the UI may
        // send the header unconditionally once a viewer has switched back.
        let catalog = self.inner.catalog.load();
        if catalog.selected() == Some(id) {
            return Ok(None);
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
        Ok(Some(id.to_owned()))
    }

    /// Serve a source's progress event log without ever waiting on a build:
    /// the default log for no selection, otherwise the selected source's
    /// entry log — live while building, retained on failure. A selected
    /// source with no entry yet starts its build (same lazy trigger as any
    /// other selected request) and immediately serves the fresh log.
    pub async fn progress_state(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<Arc<ProgressLog>, SourceError> {
        let requested = requested.map(str::trim).filter(|id| !id.is_empty());
        match self.authorize_selection(requested, headers)? {
            None => Ok(Arc::clone(&self.inner.default.inner.progress)),
            Some(id) => {
                let _ = self.ensure_serving(&id).await;
                Ok(self
                    .write_alternates()
                    .get_mut(&id)
                    .map(|entry| {
                        mark_entry_used(entry, Instant::now());
                        Arc::clone(&entry.progress)
                    })
                    .expect("ensure_serving leaves an entry for every runnable source"))
            }
        }
    }
    fn default_resolution(&self) -> ResolvedSource {
        ResolvedSource {
            state: self.inner.default.clone(),
            alternate: None,
        }
    }

    /// Fetch the serving state for one runnable alternate source, or start
    /// its build in the background. The first authorized request pays the
    /// import cost — but asynchronously: a `Building` entry is inserted
    /// before the task spawns (so the task can always find its entry), the
    /// task re-checks the entry under `build_lock` (so concurrent starters
    /// build once), and the caller gets [`EnsureError::Building`] to surface
    /// as a retryable 503. Failures are cached on the entry and replayed
    /// until the idle TTL evicts it (see `spawn_eviction_sweep`).
    async fn ensure_serving(&self, source_id: &str) -> Result<AppState, EnsureError> {
        if let Some(result) = self.touch_alternate(source_id) {
            return result;
        }
        // Reserve the entry under the write lock so exactly one concurrent
        // starter inserts; latecomers observe the reservation as Building.
        {
            let mut alternates = self.write_alternates();
            if let Some(entry) = alternates.get_mut(source_id) {
                mark_entry_used(entry, Instant::now());
                return match &entry.source {
                    AlternateSource::Building => Err(EnsureError::Building),
                    AlternateSource::Serving { state, .. } => Ok(state.clone()),
                    AlternateSource::Failed(message) => {
                        Err(EnsureError::Failed(message.clone()))
                    }
                };
            }
            alternates.insert(
                source_id.to_owned(),
                AlternateEntry {
                    source: AlternateSource::Building,
                    last_used: Instant::now(),
                    gate: Arc::new(RescanGate::default()),
                    progress: Arc::new(ProgressLog::new()),
                    build: None,
                },
            );
        }
        let entry_progress = {
            let alternates = self.inner
                .alternates
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = alternates
                .get(source_id)
                .expect("entry inserted above in this call");
            (Arc::clone(&entry.progress), Arc::clone(&entry.gate))
        };
        let (progress, gate) = entry_progress;
        let inner = Arc::clone(&self.inner);
        let id = source_id.to_owned();
        let handle = tokio::spawn(async move {
            let _guard = inner.build_lock.lock().await;
            // Another starter's task may have finished this build while we
            // waited on the lock — never build the same source twice. An
            // absent entry means invalidation removed us; stand down.
            {
                let alternates = inner
                    .alternates
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match alternates.get(&id).map(|entry| &entry.source) {
                    Some(AlternateSource::Building) => {}
                    _ => return,
                }
            }
            let definition = inner
                .catalog
                .load()
                .source(&id)
                .cloned()
                .expect("build task runs for a catalog source");
            let outcome = build_alternate(
                &id,
                &definition,
                inner.packages_dir.as_deref(),
                gate,
                progress,
            )
            .await;
            let mut alternates = inner
                .alternates
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = alternates.get_mut(&id) {
                // The entry may have been invalidated and replaced by a newer
                // Building entry while we built; only a still-Building entry
                // is ours to complete.
                if matches!(entry.source, AlternateSource::Building) {
                    match outcome {
                        Ok((state, watcher)) => {
                            tracing::info!(source = %id, "alternate importer source serving");
                            entry.source = AlternateSource::Serving { state, watcher };
                            entry.last_used = Instant::now();
                        }
                        Err(message) => {
                            tracing::warn!(source = %id, error = %message, "alternate source build failed");
                            entry.source = AlternateSource::Failed(message);
                            entry.last_used = Instant::now();
                        }
                    }
                }
            }
        });
        // Record the task for abort-on-invalidate. If it already completed,
        // the entry is no longer Building and the finished handle is inert.
        if let Some(entry) = self.write_alternates().get_mut(source_id) {
            if matches!(entry.source, AlternateSource::Building) {
                entry.build = Some(handle);
            }
        }
        Err(EnsureError::Building)
    }

    /// Bumps an existing entry's last-used time, marks its rescan gate, and
    /// returns its cached state. The gate is the alternate watcher's only
    /// "recently requested" signal: a periodic tick with no touch since the
    /// previous one rebuilds nothing.
    fn touch_alternate(&self, source_id: &str) -> Option<Result<AppState, EnsureError>> {
        let mut alternates = self.write_alternates();
        let entry = alternates.get_mut(source_id)?;
        mark_entry_used(entry, Instant::now());
        Some(match &entry.source {
            AlternateSource::Building { .. } => Err(EnsureError::Building),
            AlternateSource::Serving { state, .. } => Ok(state.clone()),
            AlternateSource::Failed(message) => Err(EnsureError::Failed(message.clone())),
        })
    }

    fn write_alternates(&self) -> RwLockWriteGuard<'_, HashMap<String, AlternateEntry>> {
        self.inner
            .alternates
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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
            let state =
                crate::build_world_state(importer, grants, root, Arc::clone(&progress))
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
            let state =
                crate::build_world_state(importer, grants, root, Arc::clone(&progress))
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
        // Reject an alternate selection without building it: the write
        // refusal holds whatever the alternate's build state is.
        match host
            .authorize_selection(requested_source(parts).as_deref(), &parts.headers)
        {
            Err(error) => Err(error.into_response()),
            Ok(Some(id)) => Err((
                StatusCode::BAD_REQUEST,
                format!("writes and compute stay on the deployment default source; {id:?} is a read-only view"),
            )
                .into_response()),
            Ok(None) => Ok(Self(host.default_state().clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Failed` entries need no `AppState`, so the idle-detection predicate
    fn entry(idle_for: Duration) -> AlternateEntry {
        AlternateEntry {
            source: AlternateSource::Failed("unused".to_owned()),
            last_used: Instant::now() - idle_for,
            gate: Arc::new(RescanGate::default()),
            progress: Arc::new(ProgressLog::new()),
            build: None,
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
    /// `ensure_serving` relies on via `touch_alternate`.
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

    /// A build in flight must never race its own eviction, however long it
    /// has been running.
    #[test]
    fn building_entries_are_exempt_from_idle_eviction() {
        let ttl = Duration::from_secs(60);
        let mut alternates = HashMap::new();
        let mut building = entry(Duration::from_secs(3600));
        building.source = AlternateSource::Building;
        alternates.insert("building".to_owned(), building);
        alternates.insert("idle".to_owned(), entry(Duration::from_secs(120)));

        let expired = expired_alternate_ids(&alternates, Instant::now(), ttl);

        assert_eq!(expired, vec!["idle".to_owned()]);
    }

    /// `"*"` opens runtime switching to every caller; an unset group stays
    /// fail-closed.
    #[test]
    fn switch_config_wildcard_authorizes_any_caller() {
        use axum::http::HeaderMap;

        let headers = HeaderMap::new(); // no group header at all
        assert!(SwitchConfig::new(Some("*".to_owned()), "x-groups").authorize(&headers));
        assert!(!SwitchConfig::new(None, "x-groups").authorize(&headers));
        assert!(!SwitchConfig::new(Some("ops".to_owned()), "x-groups").authorize(&headers));
    }

    /// The retryable wire shape: 503, `Retry-After: 2`, and a body the
    /// client can pattern-match as an in-flight build.
    #[test]
    fn building_error_maps_to_retryable_503() {
        let (status, message) =
            SourceError::Building("hindsight-memory-bank".to_owned()).status_and_message();
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(message.starts_with("building importer source"));

        let response = SourceError::Building("hindsight-memory-bank".to_owned()).into_response();
        assert_eq!(response.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("2")
        );
    }
}
