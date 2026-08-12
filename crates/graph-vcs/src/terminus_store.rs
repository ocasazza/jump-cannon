//! [`VcsStore`] implementation on top of a TerminusDB server (native only,
//! feature `terminus`).
//!
//! # Mapping
//!
//! One TerminusDB organization (default `jump_cannon`) holds **one database
//! per world**; the store is constructed per world
//! (`TerminusStore::new(config, world)`). The store keeps the crate's exact
//! jj-style semantics — TerminusDB is the persistence and branching engine,
//! not the merge engine:
//!
//! - Every write goes through the document API (`POST /api/document/...`),
//!   so each landed commit is *also* a native TerminusDB commit carrying the
//!   real author and message. That is the native commit machinery doing its
//!   job: durability, atomicity per write, and an immutable audit trail.
//! - Our branches map 1:1 to native branches (`POST /api/branch/...`), so a
//!   branch's commit documents physically live on its own native branch.
//!   Because our `create_branch` takes an arbitrary *our* [`CommitId`] (not a
//!   native one), the native origin is the from-commit's home branch head;
//!   the native branch may physically contain a few newer commit documents
//!   from that lineage, which are simply never referenced — our DAG governs
//!   reachability. Native point-in-time branching would require stamping the
//!   native `TerminusDB-Data-Version` into every commit document (a second
//!   write per commit) for zero semantic gain.
//! - Merge and rebase do NOT use the native `/api/merge` / `/api/rebase`
//!   endpoints: native merge conflicts are document-level and native rebase
//!   is a squash-apply, while our contract is attribute-level snapshot merges
//!   with recorded [`Conflict`]s and change-id-preserving replay. Both run
//!   [`merge_snapshots`] in Rust over materialized snapshots, exactly like
//!   [`crate::MinigrafStore`], and land as document writes.
//!
//! # Document layout
//!
//! Four document classes (schema installed once at store open):
//!
//! - `VcsCommit/<commit-id>` on the commit's home branch — the fat record:
//!   change id, parents (with each parent's home branch), author, message,
//!   timestamp, and the ops / conflicts / full snapshot as JSON strings. One
//!   document per commit; TerminusDB has no fact-size limit, so no chunking.
//! - `VcsMeta/<commit-id>` on `main` — the global DAG/lookup registry:
//!   home branch + parent ids. Powers `parent_map`, ancestry, and locating
//!   any commit's fat record.
//! - `VcsBranch/<name>` on `main` — the branch head pointer.
//! - `VcsOpLog/<seq>` on `main` — the store-global operation log.
//!
//! # Atomicity
//!
//! A commit lands in two HTTP transactions: (1) the fat `VcsCommit` document
//! on the home branch, (2) the pointer + meta + op-log documents on `main`
//! (one transaction when the branch *is* `main`). If (2) fails the commit
//! document is an unreachable orphan — the branch head never moved, so the
//! operation observably failed and an idempotent retry (same content-addressed
//! id, `overwrite=true`) converges. The reverse order is never used, so a
//! pointer can never reference a missing commit.
//!
//! # Calling convention
//!
//! Like [`crate::MinigrafStore`], all work is synchronous inside the boxed
//! [`VcsFuture`]s (a `reqwest::blocking` client, which owns its runtime on a
//! dedicated thread, so callers need no async reactor — the trivial executor
//! used by the hosts works unchanged). A `Mutex` serializes multi-request
//! sequences. Branch and database names are validated against the
//! TerminusDB path-segment alphabet since they are embedded in URLs.

use crate::merge::{canonical_json, diff_snapshots, merge_snapshots, Snapshot};
use crate::minigraf_store::{
    ancestors, change_id, commit_id, nearest_common_ancestor, now_ms, store_err, topo_order,
};
use crate::model::{
    BranchInfo, BranchName, ChangeId, Commit, CommitId, Conflict, ConflictResolution, GraphOp,
    MergeReport, MergeStatus, NodeId, OpKind, OpLogEntry, RebaseReport, ResolvedNode, VcsError,
};
use crate::store::{VcsFuture, VcsStore};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

/// The branch every database is born with; also the branch holding all
/// global metadata documents (pointers, commit registry, op log).
const MAIN: &str = "main";

/// Connection parameters for a TerminusDB server.
#[derive(Debug, Clone)]
pub struct TerminusConfig {
    /// Server base URL, e.g. `http://terminusdb:6363`.
    pub base_url: String,
    /// Organization holding the per-world databases (default `jump_cannon`).
    pub org: String,
    /// Basic-auth user (default `admin`).
    pub user: String,
    /// Basic-auth password (from a mounted Secret; never hardcoded).
    pub password: String,
}

impl TerminusConfig {
    /// Build from the server environment: `TERMINUSDB_URL` (default
    /// `http://terminusdb:6363`), `TERMINUSDB_ORG` (default `jump_cannon`),
    /// `TERMINUSDB_USER` (default `admin`), `TERMINUSDB_PASSWORD` (required;
    /// mounted from the chart's `terminusdb.adminPasswordSecret`).
    pub fn from_env() -> Result<Self, VcsError> {
        let password = std::env::var("TERMINUSDB_PASSWORD").map_err(|_| VcsError::Store {
            message: "TERMINUSDB_PASSWORD is required for the terminusdb store".into(),
        })?;
        Ok(Self {
            base_url: std::env::var("TERMINUSDB_URL")
                .unwrap_or_else(|_| "http://terminusdb:6363".into()),
            org: std::env::var("TERMINUSDB_ORG").unwrap_or_else(|_| "jump_cannon".into()),
            user: std::env::var("TERMINUSDB_USER").unwrap_or_else(|_| "admin".into()),
            password,
        })
    }
}

/// TerminusDB-backed [`VcsStore`]. See the module docs for the mapping.
pub struct TerminusStore {
    http: reqwest::blocking::Client,
    config: TerminusConfig,
    /// Database name = world slug.
    db: String,
    mu: Mutex<()>,
}

// The store is shared through the object-safe trait, so it must be Send+Sync.
const _: fn() = || {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TerminusStore>();
};

impl TerminusStore {
    /// Connect to the server and open (creating if needed) the database for
    /// `world`, installing the document schema on first use.
    pub fn new(config: TerminusConfig, world: &str) -> Result<Self, VcsError> {
        if !valid_segment(&config.org) || !valid_segment(world) {
            return Err(VcsError::Store {
                message: format!(
                    "invalid TerminusDB organization or world name {:?}/{world:?}: must match [A-Za-z0-9][A-Za-z0-9_-]*",
                    config.org
                ),
            });
        }
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(store_err)?;
        let store = Self {
            http,
            config,
            db: world.to_string(),
            mu: Mutex::new(()),
        };
        store.ensure_database()?;
        store.ensure_schema()?;
        Ok(store)
    }

    /// The world (database) name this store is bound to.
    pub fn world(&self) -> &str {
        &self.db
    }

    // ── HTTP layer ──────────────────────────────────────────────────────────

    fn resource(&self, branch: Option<&str>) -> String {
        match branch {
            None | Some(MAIN) => format!("{}/{}", self.config.org, self.db),
            Some(branch) => format!("{}/{}/local/branch/{branch}", self.config.org, self.db),
        }
    }

    fn document_url(&self, branch: Option<&str>) -> String {
        format!(
            "{}/api/document/{}",
            self.config.base_url.trim_end_matches('/'),
            self.resource(branch)
        )
    }

    /// GET documents, parsed as a JSON list (`as_list=true`).
    fn get_docs(
        &self,
        branch: Option<&str>,
        params: &[(&str, &str)],
    ) -> Result<Vec<Value>, VcsError> {
        let mut query: Vec<(&str, &str)> = vec![("graph_type", "instance"), ("as_list", "true")];
        query.extend_from_slice(params);
        let response = self
            .http
            .get(self.document_url(branch))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .query(&query)
            .send()
            .map_err(store_err)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            // A missing document id 404s; treat it as "no documents".
            return Ok(Vec::new());
        }
        let response = check(response)?;
        let docs: Value = response.json().map_err(store_err)?;
        match docs {
            Value::Array(list) => Ok(list),
            // Defensive: a bare stream-decoded single object.
            doc @ Value::Object(_) => Ok(vec![doc]),
            other => Err(VcsError::Corrupt {
                message: format!("unexpected document list payload: {other:?}"),
            }),
        }
    }

    fn get_doc(&self, branch: Option<&str>, id: &str) -> Result<Option<Value>, VcsError> {
        Ok(self.get_docs(branch, &[("id", id)])?.into_iter().next())
    }

    /// Insert/overwrite documents on a branch in one native commit, carrying
    /// the given author and message.
    fn post_docs(
        &self,
        branch: Option<&str>,
        docs: Vec<Value>,
        author: &str,
        message: &str,
    ) -> Result<(), VcsError> {
        let response = self
            .http
            .post(self.document_url(branch))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .query(&[
                ("graph_type", "instance"),
                ("author", author),
                ("message", message),
                ("overwrite", "true"),
            ])
            .json(&docs)
            .send()
            .map_err(store_err)?;
        check(response)?;
        Ok(())
    }

    /// Create the database when missing. Existence is probed with
    /// `HEAD /api/db/<org>/<db>` (404 = create); a create race or an
    /// already-exists rejection is tolerated.
    fn ensure_database(&self) -> Result<(), VcsError> {
        let base = self.config.base_url.trim_end_matches('/');
        let probe = self
            .http
            .head(format!("{}/api/db/{}/{}", base, self.config.org, self.db))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .send()
            .map_err(store_err)?;
        if probe.status().is_success() {
            return Ok(());
        }
        if probe.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(api_error("probe database", probe));
        }
        let response = self
            .http
            .post(format!("{}/api/db/{}/{}", base, self.config.org, self.db))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .json(&json!({ "label": self.db, "comment": "jump-cannon world graph" }))
            .send()
            .map_err(store_err)?;
        match check(response) {
            Ok(_) => Ok(()),
            // Another store instance created it first.
            Err(e) if already_exists(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Install the four document classes when absent (probed by class name).
    fn ensure_schema(&self) -> Result<(), VcsError> {
        let existing = self
            .http
            .get(format!(
                "{}/api/schema/{}",
                self.config.base_url.trim_end_matches('/'),
                self.resource(None)
            ))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .query(&[("type", "VcsCommit")])
            .send()
            .map_err(store_err)?;
        if existing.status().is_success() {
            let body = existing.text().map_err(store_err)?;
            if body.contains("VcsCommit") {
                return Ok(());
            }
        }
        let response = self
            .http
            .post(self.document_url(None))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .query(&[
                ("graph_type", "schema"),
                ("author", "jump-cannon"),
                ("message", "install graph-vcs schema"),
            ])
            .json(&schema_docs())
            .send()
            .map_err(store_err)?;
        match check(response) {
            Ok(_) => Ok(()),
            // Concurrent installer beat us to it.
            Err(e) if already_exists(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Create native branch `name` from `origin`'s head (the `origin` body
    /// field takes a branch resource path). An already-existing native branch
    /// is adopted (it can only be an orphan of an interrupted earlier
    /// `create_branch`, since our pointer check ran first).
    fn create_native_branch(&self, name: &str, origin: &str) -> Result<(), VcsError> {
        if !valid_segment(name) {
            return Err(VcsError::Store {
                message: format!(
                    "invalid branch name {name:?}: must match [A-Za-z0-9][A-Za-z0-9_-]*"
                ),
            });
        }
        let base = self.config.base_url.trim_end_matches('/');
        let response = self
            .http
            .post(format!(
                "{}/api/branch/{}/{}/local/branch/{name}",
                base, self.config.org, self.db
            ))
            .basic_auth(&self.config.user, Some(&self.config.password))
            .json(&json!({
                "origin": format!("{}/{}/local/branch/{origin}", self.config.org, self.db),
            }))
            .send()
            .map_err(store_err)?;
        match check(response) {
            Ok(_) => Ok(()),
            Err(e) if already_exists(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    // ── Branch pointers (VcsBranch docs on main) ────────────────────────────

    fn branch_rows(&self) -> Result<Vec<(String, String)>, VcsError> {
        self.get_docs(None, &[("type", "VcsBranch")])?
            .iter()
            .map(|doc| parse_branch_doc(doc))
            .collect()
    }

    fn head_sync(&self, branch: &str) -> Result<Option<CommitId>, VcsError> {
        self.get_doc(None, &branch_doc_id(branch))?
            .map(|doc| parse_branch_doc(&doc).map(|(_, head)| CommitId(head)))
            .transpose()
    }

    fn require_head(&self, branch: &str) -> Result<CommitId, VcsError> {
        self.head_sync(branch)?
            .ok_or_else(|| VcsError::UnknownBranch {
                name: branch.to_string(),
            })
    }

    // ── Commit records ──────────────────────────────────────────────────────

    /// The home branch of a commit from its registry entry.
    fn home_of(&self, id: &CommitId) -> Result<Option<String>, VcsError> {
        Ok(self
            .get_doc(None, &meta_doc_id(id))?
            .map(|doc| parse_meta_doc(&doc).map(|(home, _)| home))
            .transpose()?)
    }

    /// Read one commit's fat record (metadata + snapshot) from its home
    /// branch.
    fn commit_sync(&self, id: &CommitId) -> Result<Option<(Commit, Snapshot)>, VcsError> {
        let Some(home) = self.home_of(id)? else {
            return Ok(None);
        };
        let Some(doc) = self.get_doc(Some(&home), &commit_doc_id(id))? else {
            return Err(VcsError::Corrupt {
                message: format!("commit {} registered on {home} but document missing", id.0),
            });
        };
        parse_commit_doc(&doc).map(|(commit, snapshot, _)| Some((commit, snapshot)))
    }

    fn require_commit(&self, id: &CommitId) -> Result<(Commit, Snapshot), VcsError> {
        self.commit_sync(id)?
            .ok_or_else(|| VcsError::UnknownCommit { id: id.0.clone() })
    }

    fn snapshot_sync(&self, id: &CommitId) -> Result<Snapshot, VcsError> {
        Ok(self.require_commit(id)?.1)
    }

    /// Map of every commit id to its parent ids (the global DAG registry).
    fn parent_map(&self) -> Result<HashMap<String, Vec<String>>, VcsError> {
        let mut map = HashMap::new();
        for doc in self.get_docs(None, &[("type", "VcsMeta")])? {
            let (key, parents) = parse_meta_parents(&doc)?;
            map.insert(key, parents);
        }
        Ok(map)
    }

    // ── Op log ──────────────────────────────────────────────────────────────

    fn op_log_entries(&self) -> Result<Vec<OpLogEntry>, VcsError> {
        let mut entries: Vec<OpLogEntry> = self
            .get_docs(None, &[("type", "VcsOpLog")])?
            .iter()
            .map(parse_oplog_doc)
            .collect::<Result<_, _>>()?;
        entries.sort_by(|a, b| b.seq.cmp(&a.seq));
        Ok(entries)
    }

    fn next_oplog_seq(&self) -> Result<u64, VcsError> {
        Ok(self
            .op_log_entries()?
            .iter()
            .map(|entry| entry.seq)
            .max()
            .map_or(0, |max| max + 1))
    }

    // ── Shared write path ───────────────────────────────────────────────────

    /// Record a commit: fat document on the home branch first, then pointer +
    /// registry + op-log on `main` (one transaction when home *is* `main`).
    fn land_commit(
        &self,
        branch: &str,
        commit: Commit,
        snapshot: Snapshot,
        parent_homes: &[(CommitId, String)],
        kind: OpKind,
        summary: &str,
    ) -> Result<Commit, VcsError> {
        let fat = commit_doc(&commit, &snapshot, branch, parent_homes);
        let pointer = branch_doc(branch, &commit.id.0);
        let meta = meta_doc(&commit, branch);
        let seq = self.next_oplog_seq()?;
        let oplog = oplog_doc(seq, kind, summary);
        if branch == MAIN {
            self.post_docs(None, vec![fat, pointer, meta, oplog], &commit.author, &commit.message)?;
        } else {
            self.post_docs(Some(branch), vec![fat], &commit.author, &commit.message)?;
            self.post_docs(None, vec![pointer, meta, oplog], "jump-cannon-vcs", summary)?;
        }
        Ok(commit)
    }

    /// Pointer move + op-log entry without a new commit (fast-forwards).
    fn move_head(&self, branch: &str, head: &CommitId, kind: OpKind, summary: &str) -> Result<(), VcsError> {
        let seq = self.next_oplog_seq()?;
        self.post_docs(
            None,
            vec![branch_doc(branch, &head.0), oplog_doc(seq, kind, summary)],
            "jump-cannon-vcs",
            summary,
        )
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

    // ── Sync implementations (mirror MinigrafStore) ─────────────────────────

    fn do_commit(
        &self,
        branch: &str,
        ops: Vec<GraphOp>,
        author: &str,
        message: &str,
    ) -> Result<Commit, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.head_sync(branch)?;
        if head.is_none() && branch != MAIN {
            // A missing branch is created, rooted at the empty snapshot.
            self.create_native_branch(branch, MAIN)?;
        }
        let (base, conflicts) = match &head {
            Some(id) => {
                let (head_commit, snapshot) = self.require_commit(id)?;
                // jj semantics: unresolved conflicts carry forward.
                (snapshot, head_commit.conflicts)
            }
            None => (Snapshot::default(), Vec::new()),
        };
        let snapshot = base.apply(&ops);
        let change_id = change_id(author, message, &ops);
        let parents: Vec<CommitId> = head.iter().cloned().collect();
        let parent_homes: Vec<(CommitId, String)> = head
            .iter()
            .map(|id| (id.clone(), branch.to_string()))
            .collect();
        let commit = self.mint_commit(change_id, parents, ops, author, message, conflicts, &snapshot);
        let summary = format!("commit {} on {branch}: {message}", commit.id);
        self.land_commit(branch, commit, snapshot, &parent_homes, OpKind::Commit, &summary)
    }

    fn do_log(&self, branch: &str, limit: usize) -> Result<Vec<Commit>, VcsError> {
        let _guard = lock(&self.mu)?;
        let mut out = Vec::new();
        let head = self.require_head(branch)?;
        let mut cursor = (head, branch.to_string());
        while out.len() < limit {
            let doc = self
                .get_doc(Some(&cursor.1), &commit_doc_id(&cursor.0))?
                .ok_or_else(|| VcsError::UnknownCommit { id: cursor.0.0.clone() })?;
            let (commit, _, parents) = parse_commit_doc(&doc)?;
            let Some(parent) = parents.first().cloned() else {
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
        let mut infos: Vec<BranchInfo> = self
            .branch_rows()?
            .into_iter()
            .map(|(name, head)| BranchInfo {
                name: BranchName(name),
                head: CommitId(head),
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(infos)
    }

    fn do_create_branch(&self, name: &str, from: &CommitId) -> Result<(), VcsError> {
        let _guard = lock(&self.mu)?;
        let home = self.home_of(from)?.ok_or_else(|| VcsError::UnknownCommit {
            id: from.0.clone(),
        })?;
        if self.head_sync(name)?.is_some() {
            return Err(VcsError::BranchExists {
                name: name.to_string(),
            });
        }
        // Native branch first: if the pointer write afterwards fails, the
        // orphan native branch is harmless (adopted on retry).
        self.create_native_branch(name, &home)?;
        self.move_head(name, from, OpKind::Branch, &format!("branch {name} at {from}"))
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
            let (commit, _) = self.require_commit(&into_head)?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::UpToDate,
            });
        }
        let parents_map = self.parent_map()?;
        if ancestors(&parents_map, &into_head.0).contains(&from_head.0) {
            let (commit, _) = self.require_commit(&into_head)?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::UpToDate,
            });
        }
        if ancestors(&parents_map, &from_head.0).contains(&into_head.0) {
            let (commit, _) = self.require_commit(&from_head)?;
            self.move_head(
                into,
                &from_head,
                OpKind::Merge,
                &format!("fast-forward {into} to {from_head}"),
            )?;
            return Ok(MergeReport {
                commit,
                conflicts: Vec::new(),
                status: MergeStatus::FastForward,
            });
        }

        // Divergent heads: three-way merge with OUR rules (merge_snapshots),
        // not the native document-level merge. Unrelated roots merge against
        // the empty snapshot.
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
        let parents = vec![into_head.clone(), from_head.clone()];
        let parent_homes = vec![
            (into_head, into.to_string()),
            (from_head, from.to_string()),
        ];
        let commit = self.mint_commit(
            change_id,
            parents,
            ops,
            author,
            message,
            outcome.conflicts.clone(),
            &outcome.merged,
        );
        let summary = format!("merge {from} into {into}: {message}");
        let commit = self.land_commit(
            into,
            commit,
            outcome.merged,
            &parent_homes,
            OpKind::Merge,
            &summary,
        )?;
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
            return Ok(empty_report(branch_head));
        }
        if onto_ancestors.contains(&branch_head.0) {
            // Branch is strictly behind: fast-forward.
            self.move_head(
                branch,
                &onto_head,
                OpKind::Rebase,
                &format!("fast-forward rebase {branch} onto {onto}"),
            )?;
            return Ok(empty_report(onto_head));
        }

        // Commits to replay: reachable from the branch head but not from the
        // onto head, emitted parents-first. Replayed as single-parent commits
        // against their first parent (same documented v1 limitation as
        // MinigrafStore).
        let replay_set: HashSet<String> = branch_ancestors
            .difference(&onto_ancestors)
            .cloned()
            .collect();
        let order = topo_order(&parents_map, &replay_set);
        let mut mapping: HashMap<String, CommitId> = HashMap::new();
        // Snapshots of replayed commits are only persisted at the end of the
        // loop, so reads during replay go through this overlay.
        let mut new_snapshots: HashMap<String, (Snapshot, String)> = HashMap::new();
        let mut fat_docs = Vec::new();
        let mut meta_docs = Vec::new();
        let mut rebased = Vec::new();
        let mut all_conflicts = Vec::new();
        for old_id in &order {
            let (original, original_snapshot) = self.require_commit(&CommitId(old_id.clone()))?;
            let base_snapshot = match original.parents.first() {
                Some(parent) => self.snapshot_sync(parent)?,
                None => Snapshot::default(),
            };
            let (new_parent, new_parent_home) = original
                .parents
                .first()
                .and_then(|p| mapping.get(&p.0).cloned().map(|id| (id, branch.to_string())))
                .unwrap_or_else(|| (onto_head.clone(), onto.to_string()));
            let parent_snapshot = match new_snapshots.get(&new_parent.0) {
                Some((snapshot, _)) => snapshot.clone(),
                None => self.snapshot_sync(&new_parent)?,
            };
            let outcome = merge_snapshots(&base_snapshot, &parent_snapshot, &original_snapshot);
            // The original author and change id are preserved; the rebase
            // actor is recorded in the op log.
            let commit = self.mint_commit(
                original.change_id.clone(),
                vec![new_parent.clone()],
                original.ops.clone(),
                &original.author,
                &original.message,
                outcome.conflicts.clone(),
                &outcome.merged,
            );
            all_conflicts.extend(outcome.conflicts);
            fat_docs.push(commit_doc(
                &commit,
                &outcome.merged,
                branch,
                &[(new_parent, new_parent_home)],
            ));
            meta_docs.push(meta_doc(&commit, branch));
            new_snapshots.insert(commit.id.0.clone(), (outcome.merged, branch.to_string()));
            mapping.insert(old_id.clone(), commit.id.clone());
            rebased.push(commit);
        }
        let new_head = rebased
            .last()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| onto_head.clone());
        let summary = format!(
            "rebase {branch} onto {onto} by {_author} ({} commits)",
            rebased.len()
        );
        let seq = self.next_oplog_seq()?;
        let mut main_docs = vec![branch_doc(branch, &new_head.0), oplog_doc(seq, OpKind::Rebase, &summary)];
        main_docs.extend(meta_docs);
        // Fat documents first (all replayed commits in one native commit),
        // then the pointer + registry + op-log on main.
        self.post_docs(Some(branch), fat_docs, _author, &summary)?;
        self.post_docs(None, main_docs, "jump-cannon-vcs", &summary)?;
        Ok(RebaseReport {
            new_head,
            rebased,
            conflicts: all_conflicts,
        })
    }

    fn do_conflicts(&self, branch: &str) -> Result<Vec<Conflict>, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.require_head(branch)?;
        Ok(self.require_commit(&head)?.0.conflicts)
    }

    fn do_resolve(
        &self,
        branch: &str,
        resolutions: Vec<ConflictResolution>,
        author: &str,
    ) -> Result<Commit, VcsError> {
        let _guard = lock(&self.mu)?;
        let head = self.require_head(branch)?;
        let (head_commit, base) = self.require_commit(&head)?;
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
            vec![head.clone()],
            ops,
            author,
            &message,
            remaining,
            &snapshot,
        );
        let parent_homes = vec![(head, branch.to_string())];
        let summary = format!("resolve on {branch}: {}", commit.id);
        self.land_commit(branch, commit, snapshot, &parent_homes, OpKind::Resolve, &summary)
    }

    fn do_diff(&self, a: &CommitId, b: &CommitId) -> Result<Vec<GraphOp>, VcsError> {
        let _guard = lock(&self.mu)?;
        let (_, sa) = self.require_commit(a)?;
        let (_, sb) = self.require_commit(b)?;
        Ok(diff_snapshots(&sa, &sb))
    }

    fn do_materialize(&self, commit: &CommitId) -> Result<Snapshot, VcsError> {
        let _guard = lock(&self.mu)?;
        Ok(self.require_commit(commit)?.1)
    }

    fn do_op_log(&self, limit: usize) -> Result<Vec<OpLogEntry>, VcsError> {
        let _guard = lock(&self.mu)?;
        let mut entries = self.op_log_entries()?;
        entries.truncate(limit);
        Ok(entries)
    }
}

impl VcsStore for TerminusStore {
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

// ── Document mapping (pure functions; unit-tested without a server) ─────────

/// The schema documents for the four VCS document classes. Keys are Lexical
/// on `key`, so a document's `@id` is exactly `<Class>/<key>`.
fn schema_docs() -> Value {
    let class = |id: &str, fields: &[&str]| {
        let mut doc = json!({
            "@type": "Class",
            "@id": id,
            "@key": { "@type": "Lexical", "@fields": ["key"] },
            "key": "xsd:string",
        });
        for field in fields {
            let ty = if *field == "timestamp_ms" || *field == "seq" {
                "xsd:integer"
            } else {
                "xsd:string"
            };
            doc.as_object_mut().unwrap().insert(field.to_string(), json!(ty));
        }
        doc
    };
    json!([
        class("VcsCommit", &[
            "home", "change_id", "parents_json", "author", "message",
            "timestamp_ms", "ops_json", "conflicts_json", "snapshot_json",
        ]),
        class("VcsMeta", &["home", "parents_json"]),
        class("VcsBranch", &["head"]),
        class("VcsOpLog", &["seq", "kind", "summary", "timestamp_ms"]),
    ])
}

/// TerminusDB embeds organization, database, and branch names in URL paths;
/// restrict them to the conservative segment alphabet.
fn valid_segment(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphanumeric() || (i > 0 && (c == '_' || c == '-')))
}

fn commit_doc_id(id: &CommitId) -> String {
    format!("VcsCommit/{}", id.0)
}

fn meta_doc_id(id: &CommitId) -> String {
    format!("VcsMeta/{}", id.0)
}

fn branch_doc_id(name: &str) -> String {
    format!("VcsBranch/{name}")
}

fn field<'a>(doc: &'a Value, name: &str) -> Result<&'a str, VcsError> {
    doc.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| VcsError::Corrupt {
            message: format!("document {} is missing string field {name}", doc["@id"]),
        })
}

fn field_i64(doc: &Value, name: &str) -> Result<i64, VcsError> {
    doc.get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| VcsError::Corrupt {
            message: format!("document {} is missing integer field {name}", doc["@id"]),
        })
}

fn parse_json_field<T: serde::de::DeserializeOwned>(doc: &Value, name: &str) -> Result<T, VcsError> {
    serde_json::from_str(field(doc, name)?).map_err(|e| VcsError::Corrupt {
        message: format!("document {} has invalid JSON in {name}: {e}", doc["@id"]),
    })
}

/// One parent reference inside a fat commit document: the parent id plus the
/// home branch holding its fat document (so `log` walks without registry
/// round-trips).
#[derive(serde::Serialize, serde::Deserialize)]
struct ParentRef {
    id: String,
    home: String,
}

fn commit_doc(
    commit: &Commit,
    snapshot: &Snapshot,
    home: &str,
    parent_homes: &[(CommitId, String)],
) -> Value {
    let parents: Vec<ParentRef> = parent_homes
        .iter()
        .map(|(id, home)| ParentRef {
            id: id.0.clone(),
            home: home.clone(),
        })
        .collect();
    json!({
        "@type": "VcsCommit",
        "key": commit.id.0,
        "home": home,
        "change_id": commit.change_id.0,
        "parents_json": canonical_json(&parents),
        "author": commit.author,
        "message": commit.message,
        "timestamp_ms": commit.timestamp_ms,
        "ops_json": canonical_json(&commit.ops),
        "conflicts_json": canonical_json(&commit.conflicts),
        "snapshot_json": snapshot.canonical_json(),
    })
}

/// Parse a fat commit document into the commit, its snapshot, and its
/// parents with their home branches.
fn parse_commit_doc(doc: &Value) -> Result<(Commit, Snapshot, Vec<(CommitId, String)>), VcsError> {
    let parents: Vec<ParentRef> = parse_json_field(doc, "parents_json")?;
    let commit = Commit {
        id: CommitId(field(doc, "key")?.to_string()),
        change_id: ChangeId(field(doc, "change_id")?.to_string()),
        parents: parents.iter().map(|p| CommitId(p.id.clone())).collect(),
        ops: parse_json_field(doc, "ops_json")?,
        author: field(doc, "author")?.to_string(),
        message: field(doc, "message")?.to_string(),
        timestamp_ms: field_i64(doc, "timestamp_ms")?,
        conflicts: parse_json_field(doc, "conflicts_json")?,
    };
    let snapshot: Snapshot = parse_json_field(doc, "snapshot_json")?;
    let parents = parents
        .into_iter()
        .map(|p| (CommitId(p.id), p.home))
        .collect();
    Ok((commit, snapshot, parents))
}

/// The global registry entry for one commit (on `main`): home branch and
/// parent ids, for DAG walks and commit lookup.
fn meta_doc(commit: &Commit, home: &str) -> Value {
    json!({
        "@type": "VcsMeta",
        "key": commit.id.0,
        "home": home,
        "parents_json": canonical_json(&commit.parents),
    })
}

/// Parse a registry document into `(home, parents)`.
fn parse_meta_doc(doc: &Value) -> Result<(String, Vec<CommitId>), VcsError> {
    Ok((
        field(doc, "home")?.to_string(),
        parse_json_field(doc, "parents_json")?,
    ))
}

/// Parse a registry document into `(commit id, parent id strings)` for the
/// ancestry map.
fn parse_meta_parents(doc: &Value) -> Result<(String, Vec<String>), VcsError> {
    let parents: Vec<CommitId> = parse_json_field(doc, "parents_json")?;
    Ok((
        field(doc, "key")?.to_string(),
        parents.into_iter().map(|p| p.0).collect(),
    ))
}

fn branch_doc(name: &str, head: &str) -> Value {
    json!({
        "@type": "VcsBranch",
        "key": name,
        "head": head,
    })
}

fn parse_branch_doc(doc: &Value) -> Result<(String, String), VcsError> {
    Ok((
        field(doc, "key")?.to_string(),
        field(doc, "head")?.to_string(),
    ))
}

fn oplog_doc(seq: u64, kind: OpKind, summary: &str) -> Value {
    json!({
        "@type": "VcsOpLog",
        "key": format!("{seq:020}"),
        "seq": seq,
        "kind": opkind_str(kind),
        "summary": summary,
        "timestamp_ms": now_ms(),
    })
}

fn parse_oplog_doc(doc: &Value) -> Result<OpLogEntry, VcsError> {
    Ok(OpLogEntry {
        seq: field_i64(doc, "seq")? as u64,
        kind: parse_opkind(field(doc, "kind")?)?,
        summary: field(doc, "summary")?.to_string(),
        timestamp_ms: field_i64(doc, "timestamp_ms")?,
    })
}

// ── Free helpers ────────────────────────────────────────────────────────────

fn lock(mu: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, VcsError> {
    mu.lock().map_err(|_| VcsError::Store {
        message: "store mutex poisoned".into(),
    })
}

/// Map a non-success HTTP response to a store error, surfacing the server's
/// `api:*` error payload when present.
fn api_error(context: &str, response: reqwest::blocking::Response) -> VcsError {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("api:message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| value.get("api:error").map(|e| e.to_string()))
        })
        .unwrap_or(body);
    VcsError::Store {
        message: format!("terminusdb {context} failed ({status}): {detail}"),
    }
}

/// Whether an API error is an "already exists" rejection
/// (`api:DatabaseAlreadyExists`, `api:BranchExistsError`, schema class
/// redefinition — all carry "exists" in their type or message).
fn already_exists(error: &VcsError) -> bool {
    error.to_string().to_lowercase().contains("exists")
}

fn check(response: reqwest::blocking::Response) -> Result<reqwest::blocking::Response, VcsError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(api_error("request", response))
    }
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

#[cfg(test)]
mod unit_tests {
    use super::*;
    use vault_data::VaultNode;

    fn node(id: &str) -> VaultNode {
        VaultNode {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn sample_commit() -> (Commit, Snapshot) {
        let mut snapshot = Snapshot::default();
        snapshot.nodes.insert(NodeId("n".into()), node("n"));
        let ops = vec![GraphOp::UpsertNode(node("n"))];
        let change = change_id("alice", "initial", &ops);
        let id = commit_id(&change, &[], &snapshot);
        (
            Commit {
                id,
                change_id: change,
                parents: Vec::new(),
                ops,
                author: "alice".into(),
                message: "initial".into(),
                timestamp_ms: 42,
                conflicts: Vec::new(),
            },
            snapshot,
        )
    }

    #[test]
    fn schema_docs_define_four_lexical_keyed_classes() {
        let docs = schema_docs();
        let list = docs.as_array().unwrap();
        assert_eq!(list.len(), 4);
        for doc in list {
            assert_eq!(doc["@key"]["@type"], "Lexical");
            assert_eq!(doc["@key"]["@fields"], json!(["key"]));
        }
        let commit_class = list.iter().find(|d| d["@id"] == "VcsCommit").unwrap();
        assert_eq!(commit_class["timestamp_ms"], "xsd:integer");
        assert_eq!(commit_class["snapshot_json"], "xsd:string");
        assert_eq!(
            list.iter().find(|d| d["@id"] == "VcsOpLog").unwrap()["seq"],
            "xsd:integer"
        );
    }

    #[test]
    fn commit_doc_roundtrip() {
        let (commit, snapshot) = sample_commit();
        let parent_homes = vec![(CommitId("k-parent".into()), "dev".to_string())];
        let mut with_parent = commit.clone();
        with_parent.parents = vec![CommitId("k-parent".into())];
        let doc = commit_doc(&with_parent, &snapshot, "main", &parent_homes);
        assert_eq!(doc["@type"], "VcsCommit");
        assert_eq!(doc["key"], json!(with_parent.id.0));
        let (back, back_snapshot, back_parents) = parse_commit_doc(&doc).unwrap();
        assert_eq!(back.id, with_parent.id);
        assert_eq!(back.change_id, with_parent.change_id);
        assert_eq!(back.author, "alice");
        assert_eq!(back.timestamp_ms, 42);
        assert_eq!(back.ops.len(), 1);
        assert_eq!(back_snapshot, snapshot);
        assert_eq!(
            back_parents,
            vec![(CommitId("k-parent".into()), "dev".to_string())]
        );
    }

    #[test]
    fn commit_doc_roundtrip_with_conflicts() {
        let (mut commit, snapshot) = sample_commit();
        commit.conflicts = vec![Conflict {
            node_id: NodeId("n".into()),
            base: Some(node("n")),
            ours: Some(node("n")),
            theirs: None,
        }];
        let doc = commit_doc(&commit, &snapshot, "main", &[]);
        let (back, _, _) = parse_commit_doc(&doc).unwrap();
        assert_eq!(back.conflicts.len(), 1);
        assert!(back.conflicts[0].theirs.is_none());
    }

    #[test]
    fn meta_branch_oplog_docs_roundtrip() {
        let (commit, _) = sample_commit();
        let meta = meta_doc(&commit, "feature-x");
        let (home, parents) = parse_meta_doc(&meta).unwrap();
        assert_eq!(home, "feature-x");
        assert!(parents.is_empty());
        let (key, parent_ids) = parse_meta_parents(&meta).unwrap();
        assert_eq!(key, commit.id.0);
        assert!(parent_ids.is_empty());

        let branch = branch_doc("dev", "k-abc");
        assert_eq!(
            parse_branch_doc(&branch).unwrap(),
            ("dev".to_string(), "k-abc".to_string())
        );

        let entry = oplog_doc(7, OpKind::Merge, "merge dev into main: m");
        let parsed = parse_oplog_doc(&entry).unwrap();
        assert_eq!(parsed.seq, 7);
        assert_eq!(parsed.kind, OpKind::Merge);
        assert_eq!(parsed.summary, "merge dev into main: m");
    }

    #[test]
    fn segment_validation() {
        assert!(valid_segment("main"));
        assert!(valid_segment("jump_cannon"));
        assert!(valid_segment("feature-x_2"));
        assert!(!valid_segment(""));
        assert!(!valid_segment("has space"));
        assert!(!valid_segment("slash/segment"));
        assert!(!valid_segment("_leading"));
        assert!(!valid_segment("unicode-€"));
    }

    #[test]
    fn resource_paths() {
        let config = TerminusConfig {
            base_url: "http://terminusdb:6363/".into(),
            org: "jump_cannon".into(),
            user: "admin".into(),
            password: "pw".into(),
        };
        // Construction would perform HTTP; build the struct fields directly
        // to test pure path logic.
        let store = TerminusStore {
            http: reqwest::blocking::Client::new(),
            config,
            db: "world1".into(),
            mu: Mutex::new(()),
        };
        assert_eq!(store.resource(None), "jump_cannon/world1");
        assert_eq!(store.resource(Some("main")), "jump_cannon/world1");
        assert_eq!(
            store.resource(Some("dev")),
            "jump_cannon/world1/local/branch/dev"
        );
        assert_eq!(
            store.document_url(Some("dev")),
            "http://terminusdb:6363/api/document/jump_cannon/world1/local/branch/dev"
        );
    }
}
