//! Core domain types for graph version control.
//!
//! The model follows Jujutsu: commits carry stable [`ChangeId`]s, merges and
//! rebases operate on snapshots rather than patches, and conflicts are
//! first-class recorded state that never blocks publication.

use serde::{Deserialize, Serialize};
use std::fmt;
use vault_data::{EdgeId, VaultNode};

/// Node identity within a versioned graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable, user-meaningful identity of a change across rebases.
///
/// Derived from a hash of author + message + ops at creation time; rebasing a
/// commit keeps its [`ChangeId`] while minting a new [`CommitId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChangeId(pub String);

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Content-addressed identity of one commit: a hash of its change id, parent
/// ids, and resulting snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommitId(pub String);

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Branch identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BranchName(pub String);

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One user-facing mutation against a graph snapshot.
///
/// No `PartialEq`: `VaultNode` does not implement it (compare via
/// `serde_json::to_value` where needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphOp {
    /// Insert or replace a node (matched by `node.id`).
    UpsertNode(VaultNode),
    /// Remove a node. Does not remove edges that reference it.
    DeleteNode(NodeId),
    /// Insert an edge. Edges have no attributes, so upsert is idempotent.
    UpsertEdge(vault_data::VaultEdge),
    /// Remove an edge by identity.
    DeleteEdge(EdgeId),
}

/// One commit: a snapshot transition recorded as the ops that produced it.
///
/// `ops` is the user-facing delta record used for log/diff display; the
/// store's merge machinery works on full snapshots, like jj merges trees
/// rather than patches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub change_id: ChangeId,
    pub parents: Vec<CommitId>,
    pub ops: Vec<GraphOp>,
    pub author: String,
    pub message: String,
    pub timestamp_ms: i64,
    /// Conflicts recorded on this commit. Non-blocking: a commit can land
    /// with conflicts and be resolved by a later `resolve` operation.
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
}

/// A recorded merge conflict on one node.
///
/// `None` on any side means the node is absent on that side (e.g.
/// delete-vs-edit has `ours: None`). The merged snapshot keeps the `ours`
/// value so publication never blocks; resolution is a later operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub node_id: NodeId,
    pub base: Option<VaultNode>,
    pub ours: Option<VaultNode>,
    pub theirs: Option<VaultNode>,
}

/// How one conflicted node should be resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedNode {
    /// Take the `ours` side of the recorded conflict (absent = delete).
    Ours,
    /// Take the `theirs` side of the recorded conflict (absent = delete).
    Theirs,
    /// Take an explicit replacement node.
    Explicit(VaultNode),
    /// Delete the node outright.
    Deleted,
}

/// One resolution decision for a conflicted node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub node_id: NodeId,
    pub choice: ResolvedNode,
}

/// A branch and its current head commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: BranchName,
    pub head: CommitId,
}

/// How a merge concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStatus {
    /// A new two-parent merge commit was created.
    Merged,
    /// `into` was an ancestor of `from`; the branch head moved without a new commit.
    FastForward,
    /// `from` was already an ancestor of `into`; nothing changed.
    UpToDate,
}

/// The result of a `merge` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    /// The head commit of the `into` branch after the merge.
    pub commit: Commit,
    /// Conflicts recorded on the merge commit (empty unless status is `merged`).
    pub conflicts: Vec<Conflict>,
    pub status: MergeStatus,
}

/// The result of a `rebase` operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseReport {
    /// The new head of the rebased branch.
    pub new_head: CommitId,
    /// The replayed commits, in the order they were created. Each keeps its
    /// original [`ChangeId`] and gets a fresh [`CommitId`].
    pub rebased: Vec<Commit>,
    /// All conflicts recorded across the replayed commits.
    pub conflicts: Vec<Conflict>,
}

/// The kind of entry in the operation log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Commit,
    Merge,
    Rebase,
    Branch,
    Resolve,
}

/// One entry in the store's operation log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpLogEntry {
    /// Monotonic sequence number, starting at 0.
    pub seq: u64,
    pub kind: OpKind,
    pub summary: String,
    pub timestamp_ms: i64,
}

/// Errors crossing the VCS store boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VcsError {
    #[error("unknown branch: {name}")]
    UnknownBranch { name: String },
    #[error("branch already exists: {name}")]
    BranchExists { name: String },
    #[error("unknown commit: {id}")]
    UnknownCommit { id: String },
    #[error("unknown conflict on node {node_id} for Ours/Theirs resolution")]
    UnknownConflict { node_id: String },
    #[error("no common ancestor between {a} and {b}")]
    NoCommonAncestor { a: String, b: String },
    /// The underlying store (minigraf) rejected an operation.
    #[error("store error: {message}")]
    Store { message: String },
    /// Facts read back from the store failed to decode.
    #[error("corrupt store: {message}")]
    Corrupt { message: String },
}
