//! World export/import: a portable, serde-round-trippable dump of one
//! world's full VCS state.
//!
//! A [`WorldExport`] carries the world metadata plus every commit (with its
//! materialized snapshot) reachable from every branch head, the branch
//! heads themselves, and the op log. Export walks each branch's first-parent
//! log and materializes each commit through the [`VcsStore`] API, so it
//! works against any store implementation; import replays the records
//! verbatim through [`MinigrafStore::restore_commit`] /
//! [`MinigrafStore::restore_branch`], so commit ids, timestamps, authors,
//! and conflicts survive the round trip unchanged.
//!
//! The same structure doubles as the wasm32 persistence format: the
//! embedded host's browser backend (see `embedded::persist`) writes one
//! export per world to localStorage after every mutation and replays it on
//! boot. Both consumers want full fidelity, so nothing is elided — exports
//! grow with history length (every commit carries its full snapshot, per
//! the store's v1 design).

use crate::embedded::block_on;
use crate::types::{SessionError, WorldId};
use graph_vcs::{BranchInfo, Commit, MinigrafStore, OpLogEntry, Snapshot, VcsStore};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Version of the [`WorldExport`] wire shape. Bump on any breaking change;
/// importers reject anything but the version they were built with.
pub const WORLD_EXPORT_FORMAT_VERSION: u32 = 1;

/// One commit and the full snapshot recorded at it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedCommit {
    pub commit: Commit,
    pub snapshot: Snapshot,
}

/// A complete, portable dump of one world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldExport {
    pub format_version: u32,
    pub id: WorldId,
    pub name: String,
    pub description: Option<String>,
    pub created_ts_ms: i64,
    /// Branch heads at export time.
    pub branches: Vec<BranchInfo>,
    /// Every commit reachable from any branch head, parents before
    /// children (import relies on the order).
    pub commits: Vec<ExportedCommit>,
    /// The op log at export time. Informational: import records its own
    /// restore operations rather than replaying these entries.
    pub op_log: Vec<OpLogEntry>,
}

fn corrupt(message: impl Into<String>) -> SessionError {
    SessionError::Store(graph_vcs::VcsError::Corrupt {
        message: message.into(),
    })
}

/// Build the export from a store handle. `meta` supplies the listing fields
/// the store itself does not know.
pub(crate) fn export_from_store(
    id: &WorldId,
    name: &str,
    description: Option<&str>,
    created_ts_ms: i64,
    store: &dyn VcsStore,
) -> Result<WorldExport, SessionError> {
    let branches = block_on(store.branches())?;
    // Union of every branch's first-parent log, deduplicated by commit id.
    let mut by_id: HashMap<String, Commit> = HashMap::new();
    for branch in &branches {
        for commit in block_on(store.log(&branch.name.0, usize::MAX))? {
            by_id.entry(commit.id.0.clone()).or_insert(commit);
        }
    }
    let order = topo_order(&by_id)?;
    let mut commits = Vec::with_capacity(order.len());
    for cid in &order {
        let commit = by_id
            .get(cid)
            .expect("topo order only emits ids present in the map")
            .clone();
        let snapshot = block_on(store.materialize(&commit.id))?;
        commits.push(ExportedCommit { commit, snapshot });
    }
    Ok(WorldExport {
        format_version: WORLD_EXPORT_FORMAT_VERSION,
        id: id.clone(),
        name: name.to_string(),
        description: description.map(str::to_string),
        created_ts_ms,
        branches,
        commits,
        op_log: block_on(store.op_log(usize::MAX))?,
    })
}

/// Replay an export into a fresh store, preserving every commit id and
/// branch head. The store must be empty (import always targets a brand-new
/// world file / in-memory store).
pub(crate) fn restore_into_store(
    store: &MinigrafStore,
    export: &WorldExport,
) -> Result<(), SessionError> {
    if export.format_version != WORLD_EXPORT_FORMAT_VERSION {
        return Err(corrupt(format!(
            "unsupported world export format version {} (this build reads {WORLD_EXPORT_FORMAT_VERSION})",
            export.format_version
        )));
    }
    let known: HashSet<&str> = export
        .commits
        .iter()
        .map(|c| c.commit.id.0.as_str())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    for exported in &export.commits {
        for parent in &exported.commit.parents {
            if !known.contains(parent.0.as_str()) {
                return Err(corrupt(format!(
                    "commit {} has parent {} missing from the export",
                    exported.commit.id, parent
                )));
            }
            if !seen.contains(&parent.0) {
                return Err(corrupt(format!(
                    "commit {} appears before its parent {} (export is not parents-first)",
                    exported.commit.id, parent
                )));
            }
        }
        store.restore_commit(exported.commit.clone(), exported.snapshot.clone())?;
        seen.insert(exported.commit.id.0.clone());
    }
    for branch in &export.branches {
        if !seen.contains(&branch.head.0) {
            return Err(corrupt(format!(
                "branch {} points at commit {} missing from the export",
                branch.name, branch.head
            )));
        }
        store.restore_branch(&branch.name.0, &branch.head)?;
    }
    Ok(())
}

/// Emit commit ids parents-first. Ties break by id for determinism (same
/// Kahn discipline as the store's rebase replay).
fn topo_order(commits: &HashMap<String, Commit>) -> Result<Vec<String>, SessionError> {
    let mut remaining: HashSet<String> = commits.keys().cloned().collect();
    let mut order = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let mut ready: Vec<String> = remaining
            .iter()
            .filter(|id| {
                commits
                    .get(*id)
                    .map(|c| c.parents.iter().all(|p| !remaining.contains(&p.0)))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        ready.sort();
        if ready.is_empty() {
            return Err(corrupt("commit graph has a parent cycle"));
        }
        for id in ready {
            remaining.remove(&id);
            order.push(id);
        }
    }
    Ok(order)
}
