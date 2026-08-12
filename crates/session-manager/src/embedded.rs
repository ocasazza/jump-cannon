//! The embedded (single-user) [`WorldHost`].
//!
//! One local user, one [`MinigrafStore`] per world. Three backends:
//!
//! - [`EmbeddedSessionManager::open`] (native): one `<world-slug>.graph`
//!   minigraf file per world under `root`, plus a small `worlds.json`
//!   manifest carrying the listing metadata (name, description, creation
//!   time) that the store files themselves do not know. Closing a world
//!   never deletes its file; re-opening the manager re-lists it.
//! - [`EmbeddedSessionManager::in_memory`]: worlds live in a map behind a
//!   `Mutex` — for tests and wasm32.
//! - [`EmbeddedSessionManager::open_persistent`] (wasm32 only): in-memory
//!   stores whose contents are re-exported to `localStorage` after every
//!   successful mutation (one [`crate::WorldExport`] JSON document per
//!   world) and replayed on boot, so browser-only worlds survive reloads.
//!   This is deliberately NOT minigraf's IndexedDB backend: minigraf
//!   1.2.3's `browser` feature exposes persistence only through the
//!   JS-facing `BrowserDb` wasm-bindgen façade — `Rc<RefCell>` internals
//!   (not `Send`/`Sync`, which `VcsStore` requires), a Promise/string
//!   Datalog API with no multi-command transactions — so it cannot back a
//!   [`VcsStore`] implementation. The export/replay substitution is
//!   best-effort (localStorage quota errors are swallowed) and documented
//!   here as the v1 browser persistence story.
//!
//! Session state is purely in-memory either way: [`SingleUserDirectory`]
//! admits exactly one fixed local identity and at most one session per
//! world.

use crate::export::{export_from_store, restore_into_store, WorldExport};
use crate::types::{
    ComputeHandle, HostDescriptor, HostKind, SessionError, SessionId, UserIdentity, WorldAcl,
    WorldHandle, WorldId, WorldInfo, WorldSession, WorldSpec,
};
use crate::{SessionDirectory, SessionFuture, WorldHost};
use graph_vcs::{MinigrafStore, VcsStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The one fixed identity of the embedded host.
const LOCAL_USER: &str = "local";

/// The branch every world is born with.
const MAIN_BRANCH: &str = "main";

/// Persisted listing metadata for one world (the `worlds.json` manifest;
/// everything else lives in the world's own minigraf file).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldMeta {
    id: WorldId,
    name: String,
    description: Option<String>,
    created_ts_ms: i64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Serialize, Deserialize)]
struct WorldsManifest {
    worlds: Vec<WorldMeta>,
}

/// Mutable manager state behind one lock.
struct State {
    /// Persisted metadata for every known world (open or closed).
    meta: HashMap<WorldId, WorldMeta>,
    /// Opened stores, kept so there is at most one `MinigrafStore` per world
    /// file per manager. Lazily populated for closed worlds when a listing
    /// needs branch counts.
    stores: HashMap<WorldId, Arc<dyn VcsStore>>,
    /// Worlds currently open via `open_world`. `vcs`/`compute` require an
    /// entry here; `close_world` removes it (the store cache may stay).
    open: HashSet<WorldId>,
}

/// The embedded single-user host. See the module docs for the persistence
/// model.
pub struct EmbeddedSessionManager {
    /// World files root; `None` for the in-memory backend. Native-only: the
    /// wasm32 build is always in-memory.
    #[cfg(not(target_arch = "wasm32"))]
    root: Option<std::path::PathBuf>,
    /// wasm32 only: when set, every successful store mutation re-exports the
    /// world to `localStorage` (the `open_persistent` backend).
    #[cfg(target_arch = "wasm32")]
    persist: bool,
    state: Arc<Mutex<State>>,
    directory: SingleUserDirectory,
}

impl EmbeddedSessionManager {
    /// Open (or create) a file-backed manager rooted at `root`. One
    /// `<world-slug>.graph` file per world; listing metadata in
    /// `worlds.json`. The manifest is authoritative; stray `*.graph` files
    /// without manifest entries are adopted with default metadata so a lost
    /// manifest never orphans a world.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(root: &std::path::Path) -> Result<Self, SessionError> {
        std::fs::create_dir_all(root)?;
        let mut meta = HashMap::new();
        let manifest_path = root.join("worlds.json");
        if manifest_path.exists() {
            let manifest: WorldsManifest = serde_json::from_str(&std::fs::read_to_string(
                &manifest_path,
            )?)
            .map_err(|e| {
                SessionError::Store(graph_vcs::VcsError::Corrupt {
                    message: format!("invalid worlds.json: {e}"),
                })
            })?;
            for world in manifest.worlds {
                meta.insert(world.id.clone(), world);
            }
        }
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("graph") {
                continue;
            }
            let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Ok(id) = WorldId::parse(slug) {
                meta.entry(id.clone()).or_insert(WorldMeta {
                    name: slug.to_string(),
                    id,
                    description: None,
                    created_ts_ms: 0,
                });
            }
        }
        Ok(Self::new(Some(root.to_path_buf()), meta))
    }

    /// An in-memory manager: no filesystem, nothing persists. For tests and
    /// wasm32 targets.
    pub fn in_memory() -> Result<Self, SessionError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(Self::new(None, HashMap::new()))
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self::new(HashMap::new(), false))
        }
    }

    /// The browser-persistent manager (wasm32 only): in-memory stores plus
    /// localStorage snapshots. On boot, every world stored by a previous
    /// session is replayed (closed); afterwards each successful commit /
    /// branch / merge / rebase / resolve re-exports the affected world.
    /// Corrupt stored exports are skipped (that world simply does not come
    /// back). See the module docs for why this is not minigraf's IndexedDB
    /// backend.
    #[cfg(target_arch = "wasm32")]
    pub fn open_persistent() -> Result<Self, SessionError> {
        let manager = Self::new(HashMap::new(), true);
        for export in persist::load_all() {
            let id = export.id.clone();
            let name = export.name.clone();
            // Best-effort per world: one corrupt export must not kill boot.
            let _ = manager.restore_world(id, name, export, false);
        }
        Ok(manager)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn new(root: Option<std::path::PathBuf>, meta: HashMap<WorldId, WorldMeta>) -> Self {
        let state = Arc::new(Mutex::new(State {
            meta,
            stores: HashMap::new(),
            open: HashSet::new(),
        }));
        Self {
            root,
            state: state.clone(),
            directory: SingleUserDirectory::new(state),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn new(meta: HashMap<WorldId, WorldMeta>, persist: bool) -> Self {
        let state = Arc::new(Mutex::new(State {
            meta,
            stores: HashMap::new(),
            open: HashSet::new(),
        }));
        Self {
            persist,
            state: state.clone(),
            directory: SingleUserDirectory::new(state),
        }
    }

    /// The store for `id`, opening and caching the world file on demand.
    /// Works for open and closed worlds; fails only for unknown ones.
    fn store_for(&self, id: &WorldId) -> Result<Arc<dyn VcsStore>, SessionError> {
        let mut state = lock(&self.state)?;
        if let Some(store) = state.stores.get(id) {
            return Ok(store.clone());
        }
        if !state.meta.contains_key(id) {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        }
        let store = self.new_store(id)?;
        state.stores.insert(id.clone(), store.clone());
        Ok(store)
    }

    /// The concrete store for one world: file-backed when the manager has a
    /// root (native), in-memory otherwise.
    fn new_concrete_store(&self, id: &WorldId) -> Result<MinigrafStore, SessionError> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(root) = &self.root {
            return Ok(MinigrafStore::open(root.join(format!("{}.graph", id.0)))?);
        }
        let _ = id;
        Ok(MinigrafStore::in_memory()?)
    }

    fn new_store(&self, id: &WorldId) -> Result<Arc<dyn VcsStore>, SessionError> {
        let store = Arc::new(self.new_concrete_store(id)?);
        Ok(self.wrap_store(id, store))
    }

    /// Attach the persistence wrapper when the manager persists (wasm32
    /// `open_persistent`); the identity function everywhere else.
    #[cfg(not(target_arch = "wasm32"))]
    fn wrap_store(&self, _id: &WorldId, store: Arc<dyn VcsStore>) -> Arc<dyn VcsStore> {
        store
    }

    #[cfg(target_arch = "wasm32")]
    fn wrap_store(&self, id: &WorldId, store: Arc<dyn VcsStore>) -> Arc<dyn VcsStore> {
        if !self.persist {
            return store;
        }
        Arc::new(PersistingStore {
            inner: store.clone(),
            hook: self.persist_hook(id, store),
        })
    }

    // ── Export / import ─────────────────────────────────────────────────────

    /// Dump one world's full VCS state — metadata, every commit reachable
    /// from any branch head (with its materialized snapshot), branch heads,
    /// and the op log — as a portable [`WorldExport`]. Works on open and
    /// closed worlds.
    pub fn export_world(&self, id: &WorldId) -> Result<WorldExport, SessionError> {
        let meta = lock(&self.state)?
            .meta
            .get(id)
            .cloned()
            .ok_or_else(|| SessionError::WorldNotFound { id: id.0.clone() })?;
        let store = self.store_for(id)?;
        export_from_store(
            &meta.id,
            &meta.name,
            meta.description.as_deref(),
            meta.created_ts_ms,
            store.as_ref(),
        )
    }

    /// Import a [`WorldExport`] as a new world named `name` (the id is
    /// re-slugged from it). Fails with [`SessionError::WorldExists`] when the
    /// id is taken. The imported world is returned open; commit ids,
    /// timestamps, authors, and conflicts are preserved verbatim.
    pub fn import_world(&self, name: &str, export: WorldExport) -> Result<WorldHandle, SessionError> {
        let id = WorldId::from_name(name)?;
        let handle = self.restore_world(id, name.to_string(), export, true)?;
        #[cfg(target_arch = "wasm32")]
        let _ = self.persist_world(&handle.id);
        Ok(handle)
    }

    /// Shared import core: replay `export` into a fresh store and register
    /// the world. `mark_open` distinguishes a user import (open, like
    /// `open_world`) from a boot restore (closed).
    fn restore_world(
        &self,
        id: WorldId,
        name: String,
        export: WorldExport,
        mark_open: bool,
    ) -> Result<WorldHandle, SessionError> {
        {
            let state = lock(&self.state)?;
            if state.meta.contains_key(&id) {
                return Err(SessionError::WorldExists { id: id.0.clone() });
            }
        }
        let store = self.new_concrete_store(&id)?;
        restore_into_store(&store, &export)?;
        let mut state = lock(&self.state)?;
        state
            .stores
            .insert(id.clone(), self.wrap_store(&id, Arc::new(store)));
        state.meta.insert(
            id.clone(),
            WorldMeta {
                id: id.clone(),
                name,
                description: export.description.clone(),
                created_ts_ms: export.created_ts_ms,
            },
        );
        #[cfg(not(target_arch = "wasm32"))]
        self.persist_manifest(&state)?;
        if mark_open {
            state.open.insert(id.clone());
        }
        Ok(WorldHandle { id })
    }

    // ── Browser persistence (wasm32 `open_persistent` only) ─────────────────

    /// The mutation hook closing over one world's state slot: re-export and
    /// write through to localStorage. Failures are swallowed by the caller
    /// (persistence is best-effort; the live store is authoritative).
    #[cfg(target_arch = "wasm32")]
    fn persist_hook(
        &self,
        id: &WorldId,
        inner: Arc<dyn VcsStore>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let state = self.state.clone();
        let id = id.clone();
        Arc::new(move || {
            let _ = persist_world_now(&state, &id, inner.as_ref());
        })
    }

    /// Persist one world now (world creation / import; regular mutations go
    /// through the [`PersistingStore`] hook).
    #[cfg(target_arch = "wasm32")]
    fn persist_world(&self, id: &WorldId) -> Result<(), SessionError> {
        if !self.persist {
            return Ok(());
        }
        let store = lock(&self.state)?.stores.get(id).cloned();
        let Some(store) = store else { return Ok(()) };
        persist_world_now(&self.state, id, store.as_ref())
    }

    /// Persist `worlds.json` (native, file-backed only).
    #[cfg(not(target_arch = "wasm32"))]
    fn persist_manifest(&self, state: &State) -> Result<(), SessionError> {
        let Some(root) = &self.root else { return Ok(()) };
        let manifest = WorldsManifest {
            worlds: state.meta.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&manifest).map_err(|e| {
            SessionError::Store(graph_vcs::VcsError::Store {
                message: format!("manifest encode failed: {e}"),
            })
        })?;
        std::fs::write(root.join("worlds.json"), json)?;
        Ok(())
    }

    fn require_open(&self, id: &WorldId) -> Result<(), SessionError> {
        let state = lock(&self.state)?;
        if !state.meta.contains_key(id) || !state.open.contains(id) {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        }
        Ok(())
    }

    fn do_open_world(&self, spec: WorldSpec) -> Result<WorldHandle, SessionError> {
        let id = WorldId::from_name(&spec.name)?;
        let is_new = {
            let state = lock(&self.state)?;
            if state.open.contains(&id) {
                return Err(SessionError::WorldExists { id: id.0.clone() });
            }
            !state.meta.contains_key(&id)
        };
        if is_new {
            // Every world is born with a head: an initial empty commit on
            // `main` by the local user.
            let store = self.new_store(&id)?;
            // The boxed future wraps synchronous store work; the crate's
            // trivial executor drives it to stay runtime-free.
            block_on(store.commit(MAIN_BRANCH, Vec::new(), LOCAL_USER, "world created"))?;
            let mut state = lock(&self.state)?;
            state.stores.insert(id.clone(), store);
            state.meta.insert(
                id.clone(),
                WorldMeta {
                    id: id.clone(),
                    name: spec.name,
                    description: spec.description,
                    created_ts_ms: now_ms(),
                },
            );
            #[cfg(not(target_arch = "wasm32"))]
            self.persist_manifest(&state)?;
        }
        lock(&self.state)?.open.insert(id.clone());
        // The initial commit fired before the metadata landed, so the
        // mutation hook could not see the new world; persist it explicitly.
        #[cfg(target_arch = "wasm32")]
        let _ = self.persist_world(&id);
        Ok(WorldHandle { id })
    }

    fn do_worlds(&self) -> Result<Vec<WorldInfo>, SessionError> {
        let metas: Vec<WorldMeta> = lock(&self.state)?.meta.values().cloned().collect();
        let mut infos = Vec::with_capacity(metas.len());
        for meta in metas {
            // File-backed minigraf can surface duplicate datoms for a
            // re-asserted branch name, so count DISTINCT branch names.
            let branches = block_on(self.store_for(&meta.id)?.branches())?;
            let names: std::collections::BTreeSet<&str> =
                branches.iter().map(|b| b.name.0.as_str()).collect();
            infos.push(WorldInfo {
                id: meta.id,
                name: meta.name,
                description: meta.description,
                created_ts_ms: meta.created_ts_ms,
                branches: names.len(),
            });
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(infos)
    }
}

impl WorldHost for EmbeddedSessionManager {
    fn descriptor(&self) -> HostDescriptor {
        HostDescriptor {
            id: "embedded".to_string(),
            kind: HostKind::Embedded,
            multi_user: false,
        }
    }

    fn worlds<'a>(&'a self) -> SessionFuture<'a, Result<Vec<WorldInfo>, SessionError>> {
        Box::pin(async move { self.do_worlds() })
    }

    fn open_world<'a>(
        &'a self,
        spec: WorldSpec,
    ) -> SessionFuture<'a, Result<WorldHandle, SessionError>> {
        Box::pin(async move { self.do_open_world(spec) })
    }

    fn close_world<'a>(&'a self, id: &'a WorldId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let mut state = lock(&self.state)?;
            if !state.meta.contains_key(id) {
                return Err(SessionError::WorldNotFound { id: id.0.clone() });
            }
            // Close is not delete: the store (and its file, if any) persists.
            state.open.remove(id);
            Ok(())
        })
    }

    fn vcs<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<Arc<dyn VcsStore>, SessionError>> {
        Box::pin(async move {
            self.require_open(id)?;
            self.store_for(id)
        })
    }

    fn compute<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<ComputeHandle, SessionError>> {
        Box::pin(async move {
            self.require_open(id)?;
            Ok(ComputeHandle::InProcess)
        })
    }

    fn sessions(&self) -> Option<&dyn SessionDirectory> {
        Some(&self.directory)
    }
}

/// The embedded host's single-user session directory.
///
/// Exactly one fixed identity (`local`, no groups) may join, and each world
/// holds at most one session. Joining is idempotent; joining as any other
/// identity is rejected with [`SessionError::Unauthorized`]. All state is
/// in-memory.
pub struct SingleUserDirectory {
    manager_state: Arc<Mutex<State>>,
    /// At most one session per world (the local user's).
    sessions: Mutex<HashMap<WorldId, WorldSession>>,
}

impl SingleUserDirectory {
    fn new(manager_state: Arc<Mutex<State>>) -> Self {
        Self {
            manager_state,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn local_user() -> UserIdentity {
        UserIdentity {
            name: LOCAL_USER.to_string(),
            groups: Vec::new(),
        }
    }

    fn require_world(&self, world: &WorldId) -> Result<(), SessionError> {
        if !lock(&self.manager_state)?.meta.contains_key(world) {
            return Err(SessionError::WorldNotFound {
                id: world.0.clone(),
            });
        }
        Ok(())
    }
}

impl SessionDirectory for SingleUserDirectory {
    fn join<'a>(
        &'a self,
        world: &'a WorldId,
        user: &'a UserIdentity,
    ) -> SessionFuture<'a, Result<WorldSession, SessionError>> {
        Box::pin(async move {
            self.require_world(world)?;
            if user.name != LOCAL_USER {
                return Err(SessionError::Unauthorized {
                    reason: format!(
                        "embedded host admits only the local user, got {:?}",
                        user.name
                    ),
                });
            }
            let mut sessions = lock(&self.sessions)?;
            if let Some(session) = sessions.get(world) {
                return Ok(session.clone());
            }
            let session = WorldSession {
                id: SessionId(format!("s-{}-{LOCAL_USER}", world.0)),
                world: world.clone(),
                user: Self::local_user(),
                joined_ts_ms: now_ms(),
            };
            sessions.insert(world.clone(), session.clone());
            Ok(session)
        })
    }

    fn leave<'a>(&'a self, session: &'a SessionId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let mut sessions = lock(&self.sessions)?;
            let before = sessions.len();
            sessions.retain(|_, s| s.id != *session);
            if sessions.len() == before {
                return Err(SessionError::SessionNotFound {
                    id: session.0.clone(),
                });
            }
            Ok(())
        })
    }

    fn sessions<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<Vec<WorldSession>, SessionError>> {
        Box::pin(async move {
            self.require_world(world)?;
            Ok(lock(&self.sessions)?
                .get(world)
                .cloned()
                .into_iter()
                .collect())
        })
    }

    fn members<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<WorldAcl, SessionError>> {
        Box::pin(async move {
            self.require_world(world)?;
            // The local user is the sole reader and writer.
            Ok(WorldAcl {
                readers: vec![LOCAL_USER.to_string()],
                writers: vec![LOCAL_USER.to_string()],
            })
        })
    }
}

fn lock<T>(mu: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, SessionError> {
    lock_mutex(mu)
}

/// Shared poison-tolerant mutex helper (the Kubernetes host uses it too).
pub(crate) fn lock_mutex<T>(mu: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, SessionError> {
    mu.lock().map_err(|_| {
        SessionError::Store(graph_vcs::VcsError::Store {
            message: "session-manager mutex poisoned".into(),
        })
    })
}

/// Minimal executor: the store's futures wrap synchronous work, so a single
/// poll always completes; no async runtime dependency. Mirrors the one in
/// `graph-vcs`'s tests.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};
    struct Nop;
    impl Wake for Nop {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(Nop));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// Epoch millis natively; on wasm32 (no reliable wall clock without js-sys)
/// a monotonic counter keeps timestamps ordering-meaningful. Same convention
/// as `graph-vcs`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn now_ms() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ── Browser persistence (wasm32) ────────────────────────────────────────────
//
// The `open_persistent` backend: minigraf 1.2.3 cannot persist a `VcsStore`
// to IndexedDB (its `browser` feature only exposes the JS-facing `BrowserDb`
// façade — `Rc<RefCell>` internals, string-Datalog API, no multi-command
// transactions, not `Send`/`Sync`), so the embedded host keeps in-memory
// stores and re-exports each world to localStorage after every successful
// mutation. On boot the stored exports are replayed through the same
// restore path as `import_world`.

/// Re-export one world and write it to localStorage.
#[cfg(target_arch = "wasm32")]
fn persist_world_now(
    state: &Arc<Mutex<State>>,
    id: &WorldId,
    store: &dyn VcsStore,
) -> Result<(), SessionError> {
    let meta = lock(state)?
        .meta
        .get(id)
        .cloned()
        .ok_or_else(|| SessionError::WorldNotFound { id: id.0.clone() })?;
    let export = export_from_store(
        &meta.id,
        &meta.name,
        meta.description.as_deref(),
        meta.created_ts_ms,
        store,
    )?;
    persist::write_world(&export)
}

/// A [`VcsStore`] that delegates to the world's real store and fires the
/// persistence hook after every successful mutation. Reads pass straight
/// through.
#[cfg(target_arch = "wasm32")]
struct PersistingStore {
    inner: Arc<dyn VcsStore>,
    hook: Arc<dyn Fn() + Send + Sync>,
}

#[cfg(target_arch = "wasm32")]
impl VcsStore for PersistingStore {
    fn head<'a>(&'a self, branch: &'a str) -> graph_vcs::VcsFuture<'a, Result<Option<graph_vcs::CommitId>, graph_vcs::VcsError>> {
        self.inner.head(branch)
    }

    fn commit<'a>(
        &'a self,
        branch: &'a str,
        ops: Vec<graph_vcs::GraphOp>,
        author: &'a str,
        message: &'a str,
    ) -> graph_vcs::VcsFuture<'a, Result<graph_vcs::Commit, graph_vcs::VcsError>> {
        Box::pin(async move {
            let result = self.inner.commit(branch, ops, author, message).await;
            if result.is_ok() {
                (self.hook)();
            }
            result
        })
    }

    fn log<'a>(
        &'a self,
        branch: &'a str,
        limit: usize,
    ) -> graph_vcs::VcsFuture<'a, Result<Vec<graph_vcs::Commit>, graph_vcs::VcsError>> {
        self.inner.log(branch, limit)
    }

    fn branches(&self) -> graph_vcs::VcsFuture<'_, Result<Vec<graph_vcs::BranchInfo>, graph_vcs::VcsError>> {
        self.inner.branches()
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        from: &'a graph_vcs::CommitId,
    ) -> graph_vcs::VcsFuture<'a, Result<(), graph_vcs::VcsError>> {
        Box::pin(async move {
            let result = self.inner.create_branch(name, from).await;
            if result.is_ok() {
                (self.hook)();
            }
            result
        })
    }

    fn merge<'a>(
        &'a self,
        into: &'a str,
        from: &'a str,
        author: &'a str,
        message: &'a str,
    ) -> graph_vcs::VcsFuture<'a, Result<graph_vcs::MergeReport, graph_vcs::VcsError>> {
        Box::pin(async move {
            let result = self.inner.merge(into, from, author, message).await;
            if result.is_ok() {
                (self.hook)();
            }
            result
        })
    }

    fn rebase<'a>(
        &'a self,
        branch: &'a str,
        onto: &'a str,
        author: &'a str,
    ) -> graph_vcs::VcsFuture<'a, Result<graph_vcs::RebaseReport, graph_vcs::VcsError>> {
        Box::pin(async move {
            let result = self.inner.rebase(branch, onto, author).await;
            if result.is_ok() {
                (self.hook)();
            }
            result
        })
    }

    fn conflicts<'a>(
        &'a self,
        branch: &'a str,
    ) -> graph_vcs::VcsFuture<'a, Result<Vec<graph_vcs::Conflict>, graph_vcs::VcsError>> {
        self.inner.conflicts(branch)
    }

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        resolutions: Vec<graph_vcs::ConflictResolution>,
        author: &'a str,
    ) -> graph_vcs::VcsFuture<'a, Result<graph_vcs::Commit, graph_vcs::VcsError>> {
        Box::pin(async move {
            let result = self.inner.resolve(branch, resolutions, author).await;
            if result.is_ok() {
                (self.hook)();
            }
            result
        })
    }

    fn diff<'a>(
        &'a self,
        a: &'a graph_vcs::CommitId,
        b: &'a graph_vcs::CommitId,
    ) -> graph_vcs::VcsFuture<'a, Result<Vec<graph_vcs::GraphOp>, graph_vcs::VcsError>> {
        self.inner.diff(a, b)
    }

    fn materialize<'a>(
        &'a self,
        commit: &'a graph_vcs::CommitId,
    ) -> graph_vcs::VcsFuture<'a, Result<graph_vcs::Snapshot, graph_vcs::VcsError>> {
        self.inner.materialize(commit)
    }

    fn op_log(
        &self,
        limit: usize,
    ) -> graph_vcs::VcsFuture<'_, Result<Vec<graph_vcs::OpLogEntry>, graph_vcs::VcsError>> {
        self.inner.op_log(limit)
    }
}

/// localStorage glue for the `open_persistent` backend. Keys:
/// `jump-cannon.embedded-worlds.index` (JSON array of world slugs) and
/// `jump-cannon.embedded-worlds.world.<slug>` (one [`WorldExport`] JSON
/// document each). All writes are best-effort: quota and serialization
/// failures surface as `SessionError` to the caller, which swallows them —
/// the live in-memory store stays authoritative for the session.
#[cfg(target_arch = "wasm32")]
mod persist {
    use crate::export::WorldExport;
    use crate::types::SessionError;

    const INDEX_KEY: &str = "jump-cannon.embedded-worlds.index";

    fn world_key(slug: &str) -> String {
        format!("jump-cannon.embedded-worlds.world.{slug}")
    }

    fn store_err(message: impl Into<String>) -> SessionError {
        SessionError::Store(graph_vcs::VcsError::Store {
            message: message.into(),
        })
    }

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    fn read_index(storage: &web_sys::Storage) -> Vec<String> {
        storage
            .get_item(INDEX_KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Write one world's export and keep the slug index current.
    pub(crate) fn write_world(export: &WorldExport) -> Result<(), SessionError> {
        // No window (WebWorker, tests): nothing to persist to.
        let Some(storage) = storage() else { return Ok(()) };
        let json = serde_json::to_string(export)
            .map_err(|e| store_err(format!("world export encode failed: {e}")))?;
        storage
            .set_item(&world_key(&export.id.0), &json)
            .map_err(|e| store_err(format!("localStorage write failed (quota?): {e:?}")))?;
        let mut slugs = read_index(&storage);
        if !slugs.contains(&export.id.0) {
            slugs.push(export.id.0.clone());
            slugs.sort();
            let index_json = serde_json::to_string(&slugs)
                .map_err(|e| store_err(format!("world index encode failed: {e}")))?;
            storage
                .set_item(INDEX_KEY, &index_json)
                .map_err(|e| store_err(format!("localStorage write failed (quota?): {e:?}")))?;
        }
        Ok(())
    }

    /// Every stored world export, in slug order. Undecodable entries are
    /// dropped (the caller treats them as absent).
    pub(crate) fn load_all() -> Vec<WorldExport> {
        let Some(storage) = storage() else { return Vec::new() };
        read_index(&storage)
            .iter()
            .filter_map(|slug| {
                storage
                    .get_item(&world_key(slug))
                    .ok()
                    .flatten()
                    .and_then(|json| serde_json::from_str(&json).ok())
            })
            .collect()
    }
}
