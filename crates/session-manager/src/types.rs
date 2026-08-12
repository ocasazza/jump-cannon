//! Shared types for the session-manager interface.
//!
//! These types are plain data (serde round-trippable) so the same wire
//! shapes can cross an HTTP or gRPC boundary in later implementations.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Validated world identifier: a slug of `[A-Za-z0-9._-]`, non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorldId(pub String);

impl WorldId {
    /// Validate an explicit slug.
    pub fn parse(slug: &str) -> Result<Self, SessionError> {
        if slug.is_empty()
            || !slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(SessionError::InvalidName {
                name: slug.to_string(),
            });
        }
        Ok(Self(slug.to_string()))
    }

    /// Derive a slug from a free-form display name: lowercased, any run of
    /// non-slug characters collapsed to one `-`, leading/trailing `-`
    /// trimmed. Fails when nothing slug-like remains.
    pub fn from_name(name: &str) -> Result<Self, SessionError> {
        let mut slug = String::with_capacity(name.len());
        let mut pending_dash = false;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                if pending_dash && !slug.is_empty() {
                    slug.push('-');
                }
                pending_dash = false;
                slug.push(c.to_ascii_lowercase());
            } else if matches!(c, '.' | '_' | '-') {
                if pending_dash && !slug.is_empty() {
                    slug.push('-');
                }
                pending_dash = false;
                slug.push(c);
            } else {
                pending_dash = true;
            }
        }
        Self::parse(&slug)
    }
}

impl fmt::Display for WorldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to create in [`crate::WorldHost::open_world`]. The [`WorldId`] is
/// derived from `name` via [`WorldId::from_name`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSpec {
    pub name: String,
    pub description: Option<String>,
}

/// One row of a world listing. `branches` is a cheap count only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldInfo {
    pub id: WorldId,
    pub name: String,
    pub description: Option<String>,
    pub created_ts_ms: i64,
    pub branches: usize,
}

/// Handle to an open world. Closing via [`crate::WorldHost::close_world`]
/// invalidates the handle; it never deletes the world's data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldHandle {
    pub id: WorldId,
}

/// Identity of one live session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who is attached to a world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentity {
    pub name: String,
    pub groups: Vec<String>,
}

/// One user's attachment to one world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSession {
    pub id: SessionId,
    pub world: WorldId,
    pub user: UserIdentity,
    pub joined_ts_ms: i64,
}

/// Access list of a world: user (or group) names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldAcl {
    pub readers: Vec<String>,
    pub writers: Vec<String>,
}

/// What kind of host is behind a [`crate::WorldHost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// Single-process, single-user host (embedded store).
    Embedded,
    /// Kubernetes-orchestrated multi-user host.
    Kubernetes,
    /// A remote host reached over the network.
    Remote,
}

/// Self-description of a host, used by clients and the conformance suite to
/// pick expectations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDescriptor {
    pub id: String,
    pub kind: HostKind,
    pub multi_user: bool,
}

/// Opaque handle to the compute (layout solver) backing a world.
///
/// A plain data enum — the real layout/compute wiring lands in later
/// milestones; today this only tells the client where compute would run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeHandle {
    /// No compute available.
    Null,
    /// Compute runs in the host process.
    InProcess,
    /// Compute is a remote gRPC broker at `url`.
    RemoteGrpc { url: String },
}

/// Errors crossing the session-manager boundary.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("world not found: {id}")]
    WorldNotFound { id: String },
    #[error("world already exists: {id}")]
    WorldExists { id: String },
    #[error("invalid world name (need [A-Za-z0-9._-] after slugging): {name}")]
    InvalidName { name: String },
    #[error("session not found: {id}")]
    SessionNotFound { id: String },
    /// Reserved for multi-user implementations (Kubernetes): the identity is
    /// not permitted the operation.
    #[error("unauthorized: {reason}")]
    Unauthorized { reason: String },
    /// The underlying version-control store rejected an operation.
    #[error(transparent)]
    Store(#[from] graph_vcs::VcsError),
    /// Filesystem failure. Native-only: the wasm32 build has no filesystem.
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
