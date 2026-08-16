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
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::importer_catalog::{CatalogSourceKind, ImporterCatalog, RuntimeSwitchStatus};
use crate::progress::ProgressLog;
use crate::state::AppState;

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
/// build failure → 503 with the cached message.
#[derive(Debug)]
pub enum SourceError {
    Forbidden,
    Unknown(String),
    NotRunnable(String),
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
            Self::BuildFailed(message) => (StatusCode::SERVICE_UNAVAILABLE, message.clone()),
        }
    }
}

impl IntoResponse for SourceError {
    fn into_response(self) -> Response {
        self.status_and_message().into_response()
    }
}

/// The outcome of resolving one request's source selection.
pub struct ResolvedSource {
    pub state: AppState,
    /// The alternate source being served; `None` on the deployment default.
    pub alternate: Option<String>,
}

enum AlternateSource {
    Serving {
        state: AppState,
        /// Background rescan task for this alternate; aborted on eviction.
        watcher: Option<tokio::task::JoinHandle<()>>,
    },
    /// Cached build failure: surfaced as 503 without re-attempting the build
    /// on every request. Evicted on the same idle TTL as a serving entry.
    Failed(String),
}

/// One entry in the alternates map: its state plus the last time a request
/// resolved to it, driving idle eviction.
struct AlternateEntry {
    source: AlternateSource,
    last_used: Instant,
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
    catalog: ImporterCatalog,
    switch: SwitchConfig,
    alternates: RwLock<HashMap<String, AlternateEntry>>,
    /// Serializes lazy builds so a concurrent burst builds one alternate once.
    build_lock: tokio::sync::Mutex<()>,
    idle_ttl: Duration,
    eviction_interval: Duration,
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
    pub fn new(default: AppState, switch: SwitchConfig) -> Self {
        Self::with_ttl(default, switch, ALTERNATE_IDLE_TTL, ALTERNATE_EVICTION_INTERVAL)
    }

    /// Like [`Self::new`] with an overridable idle TTL and sweep interval —
    /// exercised by tests so eviction doesn't require a real 15-minute wait.
    pub fn with_ttl(
        default: AppState,
        switch: SwitchConfig,
        idle_ttl: Duration,
        eviction_interval: Duration,
    ) -> Self {
        let host = Self {
            inner: Arc::new(SourceHostInner {
                catalog: default.inner.importer_catalog.clone(),
                default,
                switch,
                alternates: RwLock::new(HashMap::new()),
                build_lock: tokio::sync::Mutex::new(()),
                idle_ttl,
                eviction_interval,
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
                                    AlternateSource::Failed(_) => None,
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

    pub fn catalog(&self) -> &ImporterCatalog {
        &self.inner.catalog
    }

    /// Resolve a request's selection to the serving state. `requested` is the
    /// raw header/query value; `headers` carries the caller's group memberships.
    pub async fn select(
        &self,
        requested: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedSource, SourceError> {
        let requested = requested.map(str::trim).filter(|id| !id.is_empty());
        let Some(id) = requested else {
            return Ok(self.default_resolution());
        };
        // Gate closed: the header is ignored entirely — today's behavior.
        if !self.inner.switch.enabled() {
            return Ok(self.default_resolution());
        }
        // The deployment default never requires authorization; the UI may send
        // the header unconditionally once a viewer has switched back.
        if self.inner.catalog.selected() == Some(id) {
            return Ok(self.default_resolution());
        }
        if !self.inner.switch.authorize(headers) {
            return Err(SourceError::Forbidden);
        }
        let Some(definition) = self.inner.catalog.source(id) else {
            return Err(SourceError::Unknown(id.to_owned()));
        };
        if !definition.runnable() {
            return Err(SourceError::NotRunnable(id.to_owned()));
        }
        let state = self
            .ensure_serving(id)
            .await
            .map_err(SourceError::BuildFailed)?;
        Ok(ResolvedSource {
            state,
            alternate: Some(id.to_owned()),
        })
    }

    fn default_resolution(&self) -> ResolvedSource {
        ResolvedSource {
            state: self.inner.default.clone(),
            alternate: None,
        }
    }

    /// Build (or fetch) the serving state for one runnable alternate source,
    /// mirroring session-manager's `ensure_serving`. The first authorized
    /// request pays the import cost; failures are cached and replayed as 503
    /// until the entry's idle TTL evicts it (see `spawn_eviction_sweep`).
    async fn ensure_serving(&self, source_id: &str) -> Result<AppState, String> {
        if let Some(result) = self.touch_alternate(source_id) {
            return result;
        }
        let _guard = self.inner.build_lock.lock().await;
        if let Some(result) = self.touch_alternate(source_id) {
            return result;
        }
        let definition = self
            .inner
            .catalog
            .source(source_id)
            .expect("ensure_serving is only called for known sources")
            .clone();
        match build_alternate(source_id, &definition).await {
            Ok((state, watcher)) => {
                tracing::info!(source = source_id, "alternate importer source serving");
                self.write_alternates().insert(
                    source_id.to_owned(),
                    AlternateEntry {
                        source: AlternateSource::Serving {
                            state: state.clone(),
                            watcher,
                        },
                        last_used: Instant::now(),
                    },
                );
                Ok(state)
            }
            Err(message) => {
                tracing::warn!(source = source_id, error = %message, "alternate source build failed");
                self.write_alternates().insert(
                    source_id.to_owned(),
                    AlternateEntry {
                        source: AlternateSource::Failed(message.clone()),
                        last_used: Instant::now(),
                    },
                );
                Err(message)
            }
        }
    }

    /// Bumps an existing entry's last-used time and returns its cached
    /// result, or `None` if no entry exists yet.
    fn touch_alternate(&self, source_id: &str) -> Option<Result<AppState, String>> {
        let mut alternates = self.write_alternates();
        let entry = alternates.get_mut(source_id)?;
        entry.last_used = Instant::now();
        Some(match &entry.source {
            AlternateSource::Serving { state, .. } => Ok(state.clone()),
            AlternateSource::Failed(message) => Err(message.clone()),
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
/// from catalog metadata: OKF needs only root + sourceId, Obsidian only a
/// root. The entry's declared rescan interval drives its periodic full
/// rescan (0 = notifications only).
async fn build_alternate(
    source_id: &str,
    definition: &crate::importer_catalog::ImporterSourceDefinition,
) -> Result<(AppState, Option<tokio::task::JoinHandle<()>>), String> {
    let filesystem = definition
        .source
        .as_ref()
        .ok_or_else(|| format!("importer source {source_id:?} has no filesystem source"))?;
    let root = PathBuf::from(&filesystem.path);
    let importer: Box<dyn data_loader::Importer> = match definition.kind {
        CatalogSourceKind::Okf => {
            let okf_source_id = definition
                .source_id
                .clone()
                .ok_or_else(|| format!("OKF source {source_id:?} must declare sourceId"))?;
            Box::new(
                okf_importer::OkfImporter::new(root.clone(), okf_source_id)
                    .map_err(|error| error.to_string())?,
            )
        }
        CatalogSourceKind::Obsidian => Box::new(vault_links::ObsidianLoader::new(root.clone())),
        other => {
            return Err(format!(
                "importer source kind {other:?} is not constructible at runtime"
            ))
        }
    };
    // The deployment catalog is administrator-supplied trusted configuration,
    // so — exactly like main.rs — the host grants the declared capabilities.
    let grants: std::collections::HashSet<data_loader::Capability> =
        importer.descriptor().capabilities.into_iter().collect();
    let progress = Arc::new(ProgressLog::new());
    let state = crate::build_world_state(importer, grants, root, progress)
        .await
        .map_err(|error| format!("import alternate source {source_id:?}: {error}"))?;
    let watcher = crate::watcher::spawn(
        state.clone(),
        definition.filesystem_rescan_interval_seconds.unwrap_or(0),
    );
    Ok((state, watcher))
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
            .select(requested_source(parts).as_deref(), &parts.headers)
            .await
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
            source: AlternateSource::Failed("unused".to_owned()),
            last_used: Instant::now() - idle_for,
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
    fn touching_an_entry_resets_its_idle_clock() {
        let mut alternates = HashMap::new();
        alternates.insert("alt".to_owned(), entry(Duration::from_secs(120)));

        if let Some(entry) = alternates.get_mut("alt") {
            entry.last_used = Instant::now();
        }

        assert!(expired_alternate_ids(&alternates, Instant::now(), Duration::from_secs(60)).is_empty());
    }
}
