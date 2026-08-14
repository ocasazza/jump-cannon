//! End-to-end tests for [`MinigrafStore`] against the ten milestone scenarios.
//! Follows the vault-data convention of a `src/tests.rs` module.

use crate::merge::{diff_snapshots, merge_snapshots, Snapshot};
use crate::model::{ConflictResolution, GraphOp, MergeStatus, NodeId, OpKind, ResolvedNode};
use crate::{MinigrafStore, VcsStore};
use std::future::Future;
use std::sync::Arc;
use vault_data::{EdgeId, VaultEdge, VaultNode};

/// Minimal executor: the store's futures wrap synchronous work, so a single
/// poll always completes; no async runtime dependency.
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

fn store() -> MinigrafStore {
    MinigrafStore::in_memory().unwrap()
}

/// 1. commit → materialize round-trip: nodes, edges, and frontmatter survive.
#[test]
fn commit_materialize_roundtrip() {
    let store = store();
    let mut a = node_with_fm("alpha", "status", "draft");
    a.meta.tags = vec!["x".into(), "y/z".into()];
    a.x = 1.5;
    a.y = -2.25;
    let b = node("beta");
    let commit = block_on(store.commit(
        "main",
        vec![upsert(a.clone()), upsert(b.clone()), edge("alpha", "beta")],
        "alice",
        "initial",
    ))
    .unwrap();

    let snapshot = block_on(store.materialize(&commit.id)).unwrap();
    assert_eq!(snapshot.nodes.len(), 2);
    assert_eq!(
        serde_json::to_value(snapshot.nodes.get(&NodeId("alpha".into())).unwrap()).unwrap(),
        serde_json::to_value(&a).unwrap()
    );
    assert_eq!(
        serde_json::to_value(snapshot.nodes.get(&NodeId("beta".into())).unwrap()).unwrap(),
        serde_json::to_value(&b).unwrap()
    );
    assert!(snapshot.edges.contains(&EdgeId {
        source: "alpha".into(),
        target: "beta".into()
    }));
    assert_eq!(block_on(store.head("main")).unwrap(), Some(commit.id));
}

/// 2. A node whose serialized JSON exceeds the 4080-byte fact limit is
/// chunked and round-trips.
#[test]
fn chunked_large_node_roundtrip() {
    let store = store();
    let mut big = node("big");
    big.meta
        .frontmatter
        .insert("blob".into(), serde_json::Value::String("€".repeat(5000)));
    let json = crate::merge::canonical_json(&big);
    assert!(json.len() > 4080, "test requires an over-limit node");

    let commit = block_on(store.commit("main", vec![upsert(big)], "alice", "big node")).unwrap();
    let snapshot = block_on(store.materialize(&commit.id)).unwrap();
    let stored = snapshot.nodes.get(&NodeId("big".into())).unwrap();
    assert_eq!(
        stored.meta.frontmatter.get("blob").unwrap().as_str().unwrap(),
        "€".repeat(5000)
    );
}

/// 3. Divergent commits on disjoint nodes merge cleanly.
#[test]
fn merge_disjoint_nodes_auto_merges() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node("shared"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    block_on(store.commit("main", vec![upsert(node("from-main"))], "alice", "main work")).unwrap();
    block_on(store.commit("dev", vec![upsert(node("from-dev"))], "bob", "dev work")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge dev")).unwrap();
    assert_eq!(report.status, MergeStatus::Merged);
    assert!(report.conflicts.is_empty());
    assert_eq!(report.commit.parents.len(), 2);

    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    for id in ["shared", "from-main", "from-dev"] {
        assert!(snapshot.nodes.contains_key(&NodeId(id.into())), "missing {id}");
    }
}

/// 4. Same node, different frontmatter keys edited per side → attribute-level
/// auto-merge.
#[test]
fn merge_different_frontmatter_keys_auto_merges() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node("n"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    block_on(store.commit("main", vec![upsert(node_with_fm("n", "a", "1"))], "alice", "add a")).unwrap();
    block_on(store.commit("dev", vec![upsert(node_with_fm("n", "b", "2"))], "bob", "add b")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge")).unwrap();
    assert_eq!(report.status, MergeStatus::Merged);
    assert!(report.conflicts.is_empty());
    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    let fm = &snapshot.nodes.get(&NodeId("n".into())).unwrap().meta.frontmatter;
    assert_eq!(fm.get("a").unwrap(), "1");
    assert_eq!(fm.get("b").unwrap(), "2");
}

/// 5. Same node, same attribute changed differently → recorded conflict; the
/// merge still lands; `resolve` with an explicit choice clears it.
#[test]
fn merge_same_attribute_conflicts_then_resolve() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "base"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "ours"))], "alice", "ours")).unwrap();
    block_on(store.commit("dev", vec![upsert(node_with_fm("n", "k", "theirs"))], "bob", "theirs")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge")).unwrap();
    assert_eq!(report.status, MergeStatus::Merged);
    assert_eq!(report.conflicts.len(), 1);
    let conflict = &report.conflicts[0];
    assert_eq!(conflict.node_id, NodeId("n".into()));
    assert!(conflict.base.is_some() && conflict.ours.is_some() && conflict.theirs.is_some());

    // The merge lands with `ours` kept, and the conflict is queryable.
    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    let fm = &snapshot.nodes.get(&NodeId("n".into())).unwrap().meta.frontmatter;
    assert_eq!(fm.get("k").unwrap(), "ours");
    assert_eq!(block_on(store.conflicts("main")).unwrap().len(), 1);

    // Resolve with an explicit value: conflict clears, new commit lands.
    let resolved = node_with_fm("n", "k", "agreed");
    let resolution = block_on(store.resolve(
        "main",
        vec![ConflictResolution {
            node_id: NodeId("n".into()),
            choice: ResolvedNode::Explicit(resolved),
        }],
        "alice",
    ))
    .unwrap();
    assert!(resolution.conflicts.is_empty());
    assert!(block_on(store.conflicts("main")).unwrap().is_empty());
    let snapshot = block_on(store.materialize(&resolution.id)).unwrap();
    let fm = &snapshot.nodes.get(&NodeId("n".into())).unwrap().meta.frontmatter;
    assert_eq!(fm.get("k").unwrap(), "agreed");
}

/// 6. Delete on one side vs edit on the other → conflict; merged snapshot
/// keeps ours.
#[test]
fn merge_delete_vs_edit_conflicts() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "v"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    // main (ours) deletes; dev (theirs) edits.
    block_on(store.commit("main", vec![GraphOp::DeleteNode(NodeId("n".into()))], "alice", "delete")).unwrap();
    block_on(store.commit("dev", vec![upsert(node_with_fm("n", "k", "edited"))], "bob", "edit")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge")).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    let conflict = &report.conflicts[0];
    assert!(conflict.base.is_some());
    assert!(conflict.ours.is_none(), "ours deleted the node");
    assert!(conflict.theirs.is_some(), "theirs edited the node");
    // Ours (delete) is kept in the merged snapshot.
    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    assert!(!snapshot.nodes.contains_key(&NodeId("n".into())));

    // Resolving with Theirs restores the edited node.
    let resolution = block_on(store.resolve(
        "main",
        vec![ConflictResolution {
            node_id: NodeId("n".into()),
            choice: ResolvedNode::Theirs,
        }],
        "alice",
    ))
    .unwrap();
    let snapshot = block_on(store.materialize(&resolution.id)).unwrap();
    assert_eq!(
        snapshot
            .nodes
            .get(&NodeId("n".into()))
            .unwrap()
            .meta
            .frontmatter
            .get("k")
            .unwrap(),
        "edited"
    );
}

/// 7. Edge add/add dedupes by identity; delete on one side wins when the edge
/// is unchanged on the other.
#[test]
fn edge_merge_semantics() {
    let store = store();
    let base = block_on(store.commit(
        "main",
        vec![
            upsert(node("a")),
            upsert(node("b")),
            upsert(node("c")),
            edge("a", "b"),
            edge("b", "c"),
        ],
        "alice",
        "base",
    ))
    .unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    // main adds a->c (also added by dev) and deletes b->c.
    block_on(store.commit(
        "main",
        vec![edge("a", "c"), GraphOp::DeleteEdge(EdgeId { source: "b".into(), target: "c".into() })],
        "alice",
        "main edges",
    ))
    .unwrap();
    // dev adds the same a->c edge, leaves b->c untouched.
    block_on(store.commit("dev", vec![edge("a", "c")], "bob", "dev edges")).unwrap();

    let report = block_on(store.merge("main", "dev", "alice", "merge")).unwrap();
    assert!(report.conflicts.is_empty());
    let snapshot = block_on(store.materialize(&report.commit.id)).unwrap();
    assert!(snapshot.edges.contains(&EdgeId { source: "a".into(), target: "b".into() }));
    assert!(snapshot.edges.contains(&EdgeId { source: "a".into(), target: "c".into() }));
    assert!(
        !snapshot.edges.contains(&EdgeId { source: "b".into(), target: "c".into() }),
        "deleted on main, unchanged on dev → absent"
    );
    assert_eq!(snapshot.edges.len(), 2);
}

/// 8. Rebase a two-commit branch onto a moved main: new CommitIds, same
/// ChangeIds, correct final snapshot and log topology.
#[test]
fn rebase_replays_commits() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node("root"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    let d1 = block_on(store.commit("dev", vec![upsert(node("d1"))], "bob", "d1")).unwrap();
    let d2 = block_on(store.commit("dev", vec![upsert(node_with_fm("d2", "k", "v"))], "bob", "d2")).unwrap();
    // main moves ahead.
    block_on(store.commit("main", vec![upsert(node("main-work"))], "alice", "main work")).unwrap();

    let report = block_on(store.rebase("dev", "main", "alice")).unwrap();
    assert_eq!(report.rebased.len(), 2);
    assert_eq!(report.rebased[0].change_id, d1.change_id);
    assert_eq!(report.rebased[1].change_id, d2.change_id);
    assert_ne!(report.rebased[0].id, d1.id);
    assert_ne!(report.rebased[1].id, d2.id);
    // Rebased chain: d2' -> d1' -> main head.
    assert_eq!(report.rebased[1].parents, vec![report.rebased[0].id.clone()]);
    assert_eq!(report.new_head, report.rebased[1].id);

    let snapshot = block_on(store.materialize(&report.new_head)).unwrap();
    for id in ["root", "main-work", "d1", "d2"] {
        assert!(snapshot.nodes.contains_key(&NodeId(id.into())), "missing {id}");
    }

    // Log walks first parents: d2', d1', main-work commit, base.
    let log = block_on(store.log("dev", 10)).unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log[0].id, report.rebased[1].id);
    assert_eq!(log[1].id, report.rebased[0].id);
    assert_eq!(log[3].id, base.id);
    assert_eq!(block_on(store.head("dev")).unwrap(), Some(report.new_head));
}

/// 9. The op log records commit / branch / merge / resolve / rebase in order.
#[test]
fn op_log_records_operations_in_order() {
    let store = store();
    let base = block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "v"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    block_on(store.commit("main", vec![upsert(node_with_fm("n", "k", "ours"))], "alice", "ours")).unwrap();
    block_on(store.commit("dev", vec![upsert(node_with_fm("n", "k", "theirs"))], "bob", "theirs")).unwrap();
    block_on(store.merge("main", "dev", "alice", "merge")).unwrap();
    block_on(store.resolve(
        "main",
        vec![ConflictResolution { node_id: NodeId("n".into()), choice: ResolvedNode::Ours }],
        "alice",
    ))
    .unwrap();
    block_on(store.rebase("dev", "main", "alice")).unwrap();

    let log = block_on(store.op_log(100)).unwrap();
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
}

/// 10. File-backed store: commit, drop, reopen, and the head/log survive.
#[test]
fn file_backed_reopen_preserves_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("world.graph");
    let (head, change) = {
        let store = MinigrafStore::open(&path).unwrap();
        let commit = block_on(store.commit(
            "main",
            vec![upsert(node_with_fm("persistent", "k", "v")), edge("persistent", "persistent")],
            "alice",
            "durable",
        ))
        .unwrap();
        (commit.id, commit.change_id)
    };
    let store = MinigrafStore::open(&path).unwrap();
    assert_eq!(block_on(store.head("main")).unwrap(), Some(head.clone()));
    let log = block_on(store.log("main", 10)).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].id, head);
    assert_eq!(log[0].change_id, change);
    assert_eq!(log[0].message, "durable");
    let snapshot = block_on(store.materialize(&head)).unwrap();
    assert!(snapshot.nodes.contains_key(&NodeId("persistent".into())));
    assert_eq!(snapshot.edges.len(), 1);
    // The op log persists too.
    assert_eq!(block_on(store.op_log(10)).unwrap().len(), 1);
}

/// A leftover lock file whose holder PID aliases ours (every container's
/// main process is PID 1 in its own PID namespace, so a dead pod's lock
/// reads as "this process") must be reclaimed once no live handle exists.
#[test]
fn stale_container_lock_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("world.graph");
    {
        let store = MinigrafStore::open(&path).unwrap();
        block_on(store.commit("main", vec![upsert(node("kept"))], "alice", "seed")).unwrap();
        // drop: the clean FileLock drop removes the lock file.
    }
    // Simulate the dead pod's leftover: a lock file bearing OUR pid.
    std::fs::write(
        path.with_extension("graph.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let store = MinigrafStore::open(&path)
        .expect("stale same-PID lock with no live handle must be reclaimed");
    assert!(block_on(store.head("main")).unwrap().is_some());
}

/// The registry must not weaken minigraf's real protection: a second open
/// while we hold a live handle on the same file still fails.
#[test]
fn live_same_process_double_open_still_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("world.graph");
    let _store = MinigrafStore::open(&path).unwrap();
    let err = match MinigrafStore::open(&path) {
        Ok(_) => panic!("double open of a live handle must fail"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("already open"), "unexpected error: {err}");
}

// ── Pure merge/diff unit coverage ───────────────────────────────────────────

#[test]
fn diff_snapshots_reports_all_op_kinds() {
    let mut a = Snapshot::default();
    a.nodes.insert(NodeId("keep".into()), node("keep"));
    a.nodes.insert(NodeId("gone".into()), node("gone"));
    a.edges.insert(EdgeId { source: "keep".into(), target: "gone".into() });

    let mut b = Snapshot::default();
    b.nodes.insert(NodeId("keep".into()), node_with_fm("keep", "k", "changed"));
    b.nodes.insert(NodeId("new".into()), node("new"));
    b.edges.insert(EdgeId { source: "keep".into(), target: "new".into() });

    let ops = diff_snapshots(&a, &b);
    assert!(ops.iter().any(|op| matches!(op, GraphOp::UpsertNode(n) if n.id == "keep")));
    assert!(ops.iter().any(|op| matches!(op, GraphOp::UpsertNode(n) if n.id == "new")));
    assert!(ops.iter().any(|op| matches!(op, GraphOp::DeleteNode(id) if *id == NodeId("gone".into()))));
    assert!(ops.iter().any(|op| matches!(op, GraphOp::UpsertEdge(e) if e.target == "new")));
    assert!(ops.iter().any(|op| matches!(op, GraphOp::DeleteEdge(id) if id.target == "gone")));
    // Applying the diff to `a` reproduces `b`.
    assert_eq!(a.apply(&ops), b);
}

#[test]
fn merge_add_add_identical_node_is_clean() {
    let base = Snapshot::default();
    let mut ours = Snapshot::default();
    let mut theirs = Snapshot::default();
    ours.nodes.insert(NodeId("n".into()), node("n"));
    theirs.nodes.insert(NodeId("n".into()), node("n"));
    let outcome = merge_snapshots(&base, &ours, &theirs);
    assert!(outcome.conflicts.is_empty());
    assert_eq!(outcome.merged.nodes.len(), 1);
}
