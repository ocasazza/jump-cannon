//! The multi-user [`WorldHost`] backing the session-manager server (feature
//! `server`, native only).
//!
//! VCS persistence is selectable per server (`--store` /
//! `JUMP_CANNON_SM_STORE`): the default `minigraf` backend mirrors the
//! embedded host with one `<world-slug>.graph` file per world under the
//! worlds directory, while `terminusdb` opens one TerminusDB database per
//! world ([`graph_vcs::TerminusStore`]). Either way a `worlds.json` manifest
//! under the worlds directory carries the listing metadata and each world's
//! [`WorldAcl`]. On boot the manager adopts every existing world file
//! (closed, not opened), so a restart never loses a world; with the
//! terminusdb backend the manifest alone is authoritative (there are no
//! world files to adopt).
//!
//! Session state is in-memory only: joins do not survive a restart. The
//! durable record of who changed what lives in each world's own VCS op log.
//!
//! The access policy in this milestone is deliberately coarse: world
//! creators become the sole writer of their world; writers may
//! commit/merge/rebase/resolve/close and rewrite the ACL; ANY authenticated
//! user may read (worlds, logs, search, graph serving). Readers-as-a-list is
//! recorded in the ACL but not yet enforced.

use crate::embedded::{block_on, lock_mutex as lock, now_ms};
use crate::gpu_broker::GpuBroker;
use crate::types::{
    ComputeHandle, HostDescriptor, HostKind, SessionError, SessionId, UserIdentity, WorldAcl,
    WorldHandle, WorldId, WorldInfo, WorldSession, WorldSpec,
};
use crate::{SessionDirectory, SessionFuture, WorldHost};
use graph_vcs::{MinigrafStore, VcsStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The branch every world is born with, and the branch the server
/// materializes for graph serving.
pub const MAIN_BRANCH: &str = "main";

/// VCS persistence backend for the world's stores, selected once at server
/// startup (`--store` / `JUMP_CANNON_SM_STORE`).
#[derive(Debug, Clone)]
pub enum StoreBackend {
    /// One `<world-slug>.graph` minigraf file per world (default).
    Minigraf,
    /// One TerminusDB database per world; the manifest still lives on disk.
    Terminusdb(graph_vcs::TerminusConfig),
}

/// Creator recorded when a world is opened through the identity-less
/// [`WorldHost::open_world`] trait method instead of the server's
/// authenticated [`KubernetesSessionManager::open_world_as`].
const SYSTEM_USER: &str = "system";

/// Persisted metadata for one world (the `worlds.json` manifest; everything
/// else lives in the world's own minigraf file). `acl` defaults to empty so
/// manifests written before ACLs existed still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldMeta {
    id: WorldId,
    name: String,
    description: Option<String>,
    created_ts_ms: i64,
    #[serde(default)]
    acl: WorldAcl,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorldsManifest {
    worlds: Vec<WorldMeta>,
}

/// Mutable manager state behind one lock.
struct State {
    /// Persisted metadata for every known world (open or closed).
    meta: HashMap<WorldId, WorldMeta>,
    /// Opened stores, at most one `VcsStore` per world. Lazily
    /// populated for closed worlds when a listing needs branch counts.
    stores: HashMap<WorldId, Arc<dyn VcsStore>>,
    /// Worlds currently open. `vcs`/`compute` require an entry here;
    /// `close_world` removes it (the store cache may stay).
    open: HashSet<WorldId>,
}

/// The Kubernetes multi-user host. See the module docs for the persistence
/// and access-control model.
pub struct KubernetesSessionManager {
    root: PathBuf,
    backend: StoreBackend,
    state: Arc<Mutex<State>>,
    directory: KubernetesDirectory,
    /// Optional per-world GPU compute broker, installed once at server
    /// startup (`None` = no template mounted → `compute` reports
    /// [`ComputeHandle::Null`]). A std `RwLock` (not tokio): set once before
    /// serving, read on `compute` and the session endpoints.
    gpu: std::sync::RwLock<Option<GpuBroker>>,
}

impl KubernetesSessionManager {
    /// Open (or create) a manager rooted at `root` with the default minigraf
    /// backend. See [`KubernetesSessionManager::open_with_backend`].
    pub fn open(root: &Path) -> Result<Self, SessionError> {
        Self::open_with_backend(root, StoreBackend::Minigraf)
    }

    /// Open (or create) a manager rooted at `root`, adopting every existing
    /// world file. The manifest is authoritative for metadata and ACLs;
    /// stray `*.graph` files without manifest entries are adopted (closed,
    /// with an empty ACL) so a lost manifest never orphans a world.
    pub fn open_with_backend(root: &Path, backend: StoreBackend) -> Result<Self, SessionError> {
        std::fs::create_dir_all(root)?;
        let mut meta = HashMap::new();
        let manifest_path = root.join("worlds.json");
        if manifest_path.exists() {
            let manifest: WorldsManifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?).map_err(|e| {
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
                    acl: WorldAcl::default(),
                });
            }
        }
        let state = Arc::new(Mutex::new(State {
            meta,
            stores: HashMap::new(),
            open: HashSet::new(),
        }));
        Ok(Self {
            root: root.to_path_buf(),
            backend,
            state: state.clone(),
            directory: KubernetesDirectory::new(state),
            gpu: std::sync::RwLock::new(None),
        })
    }

    /// Construct the VCS store for one world on the selected backend.
    fn open_world_store(&self, id: &WorldId) -> Result<Arc<dyn VcsStore>, SessionError> {
        match &self.backend {
            StoreBackend::Minigraf => {
                Ok(Arc::new(MinigrafStore::open(self.root.join(format!("{}.graph", id.0)))?))
            }
            StoreBackend::Terminusdb(config) => {
                Ok(Arc::new(graph_vcs::TerminusStore::new(config.clone(), &id.0)?))
            }
        }
    }

    /// Install (or clear) the per-world GPU compute broker. Called once by
    /// the server binary after the kube client resolves; `None` keeps every
    /// world's compute handle at [`ComputeHandle::Null`].
    pub fn set_gpu_broker(&self, broker: Option<GpuBroker>) {
        *self
            .gpu
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = broker;
    }

    /// The installed GPU broker, if any.
    pub fn gpu_broker(&self) -> Option<GpuBroker> {
        self.gpu
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The directory holding the world files (used as the worlds'
    /// `vault_root` for graph serving).
    pub fn worlds_dir(&self) -> &Path {
        &self.root
    }

    /// Open (creating if needed) a world with `creator` recorded as its sole
    /// writer. This is the server's authenticated entry point; the
    /// identity-less trait method records [`SYSTEM_USER`] instead.
    pub fn open_world_as(
        &self,
        spec: WorldSpec,
        creator: &UserIdentity,
    ) -> Result<WorldHandle, SessionError> {
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
            // `main` by its creator.
            let store = self.open_world_store(&id)?;
            // The boxed future wraps synchronous store work; the trivial
            // executor drives it (same convention as the embedded host).
            block_on(store.commit(MAIN_BRANCH, Vec::new(), &creator.name, "world created"))?;
            let mut state = lock(&self.state)?;
            state.stores.insert(id.clone(), store);
            state.meta.insert(
                id.clone(),
                WorldMeta {
                    id: id.clone(),
                    name: spec.name,
                    description: spec.description,
                    created_ts_ms: now_ms(),
                    acl: WorldAcl {
                        readers: Vec::new(),
                        writers: vec![creator.name.clone()],
                    },
                },
            );
            self.persist_manifest(&state)?;
        }
        lock(&self.state)?.open.insert(id.clone());
        Ok(WorldHandle { id })
    }

    /// The store of an OPEN world. Worlds the manager knows but that are
    /// closed fail with [`SessionError::WorldNotFound`], matching `vcs`.
    pub fn open_store(&self, id: &WorldId) -> Result<Arc<dyn VcsStore>, SessionError> {
        self.require_open(id)?;
        self.store_for(id)
    }

    /// Listing row for one OPEN world (branch count included).
    pub fn world_info(&self, id: &WorldId) -> Result<WorldInfo, SessionError> {
        self.require_open(id)?;
        let meta = lock(&self.state)?.meta.get(id).cloned().ok_or_else(|| {
            SessionError::WorldNotFound { id: id.0.clone() }
        })?;
        self.info_for(meta)
    }

    /// Replace a world's ACL (writer-only; enforced by the server layer).
    pub fn set_acl(&self, id: &WorldId, acl: WorldAcl) -> Result<(), SessionError> {
        let mut state = lock(&self.state)?;
        let Some(meta) = state.meta.get_mut(id) else {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        };
        meta.acl = acl;
        self.persist_manifest(&state)?;
        Ok(())
    }

    /// A world's stored ACL.
    pub fn acl(&self, id: &WorldId) -> Result<WorldAcl, SessionError> {
        let state = lock(&self.state)?;
        let Some(meta) = state.meta.get(id) else {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        };
        Ok(meta.acl.clone())
    }

    /// Whether `user` may mutate `id`: the user's name or one of their
    /// groups appears in the world's writers list.
    pub fn is_writer(&self, id: &WorldId, user: &UserIdentity) -> Result<bool, SessionError> {
        Ok(self.acl(id)?.writers.iter().any(|entry| {
            entry == &user.name || user.groups.iter().any(|group| group == entry)
        }))
    }

    /// The store for `id`, opening and caching the world store on demand.
    /// Works for open and closed worlds; fails only for unknown ones. The
    /// state lock is dropped while the store opens (a TerminusDB open does
    /// blocking HTTP); a concurrent opener would construct the same store
    /// idempotently and lose the cache insert, which is harmless.
    fn store_for(&self, id: &WorldId) -> Result<Arc<dyn VcsStore>, SessionError> {
        let state = lock(&self.state)?;
        if let Some(store) = state.stores.get(id) {
            return Ok(store.clone());
        }
        if !state.meta.contains_key(id) {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        }
        drop(state);
        let store = self.open_world_store(id)?;
        lock(&self.state)?.stores.insert(id.clone(), store.clone());
        Ok(store)
    }

    /// Persist `worlds.json`.
    fn persist_manifest(&self, state: &State) -> Result<(), SessionError> {
        let manifest = WorldsManifest {
            worlds: state.meta.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&manifest).map_err(|e| {
            SessionError::Store(graph_vcs::VcsError::Store {
                message: format!("manifest encode failed: {e}"),
            })
        })?;
        std::fs::write(self.root.join("worlds.json"), json)?;
        Ok(())
    }

    fn require_open(&self, id: &WorldId) -> Result<(), SessionError> {
        let state = lock(&self.state)?;
        if !state.meta.contains_key(id) || !state.open.contains(id) {
            return Err(SessionError::WorldNotFound { id: id.0.clone() });
        }
        Ok(())
    }

    fn info_for(&self, meta: WorldMeta) -> Result<WorldInfo, SessionError> {
        // File-backed minigraf can surface duplicate datoms for a
        // re-asserted branch name, so count DISTINCT branch names.
        let branches = block_on(self.store_for(&meta.id)?.branches())?;
        let names: std::collections::BTreeSet<&str> =
            branches.iter().map(|b| b.name.0.as_str()).collect();
        Ok(WorldInfo {
            id: meta.id,
            name: meta.name,
            description: meta.description,
            created_ts_ms: meta.created_ts_ms,
            branches: names.len(),
        })
    }

    fn do_worlds(&self) -> Result<Vec<WorldInfo>, SessionError> {
        let metas: Vec<WorldMeta> = lock(&self.state)?.meta.values().cloned().collect();
        let mut infos = Vec::with_capacity(metas.len());
        for meta in metas {
            infos.push(self.info_for(meta)?);
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(infos)
    }
}

impl WorldHost for KubernetesSessionManager {
    fn descriptor(&self) -> HostDescriptor {
        HostDescriptor {
            id: "kubernetes".to_string(),
            kind: HostKind::Kubernetes,
            multi_user: true,
        }
    }

    fn worlds<'a>(&'a self) -> SessionFuture<'a, Result<Vec<WorldInfo>, SessionError>> {
        Box::pin(async move { self.do_worlds() })
    }

    fn open_world<'a>(
        &'a self,
        spec: WorldSpec,
    ) -> SessionFuture<'a, Result<WorldHandle, SessionError>> {
        Box::pin(async move {
            self.open_world_as(
                spec,
                &UserIdentity {
                    name: SYSTEM_USER.to_string(),
                    groups: Vec::new(),
                },
            )
        })
    }

    fn close_world<'a>(&'a self, id: &'a WorldId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let mut state = lock(&self.state)?;
            if !state.meta.contains_key(id) {
                return Err(SessionError::WorldNotFound { id: id.0.clone() });
            }
            // Close is not delete: the store (and its file) persists.
            state.open.remove(id);
            Ok(())
        })
    }

    fn vcs<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<Arc<dyn VcsStore>, SessionError>> {
        Box::pin(async move { self.open_store(id) })
    }

    fn compute<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<ComputeHandle, SessionError>> {
        Box::pin(async move {
            self.require_open(id)?;
            // With the GPU broker installed the world has an on-demand
            // compute endpoint: the per-world Service DNS name is stable
            // whether or not a session is currently dispatched (zero
            // endpoints while parked); dispatch happens via the REST API.
            match self.gpu_broker() {
                Some(broker) => Ok(ComputeHandle::RemoteGrpc {
                    url: broker.compute_url(id),
                }),
                None => Ok(ComputeHandle::Null),
            }
        })
    }

    fn sessions(&self) -> Option<&dyn SessionDirectory> {
        Some(&self.directory)
    }
}

/// The Kubernetes host's session directory.
///
/// Any authenticated identity may join any known world (the coarse readers
/// policy above); joining twice is idempotent per (world, user). Sessions
/// are in-memory only.
pub struct KubernetesDirectory {
    manager_state: Arc<Mutex<State>>,
    /// Live sessions per world.
    sessions: Mutex<HashMap<WorldId, Vec<WorldSession>>>,
    next_session: AtomicU64,
}

impl KubernetesDirectory {
    fn new(manager_state: Arc<Mutex<State>>) -> Self {
        Self {
            manager_state,
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
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

impl SessionDirectory for KubernetesDirectory {
    fn join<'a>(
        &'a self,
        world: &'a WorldId,
        user: &'a UserIdentity,
    ) -> SessionFuture<'a, Result<WorldSession, SessionError>> {
        Box::pin(async move {
            self.require_world(world)?;
            let mut sessions = lock(&self.sessions)?;
            let list = sessions.entry(world.clone()).or_default();
            if let Some(session) = list.iter().find(|s| s.user.name == user.name) {
                return Ok(session.clone());
            }
            let seq = self.next_session.fetch_add(1, Ordering::Relaxed);
            let session = WorldSession {
                id: SessionId(format!("s-{}-{}-{seq}", world.0, user.name)),
                world: world.clone(),
                user: user.clone(),
                joined_ts_ms: now_ms(),
            };
            list.push(session.clone());
            Ok(session)
        })
    }

    fn leave<'a>(&'a self, session: &'a SessionId) -> SessionFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            let mut sessions = lock(&self.sessions)?;
            let before: usize = sessions.values().map(Vec::len).sum();
            sessions.retain(|_, list| {
                list.retain(|s| &s.id != session);
                !list.is_empty()
            });
            if sessions.values().map(Vec::len).sum::<usize>() == before {
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
                .unwrap_or_default())
        })
    }

    fn members<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<WorldAcl, SessionError>> {
        Box::pin(async move {
            self.require_world(world)?;
            let state = lock(&self.manager_state)?;
            Ok(state
                .meta
                .get(world)
                .map(|meta| meta.acl.clone())
                .unwrap_or_default())
        })
    }
}
