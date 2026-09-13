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
    CatalogSourceKind, ImporterCatalog, ImporterParameterDiscover, ImporterSourceDefinition,
    RuntimeSwitchStatus,
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
    /// The selection string is malformed or carries a parameter the source
    /// does not accept — a client error mapped to 400.
    BadSelection(String),
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
            Self::BadSelection(message) => (StatusCode::BAD_REQUEST, message).into_response(),
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

/// A parsed source selection: a catalog source id plus optional per-request
/// parameter values. The canonical string form — `id`, or
/// `id?k1=v1&k2=v2` with keys sorted and both key and value percent-encoded —
/// is the single wire token shared by the `x-jump-cannon-source` header, the
/// layout WS `?source=`, `sessionStorage['jc_source_id']`, and the key of the
/// alternates map. The frontend mirrors this exact grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelector {
    pub id: String,
    pub params: BTreeMap<String, String>,
}

impl SourceSelector {
    /// Parse a canonical selection string. Rejects an empty id, a `?` with no
    /// parameters, a parameter that is not `key=value`, an empty parameter
    /// name, a repeated parameter, and invalid percent-encoding. It does NOT
    /// check parameter names against a catalog definition — that is
    /// [`validate_selection_params`], which needs the source's declared
    /// parameters.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("empty source selection".to_owned());
        }
        let (id_part, query) = match raw.split_once('?') {
            Some((id, query)) => (id, Some(query)),
            None => (raw, None),
        };
        let id = decode_component(id_part)?;
        if id.is_empty() {
            return Err("source selection has an empty id".to_owned());
        }
        let mut params = BTreeMap::new();
        if let Some(query) = query {
            if query.is_empty() {
                return Err("source selection has a '?' with no parameters".to_owned());
            }
            for pair in query.split('&') {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("source selection parameter {pair:?} is not key=value"))?;
                let key = decode_component(key)?;
                let value = decode_component(value)?;
                if key.is_empty() {
                    return Err("source selection has a parameter with an empty name".to_owned());
                }
                if params.insert(key.clone(), value).is_some() {
                    return Err(format!("source selection repeats parameter {key:?}"));
                }
            }
        }
        Ok(Self { id, params })
    }

    /// The canonical parameter string (`k1=v1&k2=v2`, sorted by key, both
    /// sides percent-encoded); empty when there are no parameters.
    fn params_canonical(&self) -> String {
        self.params
            .iter()
            .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
            .collect::<Vec<_>>()
            .join("&")
    }
}

impl std::fmt::Display for SourceSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", encode_component(&self.id))?;
        if !self.params.is_empty() {
            write!(f, "?{}", self.params_canonical())?;
        }
        Ok(())
    }
}

fn decode_component(raw: &str) -> Result<String, String> {
    urlencoding::decode(raw)
        .map(std::borrow::Cow::into_owned)
        .map_err(|error| format!("invalid percent-encoding {raw:?} in source selection: {error}"))
}

fn encode_component(raw: &str) -> String {
    urlencoding::encode(raw).into_owned()
}

/// Outcome of gating a `/status`, `/progress`, or `/retry` request: the
/// request either targets the always-serving deployment default or an
/// authorized, runnable alternate (carrying the parsed selection).
enum GateOutcome {
    Default,
    Alternate(SourceSelector),
}

/// Which source a request resolves to, before any build is attempted.
enum Selection {
    Default,
    Alternate(SourceSelector),
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

/// How long a computed parameter report (including any live discovery) is
/// reused before recomputation. Discovery hits the bound API, so a short cache
/// keeps the picker responsive without hammering the upstream on every open.
const PARAMETERS_CACHE_TTL: Duration = Duration::from_secs(60);
/// Wall-clock ceiling on one parameter's live discovery request.
const DISCOVERY_TIMEOUT_SECONDS: u64 = 10;
/// Largest number of discovered values retained from one listing.
const DISCOVERY_MAX_ITEMS: usize = 1000;

/// One selectable value for a parameter: its wire id and a human label.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParameterValue {
    pub id: String,
    pub label: String,
}

/// One parameter's resolved picker: its label, pre-selected default, the
/// available values (discovered first, then static, deduped by id), whether
/// discovery ran successfully, and any discovery error (values then fall back
/// to the static list).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParameterReport {
    pub label: String,
    pub default: Option<String>,
    pub values: Vec<ParameterValue>,
    pub discovered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET /importers/sources/:id/parameters` body: the source id and its
/// per-parameter pickers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParametersReport {
    pub source: String,
    pub parameters: BTreeMap<String, ParameterReport>,
}

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
    /// Per-source-id cache of computed parameter reports, keyed by catalog id.
    /// Each entry is reused for [`PARAMETERS_CACHE_TTL`] before recomputation.
    parameters_cache: RwLock<HashMap<String, (Instant, ParametersReport)>>,
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
                parameters_cache: RwLock::new(HashMap::new()),
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
            Selection::Alternate(selector) => {
                let key = selector.to_string();
                // First touch of an unbuilt alternate validates parameter
                // coverage against the package (400 on a misconfigured source)
                // before the build spawns; an already-resident entry skips the
                // reload and resolves straight through `ensure_serving`.
                if !self.read_alternates().contains_key(&key) {
                    self.validate_parameter_coverage(&selector).await?;
                }
                let state = self.ensure_serving(&selector)?;
                Ok(ResolvedSource {
                    state,
                    alternate: Some(key),
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
            Selection::Alternate(selector) => Ok(ResolvedSource {
                state: self.inner.default.clone(),
                alternate: Some(selector.to_string()),
            }),
        }
    }

    /// The shared selection front matter for [`Self::select`] and
    /// [`Self::resolve_default`]: parse the raw header/query value and gate it
    /// to either the deployment default or a specific authorized, runnable
    /// alternate, without building anything. A closed gate ignores the
    /// selection entirely; the default id with no parameters is the default.
    fn resolve_selection(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<Selection, SourceError> {
        let requested = requested.map(str::trim).filter(|id| !id.is_empty());
        let Some(raw) = requested else {
            return Ok(Selection::Default);
        };
        // Gate closed: the header is ignored entirely — today's behavior.
        if !self.inner.switch.enabled() {
            return Ok(Selection::Default);
        }
        let selector = SourceSelector::parse(raw).map_err(SourceError::BadSelection)?;
        // The deployment default (with no parameters) never requires
        // authorization; the UI may send the header unconditionally once a
        // viewer has switched back.
        let catalog = self.inner.catalog.load();
        if catalog.selected() == Some(selector.id.as_str()) && selector.params.is_empty() {
            return Ok(Selection::Default);
        }
        if !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = catalog.source(&selector.id) else {
            return Err(SourceError::Unknown(selector.id.clone()));
        };
        if !definition.runnable() {
            return Err(SourceError::NotRunnable(selector.id.clone()));
        }
        validate_selection_params(&selector, definition).map_err(SourceError::BadSelection)?;
        Ok(Selection::Alternate(selector))
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
    fn ensure_serving(&self, selector: &SourceSelector) -> Result<AppState, SourceError> {
        let key = selector.to_string();
        let mut alternates = self.write_alternates();
        if let Some(entry) = alternates.get_mut(&key) {
            mark_entry_used(entry, Instant::now());
            return match &entry.source {
                AlternateSource::Serving { state, .. } => Ok(state.clone()),
                AlternateSource::Building {
                    progress, started, ..
                } => Err(SourceError::Building(build_status(&key, progress, *started))),
                AlternateSource::Failed { message, .. } => Err(SourceError::BuildFailed {
                    source: key.clone(),
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
            selector.clone(),
            generation,
            Arc::clone(&progress),
            Arc::clone(&gate),
        );
        let status = build_status(&key, &progress, started);
        alternates.insert(
            key,
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
    /// still the exact build it started (see [`finalize_build`]). The entry is
    /// keyed by the canonical selection string; the definition is looked up by
    /// the selection's catalog id.
    fn spawn_build(
        &self,
        selector: SourceSelector,
        generation: u64,
        progress: Arc<ProgressLog>,
        gate: Arc<RescanGate>,
    ) -> tokio::task::JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        let builder = Arc::clone(&self.inner.builder);
        let key = selector.to_string();
        tokio::spawn(async move {
            let Some(definition) = inner.catalog.load().source(&selector.id).cloned() else {
                // The source vanished from the catalog between selection and
                // build (invalidated or removed). Record it as a retryable
                // failure rather than silently dropping the entry.
                finalize_build(
                    &inner,
                    &key,
                    generation,
                    Err(format!("unknown importer source {:?}", selector.id)),
                    progress,
                    gate,
                );
                return;
            };
            let request = BuildRequest {
                key: key.clone(),
                selector: selector.clone(),
                definition,
                packages_dir: inner.packages_dir.clone(),
                gate: Arc::clone(&gate),
                progress: Arc::clone(&progress),
            };
            let result = (*builder)(request).await;
            finalize_build(&inner, &key, generation, result, progress, gate);
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
            GateOutcome::Alternate(selector) => {
                let key = selector.to_string();
                let alternates = self.read_alternates();
                Ok(match alternates.get(&key) {
                    None => BuildStatusReport::idle(&key),
                    Some(entry) => match &entry.source {
                        AlternateSource::Building {
                            progress, started, ..
                        } => BuildStatusReport::building(&key, progress, *started),
                        AlternateSource::Serving { .. } => BuildStatusReport::serving(&key),
                        AlternateSource::Failed { message, .. } => {
                            BuildStatusReport::failed(&key, message)
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
        match self.gate_alternate(source_id, headers)? {
            GateOutcome::Default => Ok(self.inner.default.inner.progress.since(since)),
            GateOutcome::Alternate(selector) => {
                let key = selector.to_string();
                let alternates = self.read_alternates();
                Ok(match alternates.get(&key) {
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
            GateOutcome::Alternate(selector) => {
                let key = selector.to_string();
                let mut alternates = self.write_alternates();
                if let Some(entry) = alternates.get(&key) {
                    match &entry.source {
                        AlternateSource::Building { .. } => {
                            return Err(SourceError::Conflict(format!(
                                "importer source {key:?} is already building"
                            )));
                        }
                        AlternateSource::Serving { .. } => {
                            return Err(SourceError::Conflict(format!(
                                "importer source {key:?} is already serving"
                            )));
                        }
                        AlternateSource::Failed { finished, .. } => {
                            tracing::info!(
                                source = %key,
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
                    selector.clone(),
                    generation,
                    Arc::clone(&progress),
                    Arc::clone(&gate),
                );
                let report = BuildStatusReport::building(&key, &progress, started);
                alternates.insert(
                    key,
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
    /// default (with no parameters) is always allowed; any other selection
    /// requires the configured group and must name a known, runnable catalog
    /// source with only that source's declared parameters.
    fn gate_alternate(
        &self,
        source_id: &str,
        headers: &HeaderMap,
    ) -> Result<GateOutcome, SourceError> {
        let id = source_id.trim();
        if id.is_empty() {
            return Ok(GateOutcome::Default);
        }
        let selector = SourceSelector::parse(id).map_err(SourceError::BadSelection)?;
        let catalog = self.inner.catalog.load();
        if catalog.selected() == Some(selector.id.as_str()) && selector.params.is_empty() {
            return Ok(GateOutcome::Default);
        }
        if !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = catalog.source(&selector.id) else {
            return Err(SourceError::Unknown(selector.id.clone()));
        };
        if !definition.runnable() {
            return Err(SourceError::NotRunnable(selector.id.clone()));
        }
        validate_selection_params(&selector, definition).map_err(SourceError::BadSelection)?;
        Ok(GateOutcome::Alternate(selector))
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

    fn read_parameters_cache(
        &self,
    ) -> RwLockReadGuard<'_, HashMap<String, (Instant, ParametersReport)>> {
        self.inner
            .parameters_cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_parameters_cache(
        &self,
    ) -> RwLockWriteGuard<'_, HashMap<String, (Instant, ParametersReport)>> {
        self.inner
            .parameters_cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resolve one source's parameter pickers, running any live discovery
    /// (bounded: [`DISCOVERY_TIMEOUT_SECONDS`], [`DISCOVERY_MAX_ITEMS`]) and
    /// caching the result per catalog id for [`PARAMETERS_CACHE_TTL`]. Gated
    /// like `/status`: the deployment default is always allowed; any other id
    /// requires the configured group and must name a known source (404/403).
    pub async fn parameters(
        &self,
        source_id: &str,
        headers: &HeaderMap,
    ) -> Result<ParametersReport, SourceError> {
        let selector = SourceSelector::parse(source_id.trim()).map_err(SourceError::BadSelection)?;
        let catalog = self.inner.catalog.load();
        let is_default = catalog.selected() == Some(selector.id.as_str());
        if !is_default && !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = catalog.source(&selector.id) else {
            return Err(SourceError::Unknown(selector.id.clone()));
        };
        let definition = definition.clone();
        drop(catalog);
        {
            let cache = self.read_parameters_cache();
            if let Some((at, report)) = cache.get(&selector.id) {
                if at.elapsed() < PARAMETERS_CACHE_TTL {
                    return Ok(report.clone());
                }
            }
        }
        let report = self.compute_parameters(&selector.id, &definition).await;
        self.write_parameters_cache()
            .insert(selector.id.clone(), (Instant::now(), report.clone()));
        Ok(report)
    }

    /// Build the per-parameter report for one source. Each parameter with a
    /// `discover` block is listed live; the static `values` are appended
    /// (deduped by id). Discovery failures fall back to the static values with
    /// the error surfaced.
    async fn compute_parameters(
        &self,
        source_id: &str,
        definition: &ImporterSourceDefinition,
    ) -> ParametersReport {
        let mut parameters = BTreeMap::new();
        for (name, parameter) in &definition.parameters {
            let mut values: Vec<ParameterValue> = Vec::new();
            let mut discovered = false;
            let mut error = None;
            if let Some(discover) = &parameter.discover {
                match self.discover_values(source_id, definition, discover).await {
                    Ok(found) => {
                        discovered = true;
                        values = found;
                    }
                    Err(message) => error = Some(message),
                }
            }
            let mut seen: std::collections::HashSet<String> =
                values.iter().map(|value| value.id.clone()).collect();
            for value in &parameter.values {
                if seen.insert(value.clone()) {
                    values.push(ParameterValue {
                        id: value.clone(),
                        label: value.clone(),
                    });
                }
            }
            parameters.insert(
                name.clone(),
                ParameterReport {
                    label: parameter.label.clone(),
                    default: parameter.default.clone(),
                    values,
                    discovered,
                    error,
                },
            );
        }
        ParametersReport {
            source: source_id.to_owned(),
            parameters,
        }
    }

    /// Run one parameter's live discovery against the bound httpjson API,
    /// exactly as `build_alternate` would build the instance (endpoint +
    /// variables + token), through the json engine's own reqwest transport.
    /// Bounded by [`DISCOVERY_TIMEOUT_SECONDS`] and [`DISCOVERY_MAX_ITEMS`].
    async fn discover_values(
        &self,
        source_id: &str,
        definition: &ImporterSourceDefinition,
        discover: &ImporterParameterDiscover,
    ) -> Result<Vec<ParameterValue>, String> {
        let http_json = definition
            .http_json
            .as_ref()
            .ok_or_else(|| "discovery requires an httpJson binding".to_owned())?;
        let packages_dir = self
            .inner
            .packages_dir
            .clone()
            .ok_or_else(|| "importer packages directory is not configured".to_owned())?;
        let manifest_path = packages_dir.join(&http_json.package);
        let package = tokio::task::spawn_blocking(move || {
            crate::importer_package::load_importer_package(&manifest_path)
        })
        .await
        .map_err(|error| format!("package load task failed: {error}"))??
        .0;
        let json_config = package.json_config().map_err(|error| error.to_string())?;
        // Template variables: package defaults, overridden by the binding's
        // static variables, overridden by parameter defaults. The variable being
        // discovered is deliberately absent from the discovery path.
        let mut variables = BTreeMap::new();
        for spec in &json_config.variables {
            if let Some(default) = &spec.default {
                variables.insert(spec.name.clone(), default.clone());
            }
        }
        for (key, value) in &http_json.variables {
            variables.insert(key.clone(), value.clone());
        }
        for (name, parameter) in &definition.parameters {
            if let Some(default) = &parameter.default {
                variables.insert(name.clone(), default.clone());
            }
        }
        let path = expand_template(&discover.path, &variables)?;
        let url = format!("{}{}", http_json.endpoint.trim_end_matches('/'), path);
        let token = http_json
            .token_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok());
        let instance = importer::InstanceConfig {
            source_id: sanitize_source_id(source_id),
            base_url: http_json.endpoint.clone(),
            variables,
            token,
            poll_interval_ms: 0,
        };
        let transport =
            importer::json::ReqwestTransport::new(&instance, package.limits(), DISCOVERY_TIMEOUT_SECONDS)
                .map_err(|error| error.to_string())?;
        let bytes = tokio::time::timeout(
            Duration::from_secs(DISCOVERY_TIMEOUT_SECONDS),
            importer::json::JsonTransport::get(&transport, &url),
        )
        .await
        .map_err(|_| format!("discovery timed out after {DISCOVERY_TIMEOUT_SECONDS}s"))?
        .map_err(|error| error.to_string())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("discovery response is not JSON: {error}"))?;
        let items = value
            .pointer(&discover.items_pointer)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("discovery response has no array at {}", discover.items_pointer))?;
        let mut out = Vec::new();
        for item in items.iter().take(DISCOVERY_MAX_ITEMS) {
            let id = match item.pointer(&discover.id_pointer) {
                Some(serde_json::Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => continue,
            };
            let label = discover
                .label_pointer
                .as_ref()
                .and_then(|pointer| item.pointer(pointer))
                .and_then(|value| match value {
                    serde_json::Value::String(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| id.clone());
            out.push(ParameterValue { id, label });
        }
        Ok(out)
    }

    /// Validate that every package variable the bound package requires (no
    /// package default) is covered by the binding's static variables or a
    /// declared parameter, and that every declared parameter names a real
    /// package variable. Loads the package on first touch; surfaced as a 400
    /// so a misconfigured catalog entry fails loudly rather than as an opaque
    /// 503 build failure. Sources with no package binding are trivially valid.
    async fn validate_parameter_coverage(
        &self,
        selector: &SourceSelector,
    ) -> Result<(), SourceError> {
        let catalog = self.inner.catalog.load();
        let Some(definition) = catalog.source(&selector.id) else {
            return Err(SourceError::Unknown(selector.id.clone()));
        };
        let (package_file, bound_variables) = match definition.kind {
            CatalogSourceKind::HttpJson => match &definition.http_json {
                Some(binding) => (binding.package.clone(), binding.variables.clone()),
                None => return Ok(()),
            },
            CatalogSourceKind::Tvix => match &definition.tvix {
                Some(binding) => (binding.package.clone(), binding.variables.clone()),
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        let parameters: BTreeMap<String, ()> =
            definition.parameters.keys().map(|k| (k.clone(), ())).collect();
        let id = selector.id.clone();
        drop(catalog);
        let Some(packages_dir) = self.inner.packages_dir.clone() else {
            // No packages dir: the build path itself reports the clear
            // deployment error; nothing to validate here.
            return Ok(());
        };
        let manifest_path = packages_dir.join(&package_file);
        let package = tokio::task::spawn_blocking(move || {
            crate::importer_package::load_importer_package(&manifest_path)
        })
        .await
        .map_err(|error| SourceError::BadSelection(format!("package load task failed: {error}")))?
        .map_err(SourceError::BadSelection)?
        .0;
        let declared = declared_variables(&package);
        for (name, has_default) in &declared {
            if *has_default || bound_variables.contains_key(name) || parameters.contains_key(name) {
                continue;
            }
            return Err(SourceError::BadSelection(format!(
                "source {id:?} is misconfigured: package variable {name:?} is required (no package \
                 default) but is neither bound in variables nor declared as a parameter"
            )));
        }
        let declared_names: std::collections::HashSet<&str> =
            declared.iter().map(|(name, _)| name.as_str()).collect();
        for name in parameters.keys() {
            if !declared_names.contains(name.as_str()) {
                return Err(SourceError::BadSelection(format!(
                    "source {id:?} declares parameter {name:?}, which is not a variable of its \
                     package"
                )));
            }
        }
        Ok(())
    }
}

/// Validate a selection's parameters against a source definition without the
/// package: parameters are only accepted on a package-backed source, and every
/// supplied key must be a declared parameter of that source. (Declared-vs-
/// package-variable and required-coverage checks need the package; see
/// [`SourceHost::validate_parameter_coverage`].)
fn validate_selection_params(
    selector: &SourceSelector,
    definition: &ImporterSourceDefinition,
) -> Result<(), String> {
    if selector.params.is_empty() {
        return Ok(());
    }
    if definition.http_json.is_none() && definition.tvix.is_none() {
        return Err(format!("source {:?} does not accept parameters", selector.id));
    }
    for key in selector.params.keys() {
        if !definition.parameters.contains_key(key) {
            return Err(format!(
                "source {:?} has no parameter {key:?}; declared parameters: [{}]",
                selector.id,
                definition
                    .parameters
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// The package's declared variables as `(name, has_default)`, engine-aware so
/// coverage validation works for both httpjson and tvix packages.
fn declared_variables(package: &importer::ValidatedPackage) -> Vec<(String, bool)> {
    if let Ok(config) = package.json_config() {
        return config
            .variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.default.is_some()))
            .collect();
    }
    if let Ok(config) = package.tvix_config() {
        return config
            .variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.default.is_some()))
            .collect();
    }
    Vec::new()
}

/// The instance variables for one build: the binding's static variables,
/// overridden by parameter defaults, overridden by the selection's parameter
/// values (later wins). Package defaults for anything still unset are applied
/// downstream by the package's own `resolve_variables`.
fn merge_binding_variables(
    base: &BTreeMap<String, String>,
    definition: &ImporterSourceDefinition,
    selector: &SourceSelector,
) -> BTreeMap<String, String> {
    let mut variables = base.clone();
    for (name, parameter) in &definition.parameters {
        if let Some(default) = &parameter.default {
            variables.insert(name.clone(), default.clone());
        }
    }
    for (key, value) in &selector.params {
        variables.insert(key.clone(), value.clone());
    }
    variables
}

/// The node-namespacing `source_id` for one build. With no parameters it is
/// the declared `sourceId` (or the sanitized package id). With parameters it is
/// the declared `sourceId` (or the catalog id) suffixed by a slug of the
/// selection's parameters, so two selections of one source never collide in the
/// node namespace. `source_id` must be `[a-z0-9._-]`, so the suffix (which
/// carries `=`/`&`) is folded through [`sanitize_source_id`].
fn namespaced_source_id(
    definition: &ImporterSourceDefinition,
    selector: &SourceSelector,
    package_id: &str,
) -> String {
    if selector.params.is_empty() {
        return definition
            .source_id
            .clone()
            .unwrap_or_else(|| sanitize_source_id(package_id));
    }
    let base = definition
        .source_id
        .clone()
        .unwrap_or_else(|| selector.id.clone());
    sanitize_source_id(&format!("{base}.{}", params_slug(&selector.params)))
}

/// A deterministic `k-v.k-v` slug of the selection parameters (sorted by key),
/// used to distinguish node namespaces per parameter set.
fn params_slug(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{key}-{value}"))
        .collect::<Vec<_>>()
        .join(".")
}

/// Expand every `{name}` placeholder in a path template with the resolved
/// variables. An unterminated placeholder or an unbound variable is an error.
fn expand_template(template: &str, variables: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unterminated {{ in template {template:?}"))?;
        let name = &after[..end];
        let value = variables
            .get(name)
            .ok_or_else(|| format!("template {template:?} references unbound variable {name:?}"))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Inputs to one alternate build, bundled so the build step is a simple
/// `Fn(BuildRequest) -> Future` seam that tests replace with a fake.
struct BuildRequest {
    /// Canonical selection string; the alternates-map key and progress source.
    key: String,
    /// Parsed selection (catalog id + parameter values) driving the build.
    selector: SourceSelector,
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
                &request.key,
                &request.selector,
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
    selector: &SourceSelector,
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
            let source_id_value =
                namespaced_source_id(definition, selector, &package.manifest().metadata.id);
            let variables = merge_binding_variables(&http_json.variables, definition, selector);
            let token = http_json
                .token_env
                .as_deref()
                .and_then(|name| std::env::var(name).ok());
            let instance = importer::InstanceConfig {
                source_id: source_id_value,
                base_url: http_json.endpoint.clone(),
                variables,
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
        CatalogSourceKind::Tvix => {
            let tvix = definition
                .tvix
                .as_ref()
                .ok_or_else(|| format!("tvix source {source_id:?} has no tvix contract"))?;
            let packages_dir = packages_dir.ok_or_else(|| {
                format!(
                    "tvix source {source_id:?} cannot be served: \
                     JUMP_CANNON_IMPORTER_PACKAGES_DIR is not configured"
                )
            })?;
            let manifest_path = packages_dir.join(&tvix.package);
            let (package, _definition) = tokio::task::spawn_blocking(move || {
                crate::importer_package::load_importer_package(&manifest_path)
            })
            .await
            .map_err(|error| format!("tvix source {source_id:?}: package load task failed: {error}"))?
            .map_err(|error| format!("tvix source {source_id:?}: {error}"))?;
            let source_id_value =
                namespaced_source_id(definition, selector, &package.manifest().metadata.id);
            let variables = merge_binding_variables(&tvix.variables, definition, selector);
            // A tvix generator evaluates a Nix expression; there is no remote
            // root, so the filesystem root is empty like the httpjson arm.
            let root = PathBuf::new();
            let importer: Box<dyn data_loader::Importer> = Box::new(
                importer::build_tvix_importer(package, variables, source_id_value)
                    .map_err(|error| error.to_string())?,
            );
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
                _ => unreachable!("filesystem match arm excludes HttpJson and Tvix"),
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

/// Extract the requested selection string: the header first (carrying the
/// canonical selection verbatim), then the `source` query parameter on the
/// layout WebSocket (which cannot set headers). The query value is a single
/// percent-encoded token — it may itself contain `?`/`&`/`=` — so it is decoded
/// once here into the canonical form that [`SourceSelector::parse`] expects.
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
            return Some(
                urlencoding::decode(value)
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_else(|_| value.to_owned()),
            );
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
            data_loader::SourceKind::Pest,
            true,
        )
        .expect("switch catalog parses");
        state_from_catalog(catalog).await
    }

    /// Build a default [`AppState`] over a `TinyImporter` for an already-parsed
    /// catalog — shared by [`default_state`] and [`param_state`].
    async fn state_from_catalog(catalog: ImporterCatalog) -> AppState {
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

    /// A default [`AppState`] whose catalog declares a runnable httpjson source
    /// `banked` with a `bank` parameter, so parameterised selections gate and
    /// namespace exactly like production (the fake builder bypasses the real
    /// package load, and `packages_dir` is `None` so coverage validation is a
    /// no-op).
    async fn param_state() -> AppState {
        let catalog_json = serde_json::json!({
            "selected": "default",
            "sources": {
                "default": { "displayName": "Default", "kind": "pest" },
                "banked": {
                    "displayName": "Banked",
                    "kind": "httpjson",
                    "httpJson": {
                        "package": "banked.toml",
                        "endpoint": "http://banked.invalid",
                        "variables": {}
                    },
                    "parameters": { "bank": { "label": "Memory bank" } }
                }
            }
        })
        .to_string();
        let catalog = ImporterCatalog::parse_with_runtime_switch(
            Some(&catalog_json),
            data_loader::SourceKind::Pest,
            true,
        )
        .expect("param catalog parses");
        state_from_catalog(catalog).await
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

    /// SourceSelector round-trips its canonical form (id + sorted, encoded
    /// params) and `validate_selection_params` rejects a parameter the source
    /// does not declare.
    #[test]
    fn source_selector_round_trips_and_rejects_undeclared_params() {
        // Bare id.
        let bare = SourceSelector::parse("hindsight").expect("bare id parses");
        assert_eq!(bare.id, "hindsight");
        assert!(bare.params.is_empty());
        assert_eq!(bare.to_string(), "hindsight");

        // Params are sorted by key and survive a Display -> parse round trip.
        let selection = "hindsight?bank=omp&tenant=default";
        let parsed = SourceSelector::parse(selection).expect("parses");
        assert_eq!(parsed.id, "hindsight");
        assert_eq!(parsed.params.get("bank").map(String::as_str), Some("omp"));
        assert_eq!(parsed.params.get("tenant").map(String::as_str), Some("default"));
        assert_eq!(parsed.to_string(), selection, "canonical form is stable");
        // Unsorted input canonicalizes to sorted output.
        assert_eq!(
            SourceSelector::parse("hindsight?tenant=default&bank=omp")
                .unwrap()
                .to_string(),
            selection
        );
        // A value needing encoding round-trips through percent-encoding.
        let encoded = SourceSelector::parse("hindsight?bank=a%2Fb").expect("encoded value parses");
        assert_eq!(encoded.params.get("bank").map(String::as_str), Some("a/b"));
        assert_eq!(encoded.to_string(), "hindsight?bank=a%2Fb");

        // Malformed selections are rejected.
        assert!(SourceSelector::parse("").is_err());
        assert!(SourceSelector::parse("hindsight?").is_err());
        assert!(SourceSelector::parse("hindsight?bank").is_err());
        assert!(SourceSelector::parse("hindsight?bank=a&bank=b").is_err());

        // validate_selection_params: a declared parameter is accepted; an
        // undeclared one and any parameter on a non-package source are rejected.
        let mut parameters = BTreeMap::new();
        parameters.insert(
            "bank".to_owned(),
            crate::importer_catalog::ImporterParameter {
                label: "Memory bank".to_owned(),
                discover: None,
                values: Vec::new(),
                default: None,
            },
        );
        let http_json = ImporterSourceDefinition {
            display_name: "Banked".to_owned(),
            description: String::new(),
            kind: CatalogSourceKind::HttpJson,
            source_id: None,
            filesystem_rescan_interval_seconds: None,
            source: None,
            http_json: Some(crate::importer_catalog::ImporterHttpJsonSource {
                package: "p.toml".to_owned(),
                endpoint: "http://x.invalid".to_owned(),
                variables: BTreeMap::new(),
                token_env: None,
                poll_interval_ms: 60_000,
            }),
            tvix: None,
            producer: None,
            parameters,
        };
        validate_selection_params(&SourceSelector::parse("s?bank=omp").unwrap(), &http_json)
            .expect("declared parameter accepted");
        assert!(
            validate_selection_params(&SourceSelector::parse("s?region=us").unwrap(), &http_json)
                .is_err(),
            "an undeclared parameter is rejected"
        );

        let obsidian = ImporterSourceDefinition {
            http_json: None,
            kind: CatalogSourceKind::Obsidian,
            parameters: BTreeMap::new(),
            ..http_json.clone()
        };
        assert!(
            validate_selection_params(&SourceSelector::parse("s?bank=omp").unwrap(), &obsidian)
                .is_err(),
            "a non-package source accepts no parameters"
        );
    }

    /// (b) Two selections of one parameterised source with different `bank`
    /// values build two independent alternates keyed by their canonical
    /// selection strings, each with a distinct node namespace.
    #[tokio::test]
    async fn two_bank_selections_build_independent_namespaced_alternates() {
        let default = param_state().await;
        let served = default.clone();
        let namespaces: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&namespaces);
        let builder: AlternateBuilder = Arc::new(move |request: BuildRequest| -> BuildFuture {
            let served = served.clone();
            let captured = Arc::clone(&captured);
            Box::pin(async move {
                captured
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(namespaced_source_id(
                    &request.definition,
                    &request.selector,
                    "pkg",
                ));
                Ok((served, None))
            })
        });
        let host = host_with_builder(default, builder);
        let headers = authed_headers();

        host.select(Some("banked?bank=omp"), &headers)
            .await
            .expect_err("first omp selection reports Building");
        host.select(Some("banked?bank=jira-ithelp"), &headers)
            .await
            .expect_err("first jira selection reports Building");
        wait_for_status(&host, "banked?bank=omp", &headers, "serving").await;
        wait_for_status(&host, "banked?bank=jira-ithelp", &headers, "serving").await;

        let omp = host
            .select(Some("banked?bank=omp"), &headers)
            .await
            .expect("omp serves");
        let jira = host
            .select(Some("banked?bank=jira-ithelp"), &headers)
            .await
            .expect("jira serves");
        assert_eq!(omp.alternate.as_deref(), Some("banked?bank=omp"));
        assert_eq!(jira.alternate.as_deref(), Some("banked?bank=jira-ithelp"));

        let recorded = namespaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(recorded.len(), 2, "each selection builds exactly once");
        assert_ne!(
            recorded[0], recorded[1],
            "two banks get distinct node namespaces"
        );
        assert!(recorded.iter().any(|n| n.contains("bank-omp")));
        assert!(recorded.iter().any(|n| n.contains("bank-jira-ithelp")));
    }
}
