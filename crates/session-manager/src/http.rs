//! `HttpSessionManager`: a [`WorldHost`] client for the session-manager
//! REST API (`/api/*`, see [`crate::server`]).
//!
//! The client is wasm32-clean: the same reqwest-based request path compiles
//! to the browser's `fetch` on wasm and to plain HTTP natively (no TLS
//! backend — TLS terminates at the ingress in front of the server).
//!
//! Every request carries the configured identity headers (default `x-user`
//! plus `x-user-groups` when the user has groups). The server authenticates
//! by header, so [`HttpDirectory::join`] ignores its `user` argument beyond
//! documentation value — the joined identity is always the client's
//! configured one.
//!
//! Error mapping: the server's JSON error envelope carries a `kind`
//! discriminant (`world_not_found`, `world_exists`, `session_not_found`,
//! `unauthorized`, …) which the client maps back onto the matching
//! [`SessionError`] variant; anything else degrades to a store error
//! carrying the server's message.

use crate::types::{
    ComputeHandle, HostDescriptor, HostKind, SessionError, SessionId, UserIdentity, WorldAcl,
    WorldHandle, WorldId, WorldInfo, WorldSession, WorldSpec,
};
use crate::{SessionDirectory, SessionFuture, WorldHost};
use graph_vcs::{
    BranchInfo, Commit, CommitId, Conflict, ConflictResolution, GraphOp, MergeReport, OpLogEntry,
    RebaseReport, Snapshot, VcsError, VcsFuture, VcsStore,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A `WorldHost` reached over HTTP: the client half of the multi-user
/// session-manager server.
pub struct HttpSessionManager {
    core: HttpCore,
    /// The remote host's self-description, fetched at connect time.
    remote: HostDescriptor,
    directory: Option<HttpDirectory>,
}

impl HttpSessionManager {
    /// Connect with the default identity headers (`x-user`,
    /// `x-user-groups`). Fetches `/api/host` to learn the remote's
    /// descriptor (multi-user flag drives whether `sessions()` is `Some`).
    pub async fn connect(
        base_url: impl Into<String>,
        user: UserIdentity,
    ) -> Result<Self, SessionError> {
        Self::connect_with_headers(base_url, user, "x-user", "x-user-groups").await
    }

    /// Connect with explicit identity header names (matching the server's
    /// `--user-header` configuration).
    pub async fn connect_with_headers(
        base_url: impl Into<String>,
        user: UserIdentity,
        user_header: impl Into<String>,
        groups_header: impl Into<String>,
    ) -> Result<Self, SessionError> {
        let core = HttpCore {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            user: Arc::new(user),
            user_header: user_header.into(),
            groups_header: groups_header.into(),
        };
        let remote: HostDescriptor = core.get_json("/api/host", "host").await?;
        let directory = if remote.multi_user {
            Some(HttpDirectory {
                core: core.clone(),
                joined: Mutex::new(HashMap::new()),
            })
        } else {
            None
        };
        Ok(Self {
            core,
            remote,
            directory,
        })
    }
}

impl WorldHost for HttpSessionManager {
    fn descriptor(&self) -> HostDescriptor {
        // The client is always a REMOTE view, whatever kind the far end is.
        HostDescriptor {
            id: self.remote.id.clone(),
            kind: HostKind::Remote,
            multi_user: self.remote.multi_user,
        }
    }

    fn worlds<'a>(&'a self) -> SessionFuture<'a, Result<Vec<WorldInfo>, SessionError>> {
        Box::pin(async move { self.core.get_json("/api/worlds", "worlds").await })
    }

    fn open_world<'a>(
        &'a self,
        spec: WorldSpec,
    ) -> SessionFuture<'a, Result<WorldHandle, SessionError>> {
        Box::pin(async move {
            let ctx = WorldId::from_name(&spec.name)
                .map(|id| id.0)
                .unwrap_or_else(|_| spec.name.clone());
            self.core
                .send_json(
                    self.core.post("/api/worlds").json(&spec),
                    &ctx,
                )
                .await
        })
    }

    fn close_world<'a>(&'a self, id: &'a WorldId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            self.core
                .send_json::<serde_json::Value>(
                    self.core.delete(&format!("/api/worlds/{id}")),
                    &id.0,
                )
                .await?;
            Ok(())
        })
    }

    fn vcs<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<Arc<dyn VcsStore>, SessionError>> {
        Box::pin(async move {
            // Existence/open check: the VCS surface has no cheaper probe,
            // and this doubles as the close-invalidation signal
            // (`WorldNotFound` once the world is closed).
            let _: WorldInfo = self
                .core
                .get_json(&format!("/api/worlds/{id}"), &id.0)
                .await?;
            Ok(Arc::new(HttpVcsStore {
                core: self.core.clone(),
                world: id.clone(),
            }) as Arc<dyn VcsStore>)
        })
    }

    fn compute<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<ComputeHandle, SessionError>> {
        Box::pin(async move {
            self.core
                .get_json(&format!("/api/worlds/{id}/compute"), &id.0)
                .await
        })
    }

    fn sessions(&self) -> Option<&dyn SessionDirectory> {
        self.directory
            .as_ref()
            .map(|directory| directory as &dyn SessionDirectory)
    }
}

/// The HTTP client's session directory: forwards the session endpoints of a
/// multi-user remote.
pub struct HttpDirectory {
    core: HttpCore,
    /// Session id → world, for `leave` (the leave route is world-scoped and
    /// the trait hands us only the session id).
    joined: Mutex<HashMap<SessionId, WorldId>>,
}

impl HttpDirectory {
    fn note(&self, session: &WorldSession) {
        if let Ok(mut joined) = self.joined.lock() {
            joined.insert(session.id.clone(), session.world.clone());
        }
    }
}

impl SessionDirectory for HttpDirectory {
    fn join<'a>(
        &'a self,
        world: &'a WorldId,
        user: &'a UserIdentity,
    ) -> SessionFuture<'a, Result<WorldSession, SessionError>> {
        Box::pin(async move {
            // The server derives the identity from the auth headers; `user`
            // is advisory on the wire. Flag genuine mismatches early.
            if user.name != self.core.user.name {
                return Err(SessionError::Unauthorized {
                    reason: format!(
                        "http client is bound to {:?}, cannot join as {:?}",
                        self.core.user.name, user.name
                    ),
                });
            }
            let session: WorldSession = self
                .core
                .send_json(
                    self.core.post(&format!("/api/worlds/{world}/sessions")),
                    &world.0,
                )
                .await?;
            self.note(&session);
            Ok(session)
        })
    }

    fn leave<'a>(&'a self, session: &'a SessionId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let world = self
                .joined
                .lock()
                .ok()
                .and_then(|joined| joined.get(session).cloned())
                .ok_or_else(|| SessionError::SessionNotFound {
                    id: session.0.clone(),
                })?;
            self.core
                .send_json::<serde_json::Value>(
                    self.core
                        .delete(&format!("/api/worlds/{world}/sessions/{session}")),
                    &session.0,
                )
                .await?;
            if let Ok(mut joined) = self.joined.lock() {
                joined.remove(session);
            }
            Ok(())
        })
    }

    fn sessions<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<Vec<WorldSession>, SessionError>> {
        Box::pin(async move {
            let sessions: Vec<WorldSession> = self
                .core
                .get_json(&format!("/api/worlds/{world}/sessions"), &world.0)
                .await?;
            for session in &sessions {
                self.note(session);
            }
            Ok(sessions)
        })
    }

    fn members<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<WorldAcl, SessionError>> {
        Box::pin(async move {
            self.core
                .get_json(&format!("/api/worlds/{world}/members"), &world.0)
                .await
        })
    }
}

/// A [`VcsStore`] forwarding every operation to one world's `/vcs/*`
/// endpoints.
pub struct HttpVcsStore {
    core: HttpCore,
    world: WorldId,
}

impl HttpVcsStore {
    fn base(&self) -> String {
        format!("/api/worlds/{}/vcs", self.world)
    }

    async fn rpc<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, VcsError> {
        self.core
            .send_json(request, &self.world.0)
            .await
            .map_err(vcs_err)
    }
}

impl VcsStore for HttpVcsStore {
    fn head<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Option<CommitId>, VcsError>> {
        Box::pin(async move {
            // No dedicated head endpoint: the branches listing carries the
            // same information.
            let branches: Vec<BranchInfo> = self.rpc(self.core.get(&self.base())).await?;
            Ok(branches
                .into_iter()
                .find(|info| info.name.0 == branch)
                .map(|info| info.head))
        })
    }

    fn commit<'a>(
        &'a self,
        branch: &'a str,
        ops: Vec<GraphOp>,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>> {
        Box::pin(async move {
            // The server records the authenticated caller as author; the
            // `author` argument is advisory and must match the client's
            // configured identity.
            if author != self.core.user.name {
                return Err(VcsError::Store {
                    message: format!(
                        "http client is bound to {:?}, cannot commit as {author:?}",
                        self.core.user.name
                    ),
                });
            }
            self.rpc(self.core.post(&format!("{}/commits", self.base())).json(&serde_json::json!({
                "branch": branch,
                "ops": ops,
                "message": message,
            })))
            .await
        })
    }

    fn log<'a>(
        &'a self,
        branch: &'a str,
        limit: usize,
    ) -> VcsFuture<'a, Result<Vec<Commit>, VcsError>> {
        Box::pin(async move {
            let limit = limit.to_string();
            self.rpc(
                self.core
                    .get(&format!("{}/log", self.base()))
                    .query(&[("branch", branch), ("limit", limit.as_str())]),
            )
            .await
        })
    }

    fn branches(&self) -> VcsFuture<'_, Result<Vec<BranchInfo>, VcsError>> {
        Box::pin(async move { self.rpc(self.core.get(&self.base())).await })
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        from: &'a CommitId,
    ) -> VcsFuture<'a, Result<(), VcsError>> {
        Box::pin(async move {
            self.rpc::<serde_json::Value>(
                self.core
                    .post(&format!("{}/branches", self.base()))
                    .json(&serde_json::json!({ "name": name, "from_commit": from })),
            )
            .await?;
            Ok(())
        })
    }

    fn merge<'a>(
        &'a self,
        into: &'a str,
        from: &'a str,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<MergeReport, VcsError>> {
        Box::pin(async move {
            if author != self.core.user.name {
                return Err(VcsError::Store {
                    message: format!(
                        "http client is bound to {:?}, cannot merge as {author:?}",
                        self.core.user.name
                    ),
                });
            }
            self.rpc(self.core.post(&format!("{}/merges", self.base())).json(&serde_json::json!({
                "into": into,
                "from": from,
                "message": message,
            })))
            .await
        })
    }

    fn rebase<'a>(
        &'a self,
        branch: &'a str,
        onto: &'a str,
        author: &'a str,
    ) -> VcsFuture<'a, Result<RebaseReport, VcsError>> {
        Box::pin(async move {
            if author != self.core.user.name {
                return Err(VcsError::Store {
                    message: format!(
                        "http client is bound to {:?}, cannot rebase as {author:?}",
                        self.core.user.name
                    ),
                });
            }
            self.rpc(
                self.core
                    .post(&format!("{}/rebases", self.base()))
                    .json(&serde_json::json!({ "branch": branch, "onto": onto })),
            )
            .await
        })
    }

    fn conflicts<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Vec<Conflict>, VcsError>> {
        Box::pin(async move {
            self.rpc(
                self.core
                    .get(&format!("{}/conflicts", self.base()))
                    .query(&[("branch", branch)]),
            )
            .await
        })
    }

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        resolutions: Vec<ConflictResolution>,
        author: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>> {
        Box::pin(async move {
            if author != self.core.user.name {
                return Err(VcsError::Store {
                    message: format!(
                        "http client is bound to {:?}, cannot resolve as {author:?}",
                        self.core.user.name
                    ),
                });
            }
            self.rpc(
                self.core.post(&format!("{}/resolutions", self.base())).json(
                    &serde_json::json!({ "branch": branch, "resolutions": resolutions }),
                ),
            )
            .await
        })
    }

    fn diff<'a>(
        &'a self,
        a: &'a CommitId,
        b: &'a CommitId,
    ) -> VcsFuture<'a, Result<Vec<GraphOp>, VcsError>> {
        Box::pin(async move {
            self.rpc(
                self.core
                    .get(&format!("{}/diff", self.base()))
                    .query(&[("a", a.0.as_str()), ("b", b.0.as_str())]),
            )
            .await
        })
    }

    fn materialize<'a>(&'a self, commit: &'a CommitId) -> VcsFuture<'a, Result<Snapshot, VcsError>> {
        Box::pin(async move {
            self.rpc(
                self.core
                    .get(&format!("{}/snapshots/{}", self.base(), commit.0)),
            )
            .await
        })
    }

    fn op_log(&self, limit: usize) -> VcsFuture<'_, Result<Vec<OpLogEntry>, VcsError>> {
        Box::pin(async move {
            let limit = limit.to_string();
            self.rpc(
                self.core
                    .get(&format!("{}/op-log", self.base()))
                    .query(&[("limit", limit.as_str())]),
            )
            .await
        })
    }
}

/// Map a client-side [`SessionError`] onto the store error channel.
fn vcs_err(error: SessionError) -> VcsError {
    match error {
        SessionError::Store(store) => store,
        other => VcsError::Store {
            message: other.to_string(),
        },
    }
}

/// Shared request plumbing: base URL, identity headers, response decoding.
#[derive(Clone)]
struct HttpCore {
    client: reqwest::Client,
    base_url: String,
    user: Arc<UserIdentity>,
    user_header: String,
    groups_header: String,
}

/// A fully-buffered response: status plus body bytes. Buffering keeps the
/// wasm path Send-safe (no web-sys values cross an await).
struct WireResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpCore {
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .header(&self.user_header, self.user.name.as_str());
        if !self.user.groups.is_empty() {
            request = request.header(&self.groups_header, self.user.groups.join(","));
        }
        request
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::DELETE, path)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, ctx: &str) -> Result<T, SessionError> {
        self.send_json(self.get(path), ctx).await
    }

    /// Execute `request`, mapping non-2xx to [`SessionError`] and decoding
    /// the JSON body on success.
    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        ctx: &str,
    ) -> Result<T, SessionError> {
        let response = execute(request).await?;
        if response.status >= 400 {
            return Err(error_for(response.status, &response.body, ctx));
        }
        serde_json::from_slice(&response.body).map_err(|error| {
            SessionError::Store(VcsError::Store {
                message: format!("invalid response body: {error}"),
            })
        })
    }
}

/// The server's error envelope.
#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    error: Option<String>,
    kind: Option<String>,
}

/// Map an HTTP error response back onto a [`SessionError`], preferring the
/// server's `kind` discriminant and falling back to the status code.
fn error_for(status: u16, body: &[u8], ctx: &str) -> SessionError {
    let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
    let message = parsed
        .error
        .unwrap_or_else(|| format!("HTTP {status}"));
    match parsed.kind.as_deref() {
        Some("world_not_found") => SessionError::WorldNotFound { id: ctx.to_string() },
        Some("world_exists") => SessionError::WorldExists { id: ctx.to_string() },
        Some("invalid_name") => SessionError::InvalidName { name: message },
        Some("session_not_found") => SessionError::SessionNotFound { id: ctx.to_string() },
        Some("unauthorized") => SessionError::Unauthorized { reason: message },
        _ => match status {
            401 | 403 => SessionError::Unauthorized { reason: message },
            404 => SessionError::WorldNotFound { id: ctx.to_string() },
            _ => SessionError::Store(VcsError::Store { message }),
        },
    }
}

fn transport_error(message: String) -> SessionError {
    SessionError::Store(VcsError::Store {
        message: format!("transport: {message}"),
    })
}

/// Execute one request to completion, returning status + buffered body.
#[cfg(not(target_arch = "wasm32"))]
async fn execute(request: reqwest::RequestBuilder) -> Result<WireResponse, SessionError> {
    let response = request
        .send()
        .await
        .map_err(|error| transport_error(error.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .bytes()
        .await
        .map_err(|error| transport_error(error.to_string()))?
        .to_vec();
    Ok(WireResponse { status, body })
}

/// wasm32: the web-sys fetch internals are `!Send`, while the `WorldHost` /
/// `VcsStore` futures must be `Send`. The whole request runs inside
/// `spawn_local`; only Send-owned data (status + body bytes) crosses the
/// oneshot boundary. Wasm is single-threaded, so this is safe.
#[cfg(target_arch = "wasm32")]
async fn execute(request: reqwest::RequestBuilder) -> Result<WireResponse, SessionError> {
    let (tx, rx) = futures::channel::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|error| error.to_string())?
                .to_vec();
            Ok::<_, String>((status, body))
        }
        .await;
        let _ = tx.send(result);
    });
    let (status, body) = rx
        .await
        .map_err(|_| transport_error("request task dropped".to_string()))?
        .map_err(transport_error)?;
    Ok(WireResponse { status, body })
}
