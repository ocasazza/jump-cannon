//! The object-safe VCS store trait.

use crate::model::{
    BranchInfo, Commit, CommitId, Conflict, ConflictResolution, GraphOp, MergeReport, OpLogEntry,
    RebaseReport, VcsError,
};
use crate::merge::Snapshot;
use std::future::Future;
use std::pin::Pin;

/// Heap-allocated future used by the object-safe [`VcsStore`] trait, mirroring
/// the `ImportFuture` convention in `data-loader`.
pub type VcsFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A version-control store for graph snapshots.
///
/// The boxed-future methods keep the trait object-safe without an
/// `async-trait` transformation. Implementations may be synchronous inside
/// (the minigraf store is); the future is just the calling convention.
pub trait VcsStore: Send + Sync {
    /// The current head of `branch`, or `None` if the branch does not exist.
    fn head<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Option<CommitId>, VcsError>>;

    /// Apply `ops` on top of the branch head and record a new commit.
    /// A missing branch is created, rooted at the empty snapshot.
    fn commit<'a>(
        &'a self,
        branch: &'a str,
        ops: Vec<GraphOp>,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>>;

    /// First-parent walk from the branch head, newest first, up to `limit`.
    fn log<'a>(
        &'a self,
        branch: &'a str,
        limit: usize,
    ) -> VcsFuture<'a, Result<Vec<Commit>, VcsError>>;

    /// All branches and their heads.
    fn branches(&self) -> VcsFuture<'_, Result<Vec<BranchInfo>, VcsError>>;

    /// Create a branch pointing at `from`. Fails if the branch already exists.
    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        from: &'a CommitId,
    ) -> VcsFuture<'a, Result<(), VcsError>>;

    /// Three-way merge of `from` into `into`, recording a two-parent commit
    /// on `into`. Conflicts are recorded, never blocking. Fast-forwards and
    /// up-to-date merges do not create a commit (see [`crate::MergeStatus`]).
    fn merge<'a>(
        &'a self,
        into: &'a str,
        from: &'a str,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<MergeReport, VcsError>>;

    /// Replay the commits of `branch` that are not already ancestors of
    /// `onto`'s head onto that head. Replayed commits keep their
    /// [`crate::ChangeId`] and get fresh [`CommitId`]s.
    fn rebase<'a>(
        &'a self,
        branch: &'a str,
        onto: &'a str,
        author: &'a str,
    ) -> VcsFuture<'a, Result<RebaseReport, VcsError>>;

    /// Conflicts recorded on the branch head commit.
    fn conflicts<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Vec<Conflict>, VcsError>>;

    /// Apply resolutions to the branch head's conflicts and record a new
    /// commit. Resolved conflicts are cleared; unresolved ones carry forward.
    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        resolutions: Vec<ConflictResolution>,
        author: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>>;

    /// The user-facing delta between two commits' snapshots.
    fn diff<'a>(
        &'a self,
        a: &'a CommitId,
        b: &'a CommitId,
    ) -> VcsFuture<'a, Result<Vec<GraphOp>, VcsError>>;

    /// The full snapshot recorded at one commit.
    fn materialize<'a>(&'a self, commit: &'a CommitId) -> VcsFuture<'a, Result<Snapshot, VcsError>>;

    /// The store's operation log, newest first, up to `limit`.
    fn op_log(&self, limit: usize) -> VcsFuture<'_, Result<Vec<OpLogEntry>, VcsError>>;
}
