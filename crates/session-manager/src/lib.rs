//! `session-manager`: the abstract world/session interface plus the embedded
//! (single-user) implementation.
//!
//! - [`WorldHost`] is the core interface: open/list/close worlds and reach
//!   each world's [`graph_vcs::VcsStore`] and compute endpoint. It is
//!   deliberately free of session-management machinery — valid for a single
//!   user as-is.
//! - [`SessionDirectory`] is the OPTIONAL multi-user capability, reached via
//!   [`WorldHost::sessions`]. `None` means the component is absent, not
//!   stubbed.
//! - [`EmbeddedSessionManager`] is the single-user host: one local user,
//!   one [`graph_vcs::MinigrafStore`] per world.
//! - [`conformance`] (feature `conformance`, always on for this crate's own
//!   tests) is a reusable assertion suite later implementations (Kubernetes,
//!   HTTP client) plug into.
//!
//! Like `graph-vcs`, the crate is pure Rust with no async runtime
//! dependency: the object-safe traits use boxed futures ([`SessionFuture`])
//! mirroring `graph_vcs::VcsFuture`, and implementations may be synchronous
//! inside. The crate compiles for wasm32 targets (wall-clock timestamps fall
//! back to a monotonic counter there, same convention as `graph-vcs`; the
//! file-backed constructor is native-only).

mod embedded;
mod export;
mod types;

#[cfg(all(feature = "server", target_arch = "wasm32"))]
compile_error!("the `server` feature is native-only (graph-api does not build for wasm32)");

#[cfg(feature = "server")]
pub mod gpu_broker;
#[cfg(feature = "server")]
pub mod kubernetes;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod world_importer;

pub mod http;

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;

#[cfg(test)]
mod tests;

pub use embedded::{EmbeddedSessionManager, SingleUserDirectory};
pub use export::{ExportedCommit, WorldExport, WORLD_EXPORT_FORMAT_VERSION};
pub use http::{HttpDirectory, HttpSessionManager, HttpVcsStore};
#[cfg(feature = "server")]
pub use gpu_broker::{GpuBroker, GpuBrokerConfig};
#[cfg(feature = "server")]
pub use kubernetes::{KubernetesDirectory, KubernetesSessionManager};
pub use types::{
    ComputeHandle, HostDescriptor, HostKind, SessionError, SessionId, UserIdentity, WorldAcl,
    WorldHandle, WorldId, WorldInfo, WorldSession, WorldSpec,
};

use graph_vcs::VcsStore;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Heap-allocated future used by the object-safe session traits, mirroring
/// the [`graph_vcs::VcsFuture`] convention.
pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A host of versioned worlds.
///
/// The core interface has NO session-management component: it is valid for
/// a single user with zero multi-user machinery. The boxed-future methods
/// keep the trait object-safe without an `async-trait` transformation;
/// implementations may be synchronous inside.
pub trait WorldHost: Send + Sync {
    /// Self-description: identity, kind, and whether multiple users exist.
    fn descriptor(&self) -> HostDescriptor;

    /// List all worlds, with cheap branch counts.
    fn worlds<'a>(&'a self) -> SessionFuture<'a, Result<Vec<WorldInfo>, SessionError>>;

    /// Open (creating if needed) the world named by `spec`. Opening a world
    /// that is already open fails with [`SessionError::WorldExists`].
    fn open_world<'a>(
        &'a self,
        spec: WorldSpec,
    ) -> SessionFuture<'a, Result<WorldHandle, SessionError>>;

    /// Close the world, invalidating outstanding handles. Close is NOT
    /// delete: a file-backed world persists and can be re-opened.
    fn close_world<'a>(&'a self, id: &'a WorldId) -> SessionFuture<'a, Result<(), SessionError>>;

    /// The version-control store of an open world.
    fn vcs<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<Arc<dyn VcsStore>, SessionError>>;

    /// The compute endpoint of an open world.
    fn compute<'a>(
        &'a self,
        id: &'a WorldId,
    ) -> SessionFuture<'a, Result<ComputeHandle, SessionError>>;

    /// Multi-user session management is an OPTIONAL capability.
    /// `None` in single-user mode — the component is absent from the
    /// interface, not stubbed. `Some` only where multiple users exist (or a
    /// single-user directory models the one local user, as the embedded
    /// host does).
    fn sessions(&self) -> Option<&dyn SessionDirectory> {
        None
    }
}

/// The optional multi-user capability of a [`WorldHost`].
pub trait SessionDirectory: Send + Sync {
    /// Attach `user` to `world`. Joining twice is idempotent: the same
    /// session is returned.
    fn join<'a>(
        &'a self,
        world: &'a WorldId,
        user: &'a UserIdentity,
    ) -> SessionFuture<'a, Result<WorldSession, SessionError>>;

    /// Detach one session.
    fn leave<'a>(&'a self, session: &'a SessionId) -> SessionFuture<'a, Result<(), SessionError>>;

    /// Live sessions on `world`.
    fn sessions<'a>(
        &'a self,
        world: &'a WorldId,
    ) -> SessionFuture<'a, Result<Vec<WorldSession>, SessionError>>;

    /// The access list of `world`.
    fn members<'a>(&'a self, world: &'a WorldId)
        -> SessionFuture<'a, Result<WorldAcl, SessionError>>;
}
