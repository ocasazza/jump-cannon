//! `graph-vcs`: a Jujutsu-inspired version-control core for [`VaultGraph`]
//! snapshots.
//!
//! - Commits carry stable [`ChangeId`]s and content-addressed [`CommitId`]s.
//! - Merges and rebases operate on whole snapshots (three-way, attribute-level
//!   for nodes, set-based for edges), like jj merges trees rather than patches.
//! - Conflicts are first-class recorded state: a merge or rebase can land with
//!   conflicts attached, and resolution is a separate later operation.
//! - [`VcsStore`] is the object-safe storage trait; [`MinigrafStore`] is the
//!   embedded implementation backed by a single-file minigraf database, and
//!   `TerminusStore` (feature `terminus`, native only) is the server
//!   implementation backed by a TerminusDB cluster. Both satisfy the shared
//!   parity suite in `contract` (feature `contract`).
//!
//! The core is pure Rust with no async runtime dependency: the trait's boxed
//! futures wrap synchronous work, and the crate compiles for wasm32 targets
//! (wall-clock timestamps fall back to a monotonic counter there; see
//! [`minigraf_store`]).

pub mod merge;
pub mod minigraf_store;
pub mod model;
pub mod store;
#[cfg(all(feature = "terminus", not(target_arch = "wasm32")))]
pub mod terminus_store;
#[cfg(any(test, feature = "contract"))]
pub mod contract;

#[cfg(test)]
mod tests;

pub use merge::{diff_snapshots, merge_snapshots, MergeOutcome, Snapshot};
pub use minigraf_store::MinigrafStore;
#[cfg(all(feature = "terminus", not(target_arch = "wasm32")))]
pub use terminus_store::{TerminusConfig, TerminusStore};
pub use model::{
    BranchInfo, BranchName, ChangeId, Commit, CommitId, Conflict, ConflictResolution, GraphOp,
    MergeReport, MergeStatus, NodeId, OpKind, OpLogEntry, RebaseReport, ResolvedNode, VcsError,
};
pub use store::{VcsFuture, VcsStore};

pub use vault_data::{EdgeId, VaultEdge, VaultNode};
