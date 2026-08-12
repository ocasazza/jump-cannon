//! Full-contract integration tests for [`TerminusStore`] against a real
//! server. Gated: they need `docker run -p 6363:6363 -e
//! TERMINUSDB_ADMIN_PASS=root terminusdb/terminusdb` running (plus the
//! `terminus` feature). Run with:
//!
//! ```sh
//! cargo test -p graph-vcs --features terminus --test terminus_docker -- --ignored
//! ```
//!
//! `TERMINUSDB_URL` (default `http://127.0.0.1:6363`) and
//! `TERMINUSDB_PASSWORD` (default `root`, the docker image default) select
//! the server. Each test uses its own database (= world), so they are
//! independent and repeatable.

#![cfg(all(feature = "terminus", not(target_arch = "wasm32")))]

use graph_vcs::model::{
    ConflictResolution, GraphOp, MergeStatus, NodeId, OpKind, ResolvedNode,
};
use graph_vcs::{TerminusConfig, TerminusStore, VcsStore};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use vault_data::{EdgeId, VaultEdge, VaultNode};

/// Minimal executor: the store's futures wrap blocking HTTP work, so a
/// single poll always completes; no async runtime dependency.
fn block_on<F: Future>(future: F) -> F::Output {
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

fn node(id: &str) -> VaultNode {
    let mut node = VaultNode {
        id: id.to_string(),
        ..Default::default()
    };
    node.meta.source_id = "test".into();
    node.meta.title = id.into();
    node
}

fn node_with_fm(id: &str, key: &str, value: &str) -> VaultNode {
    let mut node = node(id);
    node.meta
        .frontmatter
        .insert(key.to_string(), serde_json::Value::String(value.to_string()));
    node
}

fn upsert(node: VaultNode) -> GraphOp {
    GraphOp::UpsertNode(node)
}

fn edge(source: &str, target: &str) -> GraphOp {
    GraphOp::UpsertEdge(VaultEdge {
        source: source.into(),
        target: target.into(),
    })
}

/// A store backed by a fresh, uniquely named database (= world).
fn store(test: &str) -> TerminusStore {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let world = format!("t{}-{test}-{seq}", std::process::id());
    let config = TerminusConfig {
        base_url: std::env::var("TERMINUSDB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:6363".into()),
        org: std::env::var("TERMINUSDB_ORG").unwrap_or_else(|_| "jump_cannon".into()),
        user: std::env::var("TERMINUSDB_USER").unwrap_or_else(|_| "admin".into()),
        password: std::env::var("TERMINUSDB_PASSWORD").unwrap_or_else(|_| "root".into()),
    };
    TerminusStore::new(config, &world).expect("connect + create database")
}

/// 1. commit → head/materialize/log round-trip, including a node larger
/// than minigraf's 4KB fact limit (no chunking needed here).
#[test]
#[ignore = "needs docker terminusdb"]
fn commit_materialize_roundtrip() {
    let store = store("roundtrip");
    let mut big = node_with_fm("big", "blob", &"€".repeat(5000));
    big.meta.tags = vec!["x".into(), "y/z".into()];
    let commit = block_on(store.commit(
        "main",
        vec![upsert(big), upsert(node("beta")), edge("big", "beta")],
        "alice",
        "initial",
    ))
    .unwrap();

    assert_eq!(block_on(store.head("main")).unwrap(), Some(commit.id.clone()));
    let snapshot = block_on(store.materialize(&commit.id)).unwrap();
    assert_eq!(snapshot.nodes.len(), 2);
    assert_eq!(
        snapshot.nodes[&NodeId("big".into())]
            .meta
            .frontmatter
            .get("blob")
            .unwrap()
            .as_str()
            .unwrap(),
        "€".repeat(5000)
    );
    assert!(snapshot.edges.contains(&EdgeId {
        source: "big".into(),
        target: "beta".into()
    }));

    let log = block_on(store.log("main", 10)).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].id, commit.id);
    assert_eq!(log[0].author, "alice");
    assert_eq!(log[0].ops.len(), 3);
}

/// 2. Branching: create_branch from an older commit, BranchExists on a
/// duplicate, head/branches listing, and diff between commits.
#[test]
#[ignore = "needs docker terminusdb"]
fn branch_and_diff() {
    let store = store("branch");
    let base = block_on(store.commit("main", vec![upsert(node("a"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    assert!(matches!(
        block_on(store.create_branch("dev", &base.id)),
        Err(graph_vcs::VcsError::BranchExists { .. })
    ));
    let tip = block_on(store.commit("main", vec![upsert(node("b"))], "alice", "tip")).unwrap();
    assert_eq!(block_on(store.head("dev")).unwrap(), Some(base.id.clone()));
    assert_eq!(block_on(store.head("main")).unwrap(), Some(tip.id.clone()));

    let branches = block_on(store.branches()).unwrap();
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0].name.0, "dev");
    assert_eq!(branches[1].name.0, "main");

    let ops = block_on(store.diff(&base.id, &tip.id)).unwrap();
    assert!(ops.iter().any(|op| matches!(op, GraphOp::UpsertNode(n) if n.id == "b")));
    // Applying the diff to the base snapshot reproduces the tip snapshot.
    let base_snapshot = block_on(store.materialize(&base.id)).unwrap();
    let tip_snapshot = block_on(store.materialize(&tip.id)).unwrap();
    assert_eq!(base_snapshot.apply(&ops), tip_snapshot);
}

/// 3. Divergent merge with a same-attribute conflict: recorded, lands with
/// ours, queryable, resolvable; then a fast-forward merge and an up-to-date
/// merge.
#[test]
#[ignore = "needs docker terminusdb"]
fn merge_conflict_resolve_and_fast_forward() {
    let store = store("merge");
    let base = block_on(store.commit(
        "main",
        vec![upsert(node_with_fm("n", "k", "base"))],
        "alice",
        "base",
    ))
    .unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "ours"))], "alice", "ours")).unwrap();
    block_on(store.commit("dev", vec![upsert(node_with_fm("n", "k", "theirs"))], "bob", "theirs")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge dev")).unwrap();
    assert_eq!(report.status, MergeStatus::Merged);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.commit.parents.len(), 2);
    assert_eq!(block_on(store.conflicts("main")).unwrap().len(), 1);
    // Ours is kept in the merged snapshot.
    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    assert_eq!(
        snapshot.nodes[&NodeId("n".into())].meta.frontmatter.get("k").unwrap(),
        "ours"
    );

    let resolution = block_on(store.resolve(
        "main",
        vec![ConflictResolution {
            node_id: NodeId("n".into()),
            choice: ResolvedNode::Theirs,
        }],
        "alice",
    ))
    .unwrap();
    assert!(resolution.conflicts.is_empty());
    assert!(block_on(store.conflicts("main")).unwrap().is_empty());
    let snapshot = block_on(store.materialize(&resolution.id)).unwrap();
    assert_eq!(
        snapshot.nodes[&NodeId("n".into())].meta.frontmatter.get("k").unwrap(),
        "theirs"
    );

    // dev is now strictly behind main: fast-forward.
    let report = block_on(store.merge("dev", "main", "alice", "ff")).unwrap();
    assert_eq!(report.status, MergeStatus::FastForward);
    assert_eq!(block_on(store.head("dev")).unwrap(), Some(report.commit.id.clone()));
    // And again: up to date.
    let report = block_on(store.merge("dev", "main", "alice", "noop")).unwrap();
    assert_eq!(report.status, MergeStatus::UpToDate);
}

/// 4. Rebase a two-commit branch onto a moved main: fresh CommitIds, same
/// ChangeIds, correct snapshot and first-parent log topology.
#[test]
#[ignore = "needs docker terminusdb"]
fn rebase_replays_commits() {
    let store = store("rebase");
    let base = block_on(store.commit("main", vec![upsert(node("root"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    let d1 = block_on(store.commit("dev", vec![upsert(node("d1"))], "bob", "d1")).unwrap();
    let d2 = block_on(store.commit("dev", vec![upsert(node("d2"))], "bob", "d2")).unwrap();
    block_on(store.commit("main", vec![upsert(node("main-work"))], "alice", "main work")).unwrap();

    let report = block_on(store.rebase("dev", "main", "alice")).unwrap();
    assert_eq!(report.rebased.len(), 2);
    assert_eq!(report.rebased[0].change_id, d1.change_id);
    assert_eq!(report.rebased[1].change_id, d2.change_id);
    assert_ne!(report.rebased[0].id, d1.id);
    assert_ne!(report.rebased[1].id, d2.id);
    assert_eq!(report.rebased[1].parents, vec![report.rebased[0].id.clone()]);
    assert_eq!(report.new_head, report.rebased[1].id);

    let snapshot = block_on(store.materialize(&report.new_head)).unwrap();
    for id in ["root", "main-work", "d1", "d2"] {
        assert!(snapshot.nodes.contains_key(&NodeId(id.into())), "missing {id}");
    }

    let log = block_on(store.log("dev", 10)).unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log[0].id, report.rebased[1].id);
    assert_eq!(log[1].id, report.rebased[0].id);
    assert_eq!(log[3].id, base.id);
    assert_eq!(block_on(store.head("dev")).unwrap(), Some(report.new_head));

    // Replaying again is a no-op (already based on main's head).
    let report = block_on(store.rebase("dev", "main", "alice")).unwrap();
    assert!(report.rebased.is_empty());
}

/// 5. The op log records commit / branch / merge / resolve / rebase in
/// order, newest first, and survives a store reopen (server-side
/// persistence).
#[test]
#[ignore = "needs docker terminusdb"]
fn op_log_and_reopen() {
    let world_store = store("oplog");
    let base = block_on(world_store.commit(
        "main",
        vec![upsert(node_with_fm("n", "k", "v"))],
        "alice",
        "base",
    ))
    .unwrap();
    block_on(world_store.create_branch("dev", &base.id)).unwrap();
    block_on(world_store.commit("main", vec![upsert(node_with_fm("n", "k", "ours"))], "alice", "ours")).unwrap();
    block_on(world_store.commit("dev", vec![upsert(node_with_fm("n", "k", "theirs"))], "bob", "theirs")).unwrap();
    block_on(world_store.merge("main", "dev", "alice", "merge")).unwrap();
    block_on(world_store.resolve(
        "main",
        vec![ConflictResolution { node_id: NodeId("n".into()), choice: ResolvedNode::Ours }],
        "alice",
    ))
    .unwrap();
    block_on(world_store.rebase("dev", "main", "alice")).unwrap();

    let log = block_on(world_store.op_log(100)).unwrap();
    let kinds: Vec<OpKind> = log.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            OpKind::Rebase,
            OpKind::Resolve,
            OpKind::Merge,
            OpKind::Commit,
            OpKind::Commit,
            OpKind::Branch,
            OpKind::Commit,
        ]
    );
    let seqs: Vec<u64> = log.iter().map(|e| e.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(seqs, sorted, "newest first");

    // A second store instance over the same database sees the same state.
    let reopened = {
        let config = TerminusConfig {
            base_url: std::env::var("TERMINUSDB_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6363".into()),
            org: std::env::var("TERMINUSDB_ORG").unwrap_or_else(|_| "jump_cannon".into()),
            user: std::env::var("TERMINUSDB_USER").unwrap_or_else(|_| "admin".into()),
            password: std::env::var("TERMINUSDB_PASSWORD").unwrap_or_else(|_| "root".into()),
        };
        // Recover the world name by re-deriving what `store("oplog")` used:
        // it is the only database with this test's marker, so just reuse the
        // same construction path via a second handle on the same world.
        TerminusStore::new(config, &world_store.world().to_string()).unwrap()
    };
    assert_eq!(block_on(reopened.op_log(100)).unwrap().len(), log.len());
    assert_eq!(block_on(reopened.branches()).unwrap().len(), 2);
}
