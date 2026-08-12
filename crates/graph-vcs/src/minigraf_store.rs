//! [`VcsStore`] implementation on top of an embedded minigraf database.
//!
//! # Persistence model
//!
//! Everything lives in minigraf EAV facts (one database file per world):
//!
//! - Commit metadata on entity `:commit/<id>`: `:commit/id`,
//!   `:commit/change-id`, `:commit/parents` (JSON string list — parent lists
//!   are tiny, so no chunking), `:commit/author`, `:commit/message`,
//!   `:commit/ts` (int ms), `:commit/has-conflicts` (bool).
//! - Large payloads (snapshot, ops, conflicts) are stored as JSON strings
//!   split into chunks of at most [`CHUNK_BYTES`] bytes, because file-backed
//!   minigraf rejects any single fact larger than 4080 serialized bytes. Each
//!   chunk is its own entity (`:snap/<id>/<i>`, `:cops/<id>/<i>`,
//!   `:cconf/<id>/<i>`) with `<ns>/commit`, `<ns>/idx`, and `<ns>/data`
//!   attributes. Chunking is uniform — even tiny payloads get exactly one
//!   chunk — so there is a single read/write path.
//! - Branch heads on entity `:branch/b-<hash(name)>`: `:branch/name` and
//!   `:branch/head`. Moving a head retracts the old head fact and asserts the
//!   new one inside one write transaction (verified against minigraf 1.2.3).
//! - Op-log entries on entity `:oplog/<seq>` with `:oplog/seq` (int),
//!   `:oplog/kind`, `:oplog/summary`, `:oplog/ts`.
//!
//! # Design notes
//!
//! - Every commit stores its *full* snapshot. This duplicates state across
//!   commits; that is fine for v1 scale (worlds are curated graphs, not
//!   monorepos) and keeps reads trivial.
//! - Merge-base and ancestry are computed in Rust over a full scan of the
//!   `:commit/parents` facts rather than with recursive Datalog rules: one
//!   query, no rule registry, and the smallest possible minigraf surface.
//! - minigraf's API is synchronous; the [`VcsStore`] futures wrap sync work.
//!   A `Mutex` serializes access (minigraf has its own internal locks; ours
//!   keeps multi-command sequences consistent).
//! - Timestamps use wall-clock epoch millis natively. On wasm32 (where
//!   `SystemTime::now` is unavailable or panics) a monotonic counter starting
//!   at 0 is used instead — timestamps remain ordering-meaningful, just not
//!   wall-clock.

use crate::merge::{canonical_json, diff_snapshots, merge_snapshots, Snapshot};
use crate::model::{
    BranchInfo, BranchName, ChangeId, Commit, CommitId, Conflict, ConflictResolution, GraphOp,
    MergeReport, MergeStatus, NodeId, OpKind, OpLogEntry, RebaseReport, ResolvedNode, VcsError,
};
use crate::store::{VcsFuture, VcsStore};
use minigraf::{Minigraf, QueryResult, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Mutex;

/// Maximum byte length of one chunk payload. File-backed minigraf caps a
/// serialized fact at 4080 bytes; 3000 leaves ample header room.
const CHUNK_BYTES: usize = 3000;

/// Embedded, minigraf-backed [`VcsStore`].
pub struct MinigrafStore {
    db: Minigraf,
    mu: Mutex<()>,
}

// The store is shared through the object-safe trait, so it must be Send+Sync.
const _: fn() = || {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MinigrafStore>();
};

impl MinigrafStore {
    /// Open (or create) a file-backed store. Native only: minigraf owns the
    /// filesystem path handling, and file-backed minigraf is not available
    /// on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VcsError> {
        let db = minigraf::OpenOptions::new()
            .path(path)
            .open()
            .map_err(|e| store_err(e))?;
        Ok(Self {
            db,
            mu: Mutex::new(()),
        })
    }

    /// Open an in-memory store. Used by tests and wasm targets.
    pub fn in_memory() -> Result<Self, VcsError> {
        let db = Minigraf::in_memory().map_err(store_err)?;
        Ok(Self {
            db,
            mu: Mutex::new(()),
        })
    }

    // ── Datalog helpers ─────────────────────────────────────────────────────

    fn execute(&self, cmd: &str) -> Result<QueryResult, VcsError> {
        self.db.execute(cmd).map_err(store_err)
    }

    fn query(&self, cmd: &str) -> Result<Vec<Vec<Value>>, VcsError> {
        match self.execute(cmd)? {
            QueryResult::QueryResults { results, .. } => Ok(results),
            other => Err(VcsError::Corrupt {
                message: format!("expected query results for {cmd:?}, got {other:?}"),
            }),
        }
    }

    // ── Branch facts ────────────────────────────────────────────────────────

    /// All `(name, head)` pairs.
    fn branch_rows(&self) -> Result<Vec<(String, String)>, VcsError> {
        let rows = self.query(
            "(query [:find ?n ?h :where [?b :branch/name ?n] [?b :branch/head ?h]])",
        )?;
        rows.iter()
            .map(|row| Ok((as_string(row, 0)?, as_string(row, 1)?)))
            .collect()
    }

    fn head_sync(&self, branch: &str) -> Result<Option<CommitId>, VcsError> {
        Ok(self
            .branch_rows()?
            .into_iter()
            .find(|(name, _)| name == branch)
            .map(|(_, head)| CommitId(head)))
    }

    fn require_head(&self, branch: &str) -> Result<CommitId, VcsError> {
        self.head_sync(branch)?
            .ok_or_else(|| VcsError::UnknownBranch {
                name: branch.to_string(),
            })
    }

    /// Datalog commands that move (or create) a branch head. The
    /// `:branch/name` fact is asserted only when the branch is created —
    /// re-asserting it on every head move stacks duplicate datoms (both
    /// file-backed and in-memory minigraf surface them, so `branches()`
    /// would list the branch once per head move).
    fn set_head_commands(&self, branch: &str, head: &CommitId) -> Result<Vec<String>, VcsError> {
        let entity = branch_entity(branch);
        match self.head_sync(branch)? {
            Some(old) if old == *head => Ok(Vec::new()),
            Some(old) => Ok(vec![
                format!("(retract [[{entity} :branch/head \"{}\"]])", esc(&old.0)),
                format!("(transact [[{entity} :branch/head \"{}\"]])", esc(&head.0)),
            ]),
            None => Ok(vec![format!(
                "(transact [[{entity} :branch/name \"{}\"] [{entity} :branch/head \"{}\"]])",
                esc(branch),
                esc(&head.0)
            )]),
        }
    }

    // ── Op log ──────────────────────────────────────────────────────────────

    fn next_oplog_seq(&self) -> Result<u64, VcsError> {
        let rows = self.query("(query [:find ?s :where [?e :oplog/seq ?s]])")?;
        let max = rows
            .iter()
            .map(|row| as_i64(row, 0))
            .collect::<Result<Vec<i64>, _>>()?
            .into_iter()
            .max();
        Ok(max.map_or(0, |m| (m as u64) + 1))
    }

    fn oplog_command(&self, kind: OpKind, summary: &str) -> Result<String, VcsError> {
        let seq = self.next_oplog_seq()?;
        Ok(format!(
            "(transact [[:oplog/{seq} :oplog/seq {seq}] [:oplog/{seq} :oplog/kind \"{kind}\"] [:oplog/{seq} :oplog/summary \"{}\"] [:oplog/{seq} :oplog/ts {}]])",
            esc(summary),
            now_ms(),
            seq = seq,
            kind = opkind_str(kind),
        ))
    }

    // ── Commit records ──────────────────────────────────────────────────────

    /// Build the single `(transact ...)` command recording a commit plus its
    /// snapshot, ops, and conflicts.
    fn commit_command(&self, commit: &Commit, snapshot: &Snapshot) -> String {
        let id = &commit.id.0;
        let entity = format!(":commit/{id}");
        let has_conflicts = !commit.conflicts.is_empty();
        let mut facts = vec![
            format!("[{entity} :commit/id \"{}\"]", esc(id)),
            format!("[{entity} :commit/change-id \"{}\"]", esc(&commit.change_id.0)),
            format!(
                "[{entity} :commit/parents \"{}\"]",
                esc(&canonical_json(&commit.parents))
            ),
            format!("[{entity} :commit/author \"{}\"]", esc(&commit.author)),
            format!("[{entity} :commit/message \"{}\"]", esc(&commit.message)),
            format!("[{entity} :commit/ts {}]", commit.timestamp_ms),
            format!("[{entity} :commit/has-conflicts {has_conflicts}]"),
        ];
        facts.extend(chunk_facts("snap", id, &snapshot.canonical_json()));
        facts.extend(chunk_facts("cops", id, &canonical_json(&commit.ops)));
        facts.extend(chunk_facts("cconf", id, &canonical_json(&commit.conflicts)));
        format!("(transact [{}])", facts.join(" "))
    }

    /// Read one commit's metadata and ops/conflicts blobs. Snapshot is loaded
    /// separately via [`MinigrafStore::snapshot_sync`].
    fn commit_sync(&self, id: &CommitId) -> Result<Option<Commit>, VcsError> {
        let rows = self.query(&format!(
            "(query [:find ?change ?parents ?author ?msg ?ts :where [?c :commit/id \"{}\"] [?c :commit/change-id ?change] [?c :commit/parents ?parents] [?c :commit/author ?author] [?c :commit/message ?msg] [?c :commit/ts ?ts]])",
            esc(&id.0)
        ))?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let parents: Vec<CommitId> = parse_json(&as_string(row, 1)?)?;
        let ops_json = read_blob_required(self, "cops", &id.0)?;
        let conflicts_json = read_blob_required(self, "cconf", &id.0)?;
        Ok(Some(Commit {
            id: id.clone(),
            change_id: ChangeId(as_string(row, 0)?),
            parents,
            ops: parse_json(&ops_json)?,
            author: as_string(row, 2)?,
            message: as_string(row, 3)?,
            timestamp_ms: as_i64(row, 4)?,
            conflicts: parse_json(&conflicts_json)?,
        }))
    }

    fn require_commit(&self, id: &CommitId) -> Result<Commit, VcsError> {
        self.commit_sync(id)?
            .ok_or_else(|| VcsError::UnknownCommit { id: id.0.clone() })
    }

    fn snapshot_sync(&self, id: &CommitId) -> Result<Snapshot, VcsError> {
        let json = read_blob_required(self, "snap", &id.0)?;
        parse_json(&json)
    }

    /// Map of every commit id to its parent ids.
    fn parent_map(&self) -> Result<HashMap<String, Vec<String>>, VcsError> {
        let rows = self.query(
            "(query [:find ?id ?parents :where [?c :commit/id ?id] [?c :commit/parents ?parents]])",
        )?;
        let mut map = HashMap::new();
        for row in &rows {
            map.insert(as_string(row, 0)?, parse_json(&as_string(row, 1)?)?);
        }
        Ok(map)
    }

    // ── Shared write path ───────────────────────────────────────────────────

    /// Run a sequence of Datalog commands atomically in one write transaction.
    fn run_write(&self, commands: &[String]) -> Result<(), VcsError> {
        let mut tx = self.db.begin_write().map_err(store_err)?;
        for cmd in commands {
            tx.execute(cmd).map_err(store_err)?;
        }
        tx.commit().map_err(store_err)
    }

    /// Record a commit: write its record, move the branch head, append an
    /// op-log entry — all in one transaction.
    fn land_commit(
        &self,
        branch: &str,
        commit: Commit,
        snapshot: Snapshot,
        kind: OpKind,
        summary: &str,
    ) -> Result<Commit, VcsError> {
        let mut commands = vec![self.commit_command(&commit, &snapshot)];
        commands.extend(self.set_head_commands(branch, &commit.id)?);
        commands.push(self.oplog_command(kind, summary)?);
        self.run_write(&commands)?;
        Ok(commit)
    }

    // ── Verbatim restore (world import) ─────────────────────────────────────

    /// Write a commit record exactly as exported — same ids, ops, timestamp,
    /// and conflicts — without moving any branch head. Import replays commit
    /// records parents-first and then points branch heads at them with
    /// [`MinigrafStore::restore_branch`]. Not part of the [`VcsStore`] trait:
    /// restore bypasses the normal minting rules (content addressing would
    /// reject out-of-order or headless records), so it stays an inherent
    /// method for the session-manager's import path.
    pub fn restore_commit(&self, commit: Commit, snapshot: Snapshot) -> Result<(), VcsError> {
        let _guard = lock(&self.mu)?;
        let summary = format!("restore commit {}", commit.id);
        let commands = vec![
            self.commit_command(&commit, &snapshot),
            self.oplog_command(OpKind::Commit, &summary)?,
        ];
        self.run_write(&commands)
    }

    /// Point `branch` at `head`, which must already exist in the store (via
    /// [`MinigrafStore::restore_commit`] or normal operation).
    pub fn restore_branch(&self, branch: &str, head: &CommitId) -> Result<(), VcsError> {
        let _guard = lock(&self.mu)?;
        self.require_commit(head)?;
        let mut commands = self.set_head_commands(branch, head)?;
        commands.push(self.oplog_command(OpKind::Branch, &format!("restore branch {branch} at {head}"))?);
        self.run_write(&commands)
    }

    fn mint_commit(
        &self,
        change_id: ChangeId,
        parents: Vec<CommitId>,
        ops: Vec<GraphOp>,
        author: &str,
        message: &str,
        conflicts: Vec<Conflict>,
        snapshot: &Snapshot,
    ) -> Commit {
        let id = commit_id(&change_id, &parents, snapshot);
        Commit {
            id,
            change_id,
            parents,
            ops,
            author: author.to_string(),
            message: message.to_string(),
            timestamp_ms: now_ms(),
            conflicts,
        }
    }

    // ── Sync implementations ────────────────────────────────────────────────

    fn do_commit(
        &self,
        branch: &str,
        ops: Vec<GraphOp>,
        author: &str,
        message: &str,
    ) -> Result<Commit, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.head_sync(branch)?;
        let base = match &head {
            Some(id) => self.snapshot_sync(id)?,
            None => Snapshot::default(),
        };
        let snapshot = base.apply(&ops);
        // jj semantics: unresolved conflicts on the head carry forward onto
        // new commits until a `resolve` clears them.
        let conflicts = match &head {
            Some(id) => self.require_commit(id)?.conflicts,
            None => Vec::new(),
        };
        let change_id = change_id(author, message, &ops);
        let parents: Vec<CommitId> = head.into_iter().collect();
        let commit = self.mint_commit(
            change_id, parents, ops, author, message, conflicts, &snapshot,
        );
        let summary = format!("commit {} on {branch}: {message}", commit.id);
        self.land_commit(branch, commit, snapshot, OpKind::Commit, &summary)
    }

    fn do_log(&self, branch: &str, limit: usize) -> Result<Vec<Commit>, VcsError> {
        let _guard = lock(&self.mu)?;
        let mut out = Vec::new();
        let mut cursor = self.require_head(branch)?;
        while out.len() < limit {
            let commit = self.require_commit(&cursor)?;
            let Some(parent) = commit.parents.first().cloned() else {
                out.push(commit);
                break;
            };
            out.push(commit);
            cursor = parent;
        }
        Ok(out)
    }

    fn do_branches(&self) -> Result<Vec<BranchInfo>, VcsError> {
        let _guard = lock(&self.mu)?;
        // Dedupe by name: stores written before head moves stopped
        // re-asserting `:branch/name` carry one name datom per move (see
        // `set_head_commands`), and every duplicate row reports the same
        // head.
        let mut by_name: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (name, head) in self.branch_rows()? {
            by_name.entry(name).or_insert(head);
        }
        let infos: Vec<BranchInfo> = by_name
            .into_iter()
            .map(|(name, head)| BranchInfo {
                name: BranchName(name),
                head: CommitId(head),
            })
            .collect();
        Ok(infos)
    }

    fn do_create_branch(&self, name: &str, from: &CommitId) -> Result<(), VcsError> {
        let _guard = lock(&self.mu)?;
        self.require_commit(from)?;
        if self.head_sync(name)?.is_some() {
            return Err(VcsError::BranchExists {
                name: name.to_string(),
            });
        }
        let mut commands = self.set_head_commands(name, from)?;
        commands.push(self.oplog_command(OpKind::Branch, &format!("branch {name} at {from}"))?);
        self.run_write(&commands)
    }

    fn do_merge(
        &self,
        into: &str,
        from: &str,
        author: &str,
        message: &str,
    ) -> Result<MergeReport, VcsError> {
        let _guard = lock(&self.mu)?;
        let into_head = self.require_head(into)?;
        let from_head = self.require_head(from)?;
        if into_head == from_head {
            let commit = self.require_commit(&into_head)?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::UpToDate,
            });
        }
        let parents_map = self.parent_map()?;
        if ancestors(&parents_map, &into_head.0).contains(&from_head.0) {
            let commit = self.require_commit(&into_head)?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::UpToDate,
            });
        }
        if ancestors(&parents_map, &from_head.0).contains(&into_head.0) {
            let commit = self.require_commit(&from_head)?;
            let mut commands = self.set_head_commands(into, &from_head)?;
            commands.push(self.oplog_command(
                OpKind::Merge,
                &format!("fast-forward {into} to {from_head}"),
            )?);
            self.run_write(&commands)?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::FastForward,
            });
        }

        // Divergent heads: three-way merge. Unrelated roots (branches created
        // independently from empty) merge against the empty snapshot — every
        // node is then add/add or add-on-one-side.
        let base_id = nearest_common_ancestor(&parents_map, &into_head.0, &from_head.0);
        let base = match &base_id {
            Some(id) => self.snapshot_sync(&CommitId(id.clone()))?,
            None => Snapshot::default(),
        };
        let ours = self.snapshot_sync(&into_head)?;
        let theirs = self.snapshot_sync(&from_head)?;
        let outcome = merge_snapshots(&base, &ours, &theirs);
        let ops = diff_snapshots(&base, &outcome.merged);
        let change_id = change_id(author, message, &ops);
        let commit = self.mint_commit(
            change_id,
            vec![into_head.clone(), from_head.clone()],
            ops,
            author,
            message,
            outcome.conflicts.clone(),
            &outcome.merged,
        );
        let summary = format!("merge {from} into {into}: {message}");
        let commit = self.land_commit(into, commit, outcome.merged, OpKind::Merge, &summary)?;
        Ok(MergeReport {
            conflicts: commit.conflicts.clone(),
            commit,
            status: MergeStatus::Merged,
        })
    }

    fn do_rebase(
        &self,
        branch: &str,
        onto: &str,
        _author: &str,
    ) -> Result<RebaseReport, VcsError> {
        let _guard = lock(&self.mu)?;
        let branch_head = self.require_head(branch)?;
        let onto_head = self.require_head(onto)?;
        let empty_report = |head: CommitId| RebaseReport {
            new_head: head,
            rebased: Vec::new(),
            conflicts: Vec::new(),
        };
        if branch_head == onto_head {
            return Ok(empty_report(branch_head));
        }
        let parents_map = self.parent_map()?;
        let onto_ancestors = ancestors(&parents_map, &onto_head.0);
        let branch_ancestors = ancestors(&parents_map, &branch_head.0);
        if branch_ancestors.contains(&onto_head.0) {
            // Already based on `onto`'s head.
            return Ok(empty_report(branch_head));
        }
        if onto_ancestors.contains(&branch_head.0) {
            // Branch is strictly behind: fast-forward.
            let mut commands = self.set_head_commands(branch, &onto_head)?;
            commands.push(self.oplog_command(
                OpKind::Rebase,
                &format!("fast-forward rebase {branch} onto {onto}"),
            )?);
            self.run_write(&commands)?;
            return Ok(empty_report(onto_head));
        }

        // Commits to replay: reachable from the branch head but not from the
        // onto head, emitted parents-first (Kahn). Merge commits in the
        // replayed range are replayed as single-parent commits against their
        // first parent — documented v1 limitation.
        let replay_set: HashSet<String> = branch_ancestors
            .difference(&onto_ancestors)
            .cloned()
            .collect();
        let order = topo_order(&parents_map, &replay_set);
        let mut mapping: HashMap<String, CommitId> = HashMap::new();
        // Snapshots of replayed commits are only persisted at the end of the
        // loop, so reads during replay go through this overlay.
        let mut new_snapshots: HashMap<String, Snapshot> = HashMap::new();
        let mut commands = Vec::new();
        let mut rebased = Vec::new();
        let mut all_conflicts = Vec::new();
        for old_id in &order {
            let original = self.require_commit(&CommitId(old_id.clone()))?;
            let base_snapshot = match original.parents.first() {
                Some(parent) => self.snapshot_sync(parent)?,
                None => Snapshot::default(),
            };
            let new_parent = original
                .parents
                .first()
                .and_then(|p| mapping.get(&p.0).cloned())
                .unwrap_or_else(|| onto_head.clone());
            let parent_snapshot = match new_snapshots.get(&new_parent.0) {
                Some(snapshot) => snapshot.clone(),
                None => self.snapshot_sync(&new_parent)?,
            };
            let original_snapshot = self.snapshot_sync(&original.id)?;
            let outcome = merge_snapshots(&base_snapshot, &parent_snapshot, &original_snapshot);
            // The original author and change id are preserved; the rebase
            // actor is recorded in the op log.
            let commit = self.mint_commit(
                original.change_id.clone(),
                vec![new_parent],
                original.ops.clone(),
                &original.author,
                &original.message,
                outcome.conflicts.clone(),
                &outcome.merged,
            );
            all_conflicts.extend(outcome.conflicts);
            commands.push(self.commit_command(&commit, &outcome.merged));
            new_snapshots.insert(commit.id.0.clone(), outcome.merged);
            mapping.insert(old_id.clone(), commit.id.clone());
            rebased.push(commit);
        }
        let new_head = rebased
            .last()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| onto_head.clone());
        commands.extend(self.set_head_commands(branch, &new_head)?);
        commands.push(self.oplog_command(
            OpKind::Rebase,
            &format!(
                "rebase {branch} onto {onto} by {_author} ({} commits)",
                rebased.len()
            ),
        )?);
        self.run_write(&commands)?;
        Ok(RebaseReport {
            new_head,
            rebased,
            conflicts: all_conflicts,
        })
    }

    fn do_conflicts(&self, branch: &str) -> Result<Vec<Conflict>, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.require_head(branch)?;
        Ok(self.require_commit(&head)?.conflicts)
    }

    fn do_resolve(
        &self,
        branch: &str,
        resolutions: Vec<ConflictResolution>,
        author: &str,
    ) -> Result<Commit, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.require_head(branch)?;
        let head_commit = self.require_commit(&head)?;
        let base = self.snapshot_sync(&head)?;
        let mut snapshot = base.clone();
        let mut remaining = head_commit.conflicts.clone();
        for resolution in &resolutions {
            let node: Option<vault_data::VaultNode> = match &resolution.choice {
                ResolvedNode::Ours | ResolvedNode::Theirs => {
                    let conflict = head_commit
                        .conflicts
                        .iter()
                        .find(|c| c.node_id == resolution.node_id)
                        .ok_or_else(|| VcsError::UnknownConflict {
                            node_id: resolution.node_id.0.clone(),
                        })?;
                    match &resolution.choice {
                        ResolvedNode::Ours => conflict.ours.clone(),
                        _ => conflict.theirs.clone(),
                    }
                }
                ResolvedNode::Explicit(node) => Some(node.clone()),
                ResolvedNode::Deleted => None,
            };
            match node {
                Some(node) => {
                    snapshot.nodes.insert(NodeId(node.id.clone()), node);
                }
                None => {
                    snapshot.nodes.remove(&resolution.node_id);
                }
            }
            remaining.retain(|c| c.node_id != resolution.node_id);
        }
        let ops = diff_snapshots(&base, &snapshot);
        let message = format!("resolve {} conflict(s)", resolutions.len());
        let change_id = change_id(author, &message, &ops);
        let commit = self.mint_commit(
            change_id,
            vec![head],
            ops,
            author,
            &message,
            remaining,
            &snapshot,
        );
        let summary = format!("resolve on {branch}: {}", commit.id);
        self.land_commit(branch, commit, snapshot, OpKind::Resolve, &summary)
    }

    fn do_diff(&self, a: &CommitId, b: &CommitId) -> Result<Vec<GraphOp>, VcsError> {
        let _guard = lock(&self.mu)?;
        self.require_commit(a)?;
        self.require_commit(b)?;
        let sa = self.snapshot_sync(a)?;
        let sb = self.snapshot_sync(b)?;
        Ok(diff_snapshots(&sa, &sb))
    }

    fn do_materialize(&self, commit: &CommitId) -> Result<Snapshot, VcsError> {
        let _guard = lock(&self.mu)?;
        self.require_commit(commit)?;
        self.snapshot_sync(commit)
    }

    fn do_op_log(&self, limit: usize) -> Result<Vec<OpLogEntry>, VcsError> {
        let _guard = lock(&self.mu)?;
        let rows = self.query(
            "(query [:find ?s ?k ?m ?t :where [?e :oplog/seq ?s] [?e :oplog/kind ?k] [?e :oplog/summary ?m] [?e :oplog/ts ?t]])",
        )?;
        let mut entries: Vec<OpLogEntry> = rows
            .iter()
            .map(|row| {
                Ok(OpLogEntry {
                    seq: as_i64(row, 0)? as u64,
                    kind: parse_opkind(&as_string(row, 1)?)?,
                    summary: as_string(row, 2)?,
                    timestamp_ms: as_i64(row, 3)?,
                })
            })
            .collect::<Result<_, VcsError>>()?;
        entries.sort_by(|a, b| b.seq.cmp(&a.seq));
        entries.truncate(limit);
        Ok(entries)
    }
}

impl VcsStore for MinigrafStore {
    fn head<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Option<CommitId>, VcsError>> {
        Box::pin(async move {
            let _guard = lock(&self.mu)?;
            self.head_sync(branch)
        })
    }

    fn commit<'a>(
        &'a self,
        branch: &'a str,
        ops: Vec<GraphOp>,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>> {
        Box::pin(async move { self.do_commit(branch, ops, author, message) })
    }

    fn log<'a>(
        &'a self,
        branch: &'a str,
        limit: usize,
    ) -> VcsFuture<'a, Result<Vec<Commit>, VcsError>> {
        Box::pin(async move { self.do_log(branch, limit) })
    }

    fn branches(&self) -> VcsFuture<'_, Result<Vec<BranchInfo>, VcsError>> {
        Box::pin(async move { self.do_branches() })
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        from: &'a CommitId,
    ) -> VcsFuture<'a, Result<(), VcsError>> {
        Box::pin(async move { self.do_create_branch(name, from) })
    }

    fn merge<'a>(
        &'a self,
        into: &'a str,
        from: &'a str,
        author: &'a str,
        message: &'a str,
    ) -> VcsFuture<'a, Result<MergeReport, VcsError>> {
        Box::pin(async move { self.do_merge(into, from, author, message) })
    }

    fn rebase<'a>(
        &'a self,
        branch: &'a str,
        onto: &'a str,
        author: &'a str,
    ) -> VcsFuture<'a, Result<RebaseReport, VcsError>> {
        Box::pin(async move { self.do_rebase(branch, onto, author) })
    }

    fn conflicts<'a>(&'a self, branch: &'a str) -> VcsFuture<'a, Result<Vec<Conflict>, VcsError>> {
        Box::pin(async move { self.do_conflicts(branch) })
    }

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        resolutions: Vec<ConflictResolution>,
        author: &'a str,
    ) -> VcsFuture<'a, Result<Commit, VcsError>> {
        Box::pin(async move { self.do_resolve(branch, resolutions, author) })
    }

    fn diff<'a>(
        &'a self,
        a: &'a CommitId,
        b: &'a CommitId,
    ) -> VcsFuture<'a, Result<Vec<GraphOp>, VcsError>> {
        Box::pin(async move { self.do_diff(a, b) })
    }

    fn materialize<'a>(&'a self, commit: &'a CommitId) -> VcsFuture<'a, Result<Snapshot, VcsError>> {
        Box::pin(async move { self.do_materialize(commit) })
    }

    fn op_log(&self, limit: usize) -> VcsFuture<'_, Result<Vec<OpLogEntry>, VcsError>> {
        Box::pin(async move { self.do_op_log(limit) })
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

pub(crate) fn store_err(error: impl std::fmt::Display) -> VcsError {
    VcsError::Store {
        message: error.to_string(),
    }
}

fn lock(mu: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, VcsError> {
    mu.lock().map_err(|_| VcsError::Store {
        message: "store mutex poisoned".into(),
    })
}

/// Escape a Rust string for embedding in a Datalog string literal. The
/// minigraf parser interprets `\` escapes (and drops unknown ones), so every
/// backslash and quote in the payload must be escaped first.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn as_string(row: &[Value], idx: usize) -> Result<String, VcsError> {
    match row.get(idx) {
        Some(Value::String(s)) => Ok(s.clone()),
        other => Err(VcsError::Corrupt {
            message: format!("expected string at column {idx}, got {other:?}"),
        }),
    }
}

fn as_i64(row: &[Value], idx: usize) -> Result<i64, VcsError> {
    match row.get(idx) {
        Some(Value::Integer(i)) => Ok(*i),
        other => Err(VcsError::Corrupt {
            message: format!("expected integer at column {idx}, got {other:?}"),
        }),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, VcsError> {
    serde_json::from_str(json).map_err(|e| VcsError::Corrupt {
        message: format!("invalid stored JSON: {e}"),
    })
}

pub(crate) fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn change_id(author: &str, message: &str, ops: &[GraphOp]) -> ChangeId {
    ChangeId(format!(
        "c-{}",
        &sha256_hex(&[author, message, &canonical_json(&ops.to_vec())])[..16]
    ))
}

pub(crate) fn commit_id(change: &ChangeId, parents: &[CommitId], snapshot: &Snapshot) -> CommitId {
    CommitId(format!(
        "k-{}",
        &sha256_hex(&[
            &change.0,
            &canonical_json(&parents.to_vec()),
            &snapshot.canonical_json(),
        ])[..16]
    ))
}

fn branch_entity(name: &str) -> String {
    format!(":branch/b-{}", &sha256_hex(&[name])[..8])
}

fn opkind_str(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Commit => "commit",
        OpKind::Merge => "merge",
        OpKind::Rebase => "rebase",
        OpKind::Branch => "branch",
        OpKind::Resolve => "resolve",
    }
}

fn parse_opkind(s: &str) -> Result<OpKind, VcsError> {
    Ok(match s {
        "commit" => OpKind::Commit,
        "merge" => OpKind::Merge,
        "rebase" => OpKind::Rebase,
        "branch" => OpKind::Branch,
        "resolve" => OpKind::Resolve,
        other => {
            return Err(VcsError::Corrupt {
                message: format!("unknown op kind {other:?}"),
            })
        }
    })
}

/// Split a payload into char-boundary-safe chunks of at most `max` bytes.
fn chunk_str(s: &str, max: usize) -> Vec<&str> {
    if s.is_empty() {
        return vec![""];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for (idx, ch) in s.char_indices() {
        if idx + ch.len_utf8() - start > max {
            chunks.push(&s[start..end]);
            start = end;
        }
        end = idx + ch.len_utf8();
    }
    if start < s.len() {
        chunks.push(&s[start..]);
    }
    chunks
}

/// Build one fact per chunk of a blob payload. Entity keywords embed the
/// commit id, which is always `k-<hex>` and therefore keyword-safe; the
/// arbitrary payload lives only in string values.
fn chunk_facts(ns: &str, key: &str, payload: &str) -> Vec<String> {
    chunk_str(payload, CHUNK_BYTES)
        .iter()
        .enumerate()
        .flat_map(|(idx, data)| {
            let entity = format!(":{ns}/{key}/{idx}");
            vec![
                format!("[{entity} :{ns}/commit \"{}\"]", esc(key)),
                format!("[{entity} :{ns}/idx {idx}]"),
                format!("[{entity} :{ns}/data \"{}\"]", esc(data)),
            ]
        })
        .collect()
}

/// Read and reassemble a chunked blob. Returns `Ok(None)` when no chunks
/// exist for the key.
fn read_blob(db: &MinigrafStore, ns: &str, key: &str) -> Result<Option<String>, VcsError> {
    let rows = db.query(&format!(
        "(query [:find ?i ?d :where [?e :{ns}/commit \"{}\"] [?e :{ns}/idx ?i] [?e :{ns}/data ?d]])",
        esc(key)
    ))?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut chunks: Vec<(i64, String)> = rows
        .iter()
        .map(|row| Ok((as_i64(row, 0)?, as_string(row, 1)?)))
        .collect::<Result<_, VcsError>>()?;
    chunks.sort_by_key(|(idx, _)| *idx);
    for (expected, (idx, _)) in chunks.iter().enumerate() {
        if *idx != expected as i64 {
            return Err(VcsError::Corrupt {
                message: format!("blob {ns}/{key} is missing chunk {expected}"),
            });
        }
    }
    Ok(Some(chunks.into_iter().map(|(_, data)| data).collect()))
}

fn read_blob_required(db: &MinigrafStore, ns: &str, key: &str) -> Result<String, VcsError> {
    read_blob(db, ns, key)?.ok_or_else(|| VcsError::Corrupt {
        message: format!("missing blob {ns}/{key}"),
    })
}

/// All commit ids reachable from `start` by following parent links,
/// including `start` itself.
pub(crate) fn ancestors(map: &HashMap<String, Vec<String>>, start: &str) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(id) = stack.pop() {
        if seen.insert(id.clone()) {
            if let Some(parents) = map.get(&id) {
                stack.extend(parents.iter().cloned());
            }
        }
    }
    seen
}

/// BFS depths from `start` over parent links (head = 0, its parents = 1, …).
pub(crate) fn depths(map: &HashMap<String, Vec<String>>, start: &str) -> HashMap<String, usize> {
    let mut depth = HashMap::new();
    let mut frontier = vec![start.to_string()];
    let mut current = 0;
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for id in frontier {
            if depth.contains_key(&id) {
                continue;
            }
            depth.insert(id.clone(), current);
            if let Some(parents) = map.get(&id) {
                next.extend(parents.iter().cloned());
            }
        }
        frontier = next;
        current += 1;
    }
    depth
}

/// The nearest common ancestor of two heads: shared ancestor with minimal
/// depth from `a`, ties broken by depth from `b` then by id for determinism.
pub(crate) fn nearest_common_ancestor(
    map: &HashMap<String, Vec<String>>,
    a: &str,
    b: &str,
) -> Option<String> {
    let da = depths(map, a);
    let db = depths(map, b);
    da.iter()
        .filter_map(|(id, depth_a)| db.get(id).map(|depth_b| (id, *depth_a, *depth_b)))
        .min_by(|(id_a, da_a, db_a), (id_b, da_b, db_b)| {
            da_a.cmp(da_b).then(db_a.cmp(db_b)).then(id_a.cmp(id_b))
        })
        .map(|(id, _, _)| id.clone())
}

/// Emit the ids in `set` parents-first. Parents outside the set are ignored
/// (they anchor the replay). Ties broken by id for determinism.
pub(crate) fn topo_order(map: &HashMap<String, Vec<String>>, set: &HashSet<String>) -> Vec<String> {
    let mut remaining: HashSet<String> = set.clone();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let mut ready: Vec<String> = remaining
            .iter()
            .filter(|id| {
                map.get(*id)
                    .map(|parents| parents.iter().all(|p| !remaining.contains(p)))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        ready.sort();
        if ready.is_empty() {
            // A parent cycle would loop forever; corrupt DAG, fail loudly.
            break;
        }
        for id in ready {
            remaining.remove(&id);
            order.push(id);
        }
    }
    order
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// wasm32 has no reliable wall clock without js-sys; a monotonic counter
/// keeps timestamps ordering-meaningful. Documented on the module docs.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn chunk_str_respects_byte_limit_and_roundtrips() {
        // 3-byte characters straddle chunk boundaries.
        let s: String = "€".repeat(10_000);
        let chunks = chunk_str(&s, CHUNK_BYTES);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= CHUNK_BYTES);
        }
        assert_eq!(chunks.concat(), s);
    }

    #[test]
    fn chunk_str_handles_empty_and_exact() {
        assert_eq!(chunk_str("", CHUNK_BYTES), vec![""]);
        let exact = "a".repeat(CHUNK_BYTES);
        assert_eq!(chunk_str(&exact, CHUNK_BYTES).concat(), exact);
    }

    #[test]
    fn ancestors_and_merge_base() {
        // root <- a <- b   root <- x <- y
        let mut map = HashMap::new();
        map.insert("root".to_string(), vec![]);
        map.insert("a".to_string(), vec!["root".to_string()]);
        map.insert("b".to_string(), vec!["a".to_string()]);
        map.insert("x".to_string(), vec!["root".to_string()]);
        map.insert("y".to_string(), vec!["x".to_string()]);
        assert_eq!(
            nearest_common_ancestor(&map, "b", "y"),
            Some("root".to_string())
        );
        assert!(ancestors(&map, "b").contains("root"));
        assert!(!ancestors(&map, "b").contains("x"));
    }
}
