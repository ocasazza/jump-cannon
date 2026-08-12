//! The session-manager HTTP server (feature `server`, native only).
//!
//! One axum app serves three surfaces:
//!
//! - `GET /healthz` — unauthenticated liveness.
//! - `/api/*` — the versioned world/session/VCS REST API consumed by
//!   [`crate::http::HttpSessionManager`]. Every request must carry the
//!   configured user header (default `x-user`); an optional second header
//!   (default `x-user-groups`, comma-separated) carries group memberships.
//!   In deployment an authenticating ingress sets these; the server trusts
//!   them verbatim (same local-dev no-auth stance as graph-api, one hop
//!   out).
//! - `/worlds/:name/*` — the world's live graph, served by nesting a
//!   per-world [`graph_api::api_router`] (built from a
//!   [`crate::world_importer::WorldImporter`]) and dispatching through it
//!   via tower `oneshot`. No outer-router rebuild on world create/close.
//!
//! ACL policy (coarse, this milestone): the world creator is its sole
//! initial writer; writers may commit/merge/rebase/resolve, close the
//! world, and replace the ACL; ANY authenticated user may read everything
//! and join sessions.

use crate::kubernetes::{KubernetesSessionManager, MAIN_BRANCH};
use crate::types::{
    SessionError, SessionId, UserIdentity, WorldAcl, WorldHandle, WorldId, WorldInfo, WorldSpec,
};
use crate::world_importer::WorldImporter;
use crate::{SessionDirectory, WorldHost};
use axum::{
    extract::{Path, Query, Request, State},
    http::{HeaderName, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post, put},
    Extension, Json, Router,
};
use graph_vcs::{CommitId, ConflictResolution, GraphOp, VcsError, VcsStore};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Header configuration for the auth middleware.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Header carrying the authenticated user name (default `x-user`).
    pub user_header: String,
    /// Header carrying comma-separated group memberships (default
    /// `x-user-groups`).
    pub groups_header: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            user_header: "x-user".to_string(),
            groups_header: "x-user-groups".to_string(),
        }
    }
}

/// Cloneable server state shared by all handlers.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<ServerStateInner>,
}

struct ServerStateInner {
    manager: Arc<KubernetesSessionManager>,
    user_header: HeaderName,
    groups_header: HeaderName,
    /// Pre-built per-world graph serving state. Inserted on world open,
    /// removed on close; no outer-router rebuilds.
    worlds: RwLock<HashMap<WorldId, WorldServing>>,
}

/// One world's nested graph-api router plus the push trigger that rebuilds
/// its serving snapshot after head-moving VCS mutations.
struct WorldServing {
    router: Router,
    push: tokio::sync::watch::Sender<u64>,
    /// The branch the world's `WorldImporter` materializes.
    branch: String,
    /// The world's graph-api compute broker + progress log, handed to the
    /// GPU broker on dispatch/status so the session loop observes the right
    /// per-world broker and surfaces progress into the world's own stream.
    compute_broker: graph_api::compute_broker::ComputeBroker,
    progress: Arc<graph_api::progress::ProgressLog>,
}

/// Build the full server router.
pub fn router(manager: Arc<KubernetesSessionManager>, config: ServerConfig) -> Router {
    let state = ServerState {
        inner: Arc::new(ServerStateInner {
            manager,
            user_header: HeaderName::from_bytes(config.user_header.as_bytes())
                .expect("--user-header must be a valid header name"),
            groups_header: HeaderName::from_bytes(config.groups_header.as_bytes())
                .expect("groups header must be a valid header name"),
            worlds: RwLock::new(HashMap::new()),
        }),
    };

    let api = Router::new()
        .route("/api/host", get(host_descriptor))
        .route("/api/worlds", get(list_worlds).post(create_world))
        .route("/api/worlds/:id", get(world_info).delete(close_world))
        .route("/api/worlds/:id/acl", put(put_acl))
        .route("/api/worlds/:id/compute", get(world_compute))
        .route(
            "/api/worlds/:id/compute/dispatch",
            post(compute_dispatch),
        )
        .route("/api/worlds/:id/compute/park", post(compute_park))
        .route(
            "/api/worlds/:id/compute/session",
            get(compute_session),
        )
        .route(
            "/api/worlds/:id/vcs/branches",
            get(vcs_branches).post(vcs_create_branch),
        )
        .route("/api/worlds/:id/vcs/log", get(vcs_log))
        .route("/api/worlds/:id/vcs/commits", post(vcs_commit))
        .route("/api/worlds/:id/vcs/merges", post(vcs_merge))
        .route("/api/worlds/:id/vcs/rebases", post(vcs_rebase))
        .route("/api/worlds/:id/vcs/conflicts", get(vcs_conflicts))
        .route("/api/worlds/:id/vcs/resolutions", post(vcs_resolve))
        .route("/api/worlds/:id/vcs/diff", get(vcs_diff))
        .route("/api/worlds/:id/vcs/op-log", get(vcs_op_log))
        .route("/api/worlds/:id/vcs/snapshots/:commit", get(vcs_snapshot))
        .route(
            "/api/worlds/:id/sessions",
            get(list_sessions).post(join_session),
        )
        .route("/api/worlds/:id/sessions/:sid", delete(leave_session))
        .route("/api/worlds/:id/members", get(world_members))
        // Per-world graph serving, dispatched to the nested per-world
        // graph-api router. `any` covers the binary GETs plus PUT
        // /vault/page, POST /generate, and the layout WS upgrade.
        .route("/worlds/:name", any(world_mux))
        .route("/worlds/:name/*path", any(world_mux))
        // Commits carry whole op batches; the default 2 MiB body limit is
        // too tight for large worlds.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        // The Dioxus/Tauri app calls this API cross-origin.
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(middleware::from_fn_with_state(state.clone(), auth));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(api)
        .with_state(state)
}

// --- auth -------------------------------------------------------------------

/// Extract the caller identity from the configured headers. Missing or
/// empty user header → 401. Only `/healthz` is exempt (it is mounted
/// outside the layered API router).
async fn auth(State(s): State<ServerState>, mut req: Request, next: Next) -> Response {
    let name = req
        .headers()
        .get(&s.inner.user_header)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let Some(name) = name else {
        return err(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            format!(
                "missing or empty user header {:?}",
                s.inner.user_header.as_str()
            ),
        );
    };
    let groups = req
        .headers()
        .get(&s.inner.groups_header)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|group| !group.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    req.extensions_mut().insert(UserIdentity { name, groups });
    next.run(req).await
}

// --- shared helpers ----------------------------------------------------------

/// JSON error envelope. `kind` is a stable machine-readable discriminant the
/// HTTP client maps back onto [`SessionError`] variants.
fn err(status: StatusCode, kind: &str, message: String) -> Response {
    (status, Json(json!({ "error": message, "kind": kind }))).into_response()
}

fn session_error_response(error: SessionError) -> Response {
    let (status, kind) = match &error {
        SessionError::WorldNotFound { .. } => (StatusCode::NOT_FOUND, "world_not_found"),
        SessionError::WorldExists { .. } => (StatusCode::CONFLICT, "world_exists"),
        SessionError::InvalidName { .. } => (StatusCode::BAD_REQUEST, "invalid_name"),
        SessionError::SessionNotFound { .. } => (StatusCode::NOT_FOUND, "session_not_found"),
        SessionError::Unauthorized { .. } => (StatusCode::FORBIDDEN, "unauthorized"),
        SessionError::Store(_) => (StatusCode::BAD_REQUEST, "store"),
        SessionError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io"),
    };
    err(status, kind, error.to_string())
}

fn vcs_error_response(error: VcsError) -> Response {
    let status = match &error {
        VcsError::UnknownBranch { .. } | VcsError::UnknownCommit { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    err(status, "store", error.to_string())
}

fn parse_world_id(raw: &str) -> Result<WorldId, Response> {
    WorldId::parse(raw).map_err(|_| {
        err(
            StatusCode::NOT_FOUND,
            "world_not_found",
            format!("unknown world {raw:?}"),
        )
    })
}

fn directory(s: &ServerState) -> &dyn SessionDirectory {
    s.inner
        .manager
        .sessions()
        .expect("the Kubernetes host always exposes a session directory")
}

/// Gate a mutating operation: unknown world → 404, non-writer → 403.
fn require_writer(s: &ServerState, id: &WorldId, user: &UserIdentity) -> Result<(), Response> {
    match s.inner.manager.is_writer(id, user) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err(
            StatusCode::FORBIDDEN,
            "unauthorized",
            format!("{:?} is not a writer on world {id}", user.name),
        )),
        Err(e) => Err(session_error_response(e)),
    }
}

fn open_store(s: &ServerState, id: &WorldId) -> Result<Arc<dyn VcsStore>, Response> {
    s.inner.manager.open_store(id).map_err(session_error_response)
}

/// Build (or fetch) the nested graph serving state for one open world.
async fn ensure_serving(s: &ServerState, id: &WorldId) -> Result<(), Response> {
    if read_worlds(s).contains_key(id) {
        return Ok(());
    }
    let store = open_store(s, id)?;
    let (tx, rx) = tokio::sync::watch::channel(0u64);
    let importer: Box<dyn data_loader::Importer> =
        Box::new(WorldImporter::new(store, MAIN_BRANCH, id));
    // The host grants exactly the minimal capabilities the world importer
    // declares (in-memory read + watch of the world's store).
    let grants: HashSet<data_loader::Capability> =
        importer.descriptor().capabilities.into_iter().collect();
    let progress = Arc::new(graph_api::progress::ProgressLog::new());
    let app_state = graph_api::build_world_state(
        importer,
        grants,
        s.inner.manager.worlds_dir().join(&id.0),
        progress.clone(),
    )
    .await
    .map_err(|error| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "import",
            format!("build world serving state: {error}"),
        )
    })?
    .with_push_trigger(rx);
    // Arm the push change driver; the initial snapshot was already built by
    // `build_world_state`, and every later tick runs a full rebuild.
    graph_api::watcher::spawn(app_state.clone(), 0);
    let compute_broker = app_state.inner.compute_broker.clone();
    let router = graph_api::api_router(app_state);
    write_worlds(s).insert(
        id.clone(),
        WorldServing {
            router,
            push: tx,
            branch: MAIN_BRANCH.to_string(),
            compute_broker,
            progress,
        },
    );
    Ok(())
}

/// Reset the world's GPU idle clock (no-op when the broker is disabled or
/// the world was never dispatched). Wired to every graph-serving request and
/// every head-moving VCS mutation.
fn touch_gpu(s: &ServerState, id: &WorldId) {
    if let Some(broker) = s.inner.manager.gpu_broker() {
        broker.touch(id);
    }
}

/// Fire the world's push trigger when `branch` is the served branch. The
/// graph-api push driver then re-materializes the head and swaps the
/// serving snapshot.
fn fire_push(s: &ServerState, id: &WorldId, branch: &str) {
    touch_gpu(s, id);
    if let Some(serving) = read_worlds(s).get(id) {
        if serving.branch == branch {
            serving.push.send_modify(|tick| *tick += 1);
        }
    }
}

fn read_worlds(
    s: &ServerState,
) -> std::sync::RwLockReadGuard<'_, HashMap<WorldId, WorldServing>> {
    s.inner
        .worlds
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_worlds(
    s: &ServerState,
) -> std::sync::RwLockWriteGuard<'_, HashMap<WorldId, WorldServing>> {
    s.inner
        .worlds
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// --- meta routes --------------------------------------------------------------

async fn healthz() -> &'static str {
    "ok"
}

async fn host_descriptor(State(s): State<ServerState>) -> impl IntoResponse {
    Json(s.inner.manager.descriptor())
}

async fn list_worlds(State(s): State<ServerState>) -> Response {
    match s.inner.manager.worlds().await {
        Ok(worlds) => Json(worlds).into_response(),
        Err(e) => session_error_response(e),
    }
}

#[derive(Deserialize)]
struct CreateWorldReq {
    name: String,
    description: Option<String>,
}

async fn create_world(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Json(req): Json<CreateWorldReq>,
) -> Response {
    let handle = match s.inner.manager.open_world_as(
        WorldSpec {
            name: req.name,
            description: req.description,
        },
        &user,
    ) {
        Ok(handle) => handle,
        Err(e) => return session_error_response(e),
    };
    if let Err(resp) = ensure_serving(&s, &handle.id).await {
        return resp;
    }
    Json::<WorldHandle>(handle).into_response()
}

async fn world_info(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match s.inner.manager.world_info(&id) {
        Ok(info) => Json::<WorldInfo>(info).into_response(),
        Err(e) => session_error_response(e),
    }
}

async fn close_world(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    match s.inner.manager.close_world(&id).await {
        Ok(()) => {
            // Dropping the serving entry ends the push-driver task (its
            // sender is gone) and unmounts the nested router.
            write_worlds(&s).remove(&id);
            // Park the world's GPU session (if any) — a closed world must
            // not hold Kueue quota.
            if let Some(broker) = s.inner.manager.gpu_broker() {
                broker.retire(&id).await;
            }
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => session_error_response(e),
    }
}

async fn put_acl(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(acl): Json<WorldAcl>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    match s.inner.manager.set_acl(&id, acl) {
        Ok(()) => match s.inner.manager.acl(&id) {
            Ok(acl) => Json(acl).into_response(),
            Err(e) => session_error_response(e),
        },
        Err(e) => session_error_response(e),
    }
}

async fn world_compute(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match s.inner.manager.compute(&id).await {
        Ok(handle) => Json(handle).into_response(),
        Err(e) => session_error_response(e),
    }
}

// --- per-world GPU compute session routes ---
//
// Mirror graph-api's `/compute/session` contract per world:
// `GET  /api/worlds/:id/compute/session` — `{"enabled": false}` when the
// broker is disabled; otherwise the live `SessionStatus` shape derived from
// cluster objects. Any authenticated user may read.
// `POST /api/worlds/:id/compute/dispatch|park` — soft envelope
// `{ "ok": bool, "state": string|null, "error": string|null }`; writer-only.

/// The world's compute broker + progress log, building the serving state on
/// demand (dispatch against a world whose serving state was never built must
/// still work).
async fn world_gpu_parts(
    s: &ServerState,
    id: &WorldId,
) -> Result<(graph_api::compute_broker::ComputeBroker, Arc<graph_api::progress::ProgressLog>), Response>
{
    ensure_serving(s, id).await?;
    let serving = read_worlds(s);
    let Some(serving) = serving.get(id) else {
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "import",
            format!("world {id} has no serving state"),
        ));
    };
    Ok((serving.compute_broker.clone(), serving.progress.clone()))
}

fn gpu_disabled_response() -> Response {
    Json(json!({
        "ok": false,
        "state": null,
        "error": "gpu broker is not enabled (no JUMP_CANNON_SM_GPU_TEMPLATE mounted)",
    }))
    .into_response()
}

async fn compute_dispatch(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let Some(broker) = s.inner.manager.gpu_broker() else {
        return gpu_disabled_response();
    };
    let (compute_broker, progress) = match world_gpu_parts(&s, &id).await {
        Ok(parts) => parts,
        Err(resp) => return resp,
    };
    match broker.dispatch(&id, &compute_broker, &progress).await {
        Ok(state) => Json(json!({ "ok": true, "state": state.as_str(), "error": null }))
            .into_response(),
        Err(e) => Json(json!({ "ok": false, "state": null, "error": format!("{e:#}") }))
            .into_response(),
    }
}

async fn compute_park(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let Some(broker) = s.inner.manager.gpu_broker() else {
        return gpu_disabled_response();
    };
    match broker.park(&id).await {
        Ok(state) => Json(json!({ "ok": true, "state": state.as_str(), "error": null }))
            .into_response(),
        Err(e) => Json(json!({ "ok": false, "state": null, "error": format!("{e:#}") }))
            .into_response(),
    }
}

async fn compute_session(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let Some(broker) = s.inner.manager.gpu_broker() else {
        return Json(json!({ "enabled": false })).into_response();
    };
    let (compute_broker, progress) = match world_gpu_parts(&s, &id).await {
        Ok(parts) => parts,
        Err(resp) => return resp,
    };
    let status = broker.status(&id, &compute_broker, &progress).await;
    match serde_json::to_value(status) {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "gpu session status serialization failed");
            Json(json!({ "enabled": false })).into_response()
        }
    }
}

// --- VCS routes -----------------------------------------------------------------

async fn vcs_branches(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.branches().await {
        Ok(branches) => Json(branches).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct CreateBranchReq {
    name: String,
    from_commit: CommitId,
}

async fn vcs_create_branch(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(req): Json<CreateBranchReq>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.create_branch(&req.name, &req.from_commit).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    branch: Option<String>,
    limit: Option<usize>,
}

async fn vcs_log(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    let branch = query.branch.unwrap_or_else(|| MAIN_BRANCH.to_string());
    match store.log(&branch, query.limit.unwrap_or(100)).await {
        Ok(commits) => Json(commits).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct CommitReq {
    branch: String,
    ops: Vec<GraphOp>,
    message: String,
}

async fn vcs_commit(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(req): Json<CommitReq>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store
        .commit(&req.branch, req.ops, &user.name, &req.message)
        .await
    {
        Ok(commit) => {
            fire_push(&s, &id, &req.branch);
            Json(commit).into_response()
        }
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct MergeReq {
    into: String,
    from: String,
    message: String,
}

async fn vcs_merge(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(req): Json<MergeReq>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store
        .merge(&req.into, &req.from, &user.name, &req.message)
        .await
    {
        Ok(report) => {
            fire_push(&s, &id, &req.into);
            Json(report).into_response()
        }
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct RebaseReq {
    branch: String,
    onto: String,
}

async fn vcs_rebase(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(req): Json<RebaseReq>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.rebase(&req.branch, &req.onto, &user.name).await {
        Ok(report) => {
            fire_push(&s, &id, &req.branch);
            Json(report).into_response()
        }
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct ConflictsQuery {
    branch: Option<String>,
}

async fn vcs_conflicts(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<ConflictsQuery>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    let branch = query.branch.unwrap_or_else(|| MAIN_BRANCH.to_string());
    match store.conflicts(&branch).await {
        Ok(conflicts) => Json(conflicts).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct ResolutionsReq {
    branch: String,
    resolutions: Vec<ConflictResolution>,
}

async fn vcs_resolve(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
    Json(req): Json<ResolutionsReq>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Err(resp) = require_writer(&s, &id, &user) {
        return resp;
    }
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store
        .resolve(&req.branch, req.resolutions, &user.name)
        .await
    {
        Ok(commit) => {
            fire_push(&s, &id, &req.branch);
            Json(commit).into_response()
        }
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct DiffQuery {
    a: String,
    b: String,
}

async fn vcs_diff(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.diff(&CommitId(query.a), &CommitId(query.b)).await {
        Ok(ops) => Json(ops).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

#[derive(Deserialize)]
struct OpLogQuery {
    limit: Option<usize>,
}

async fn vcs_op_log(
    State(s): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<OpLogQuery>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.op_log(query.limit.unwrap_or(100)).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

async fn vcs_snapshot(
    State(s): State<ServerState>,
    Path((id, commit)): Path<(String, String)>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let store = match open_store(&s, &id) {
        Ok(store) => store,
        Err(resp) => return resp,
    };
    match store.materialize(&CommitId(commit)).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => vcs_error_response(e),
    }
}

// --- session routes --------------------------------------------------------------

async fn join_session(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match directory(&s).join(&id, &user).await {
        Ok(session) => Json(session).into_response(),
        Err(e) => session_error_response(e),
    }
}

async fn leave_session(
    State(s): State<ServerState>,
    Extension(user): Extension<UserIdentity>,
    Path((id, sid)): Path<(String, String)>,
) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let sid = SessionId(sid);
    // The session owner or any world writer may detach a session.
    let directory = s
        .inner
        .manager
        .sessions()
        .expect("the Kubernetes host always exposes a session directory");
    match directory.sessions(&id).await {
        Ok(list) => {
            let Some(session) = list.iter().find(|session| session.id == sid) else {
                return err(
                    StatusCode::NOT_FOUND,
                    "session_not_found",
                    format!("session not found: {sid}"),
                );
            };
            if session.user.name != user.name {
                if let Err(resp) = require_writer(&s, &id, &user) {
                    return resp;
                }
            }
        }
        Err(e) => return session_error_response(e),
    }
    match directory.leave(&sid).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => session_error_response(e),
    }
}

async fn list_sessions(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match directory(&s).sessions(&id).await {
        Ok(sessions) => Json(sessions).into_response(),
        Err(e) => session_error_response(e),
    }
}

async fn world_members(State(s): State<ServerState>, Path(id): Path<String>) -> Response {
    let id = match parse_world_id(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match directory(&s).members(&id).await {
        Ok(acl) => Json::<WorldAcl>(acl).into_response(),
        Err(e) => session_error_response(e),
    }
}

// --- per-world graph mux -----------------------------------------------------------

/// Dispatch a `/worlds/:name/*` request to the world's pre-built graph-api
/// router: strip the `/worlds/:name` prefix by rewriting the URI, then
/// drive the nested router in-process via tower `oneshot` (axum 0.7's
/// `Router<()>` implements `Service<Request<B>>`). Unknown or closed worlds
/// get a JSON 404.
async fn world_mux(State(s): State<ServerState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(rest) = path.strip_prefix("/worlds/") else {
        return err(
            StatusCode::NOT_FOUND,
            "world_not_found",
            format!("unknown world route {path:?}"),
        );
    };
    let name = rest.split('/').next().unwrap_or_default();
    let id = match parse_world_id(name) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    touch_gpu(&s, &id);
    let router = read_worlds(&s).get(&id).map(|serving| serving.router.clone());
    let Some(router) = router else {
        return err(
            StatusCode::NOT_FOUND,
            "world_not_found",
            format!("world {name:?} is not open"),
        );
    };
    let suffix = &rest[name.len()..]; // "" or "/graph/init?..."
    let suffix = if suffix.is_empty() { "/" } else { suffix };
    let uri = match format!(
        "{}{}",
        suffix,
        req.uri()
            .query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    )
    .parse::<Uri>()
    {
        Ok(uri) => uri,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("invalid world path {path:?}"),
            )
        }
    };
    let (mut parts, body) = req.into_parts();
    parts.uri = uri;
    let req = Request::from_parts(parts, body);
    use tower::ServiceExt as _;
    match router.oneshot(req).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}
