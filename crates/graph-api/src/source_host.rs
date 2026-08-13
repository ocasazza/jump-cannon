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
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

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
    Serving(AppState),
    /// Cached build failure: surfaced as 503 without re-attempting the build
    /// on every request. A cached entry lives until process restart.
    Failed(String),
}

struct SourceHostInner {
    default: AppState,
    catalog: ImporterCatalog,
    switch: SwitchConfig,
    alternates: RwLock<HashMap<String, AlternateSource>>,
    /// Serializes lazy builds so a concurrent burst builds one alternate once.
    build_lock: tokio::sync::Mutex<()>,
}

/// Cloneable handle owning the default serving state plus lazily-built
/// alternates. This is the axum router state for the standalone server.
#[derive(Clone)]
pub struct SourceHost {
    inner: Arc<SourceHostInner>,
}

impl SourceHost {
    pub fn new(default: AppState, switch: SwitchConfig) -> Self {
        Self {
            inner: Arc::new(SourceHostInner {
                catalog: default.inner.importer_catalog.clone(),
                default,
                switch,
                alternates: RwLock::new(HashMap::new()),
                build_lock: tokio::sync::Mutex::new(()),
            }),
        }
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
    /// request pays the import cost; failures are cached and replayed as 503.
    async fn ensure_serving(&self, source_id: &str) -> Result<AppState, String> {
        if let Some(entry) = self.read_alternates().get(source_id) {
            return match entry {
                AlternateSource::Serving(state) => Ok(state.clone()),
                AlternateSource::Failed(message) => Err(message.clone()),
            };
        }
        let _guard = self.inner.build_lock.lock().await;
        if let Some(entry) = self.read_alternates().get(source_id) {
            return match entry {
                AlternateSource::Serving(state) => Ok(state.clone()),
                AlternateSource::Failed(message) => Err(message.clone()),
            };
        }
        let definition = self
            .inner
            .catalog
            .source(source_id)
            .expect("ensure_serving is only called for known sources")
            .clone();
        match build_alternate(source_id, &definition).await {
            Ok(state) => {
                tracing::info!(source = source_id, "alternate importer source serving");
                self.write_alternates().insert(
                    source_id.to_owned(),
                    AlternateSource::Serving(state.clone()),
                );
                Ok(state)
            }
            Err(message) => {
                tracing::warn!(source = source_id, error = %message, "alternate source build failed");
                self.write_alternates().insert(
                    source_id.to_owned(),
                    AlternateSource::Failed(message.clone()),
                );
                Err(message)
            }
        }
    }

    fn read_alternates(&self) -> RwLockReadGuard<'_, HashMap<String, AlternateSource>> {
        self.inner
            .alternates
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_alternates(&self) -> RwLockWriteGuard<'_, HashMap<String, AlternateSource>> {
        self.inner
            .alternates
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Construct one alternate serving state from its catalog entry. Runnable
/// kinds are exactly the filesystem kinds constructible from catalog metadata:
/// OKF needs only root + sourceId, Obsidian only a root. The entry's declared
/// rescan interval drives its periodic full rescan (0 = notifications only).
async fn build_alternate(
    source_id: &str,
    definition: &crate::importer_catalog::ImporterSourceDefinition,
) -> Result<AppState, String> {
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
    crate::watcher::spawn(
        state.clone(),
        definition.filesystem_rescan_interval_seconds.unwrap_or(0),
    );
    Ok(state)
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
