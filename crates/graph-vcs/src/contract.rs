//! The store-backend parity contract: the milestone scenarios every
//! [`VcsStore`] implementation must satisfy, written once and run against
//! each backend.
//!
//! Compiled for this crate's own tests (`src/tests.rs` runs it against
//! [`crate::MinigrafStore`]) and for external consumers via the `contract`
//! feature — `tests/terminus_contract.rs` runs the identical functions
//! against [`crate::TerminusStore`] (feature `terminus`) on a reachable
//! server. This mirrors session-manager's `conformance` feature suite.
//!
//! Each function is one scenario and takes an already-open store; backends
//! supply a fresh, empty store per scenario. Backend-specific behavior
//! (minigraf chunking/file locks, TerminusDB server-side persistence) stays
//! in backend-specific tests.

use crate::model::{ConflictResolution, GraphOp, MergeStatus, NodeId, OpKind, ResolvedNode};
use crate::store::VcsStore;
use std::future::Future;
use std::sync::Arc;
use vault_data::{EdgeId, VaultEdge, VaultNode};

/// Minimal executor: store futures wrap synchronous work, so a single poll
/// always completes; no async runtime dependency.
pub fn block_on<F: Future>(future: F) -> F::Output {
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

/// commit → materialize round-trip: nodes, edges, and frontmatter survive.
pub fn commit_materialize_roundtrip<S: VcsStore>(store: &S) {
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

/// A node with a multi-KB frontmatter value round-trips (backends with a
/// per-fact size limit must chunk transparently).
pub fn large_node_roundtrip<S: VcsStore>(store: &S) {
    let big = node_with_fm("big", "blob", &"€".repeat(5000));
    let commit = block_on(store.commit("main", vec![upsert(big)], "alice", "big node")).unwrap();
    let snapshot = block_on(store.materialize(&commit.id)).unwrap();
    let stored = snapshot.nodes.get(&NodeId("big".into())).unwrap();
    assert_eq!(
        stored.meta.frontmatter.get("blob").unwrap().as_str().unwrap(),
        "€".repeat(5000)
    );
}

/// Branching: create_branch from an older commit, BranchExists on a
/// duplicate, head/branches listing, and a diff that reproduces the tip
/// snapshot when applied to the base snapshot.
pub fn branch_and_diff<S: VcsStore>(store: &S) {
    let base = block_on(store.commit("main", vec![upsert(node("a"))], "alice", "base")).unwrap();
    block_on(store.create_branch("dev", &base.id)).unwrap();
    assert!(matches!(
        block_on(store.create_branch("dev", &base.id)),
        Err(crate::model::VcsError::BranchExists { .. })
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
    let base_snapshot = block_on(store.materialize(&base.id)).unwrap();
    let tip_snapshot = block_on(store.materialize(&tip.id)).unwrap();
    assert_eq!(base_snapshot.apply(&ops), tip_snapshot);
}

/// Divergent commits on disjoint nodes merge cleanly.
pub fn merge_disjoint_nodes_auto_merges<S: VcsStore>(store: &S) {
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

/// Same node, different frontmatter keys edited per side → attribute-level
/// auto-merge.
pub fn merge_different_frontmatter_keys_auto_merges<S: VcsStore>(store: &S) {
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

/// Same node, same attribute changed differently → recorded conflict; the
/// merge still lands; `resolve` with an explicit choice clears it.
pub fn merge_same_attribute_conflicts_then_resolve<S: VcsStore>(store: &S) {
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

/// Delete on one side vs edit on the other → conflict; merged snapshot
/// keeps ours; resolving with Theirs restores the edited node.
pub fn merge_delete_vs_edit_conflicts<S: VcsStore>(store: &S) {
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

/// Edge add/add dedupes by identity; delete on one side wins when the edge
/// is unchanged on the other.
pub fn edge_merge_semantics<S: VcsStore>(store: &S) {
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

/// Rebase a two-commit branch onto a moved main: new CommitIds, same
/// ChangeIds, correct final snapshot and log topology; replaying again is a
/// no-op.
pub fn rebase_replays_commits<S: VcsStore>(store: &S) {
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

    // Replaying again is a no-op (already based on main's head).
    let report = block_on(store.rebase("dev", "main", "alice")).unwrap();
    assert!(report.rebased.is_empty());
}

/// The op log records commit / branch / merge / resolve / rebase in order,
/// newest first.
pub fn op_log_records_operations_in_order<S: VcsStore>(store: &S) {
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

/// Every contract scenario, in a fixed order. Used by harnesses that run
/// the whole contract in a single test (e.g. against a container they boot
/// and tear down themselves).
pub fn run_all<S: VcsStore, F: FnMut(&str) -> S>(mut fresh_store: F) {
    let scenarios: &[(&str, fn(&S))] = &[
        ("commit_materialize_roundtrip", commit_materialize_roundtrip),
        ("large_node_roundtrip", large_node_roundtrip),
        ("branch_and_diff", branch_and_diff),
        ("merge_disjoint_nodes_auto_merges", merge_disjoint_nodes_auto_merges),
        (
            "merge_different_frontmatter_keys_auto_merges",
            merge_different_frontmatter_keys_auto_merges,
        ),
        (
            "merge_same_attribute_conflicts_then_resolve",
            merge_same_attribute_conflicts_then_resolve,
        ),
        ("merge_delete_vs_edit_conflicts", merge_delete_vs_edit_conflicts),
        ("edge_merge_semantics", edge_merge_semantics),
        ("rebase_replays_commits", rebase_replays_commits),
        ("op_log_records_operations_in_order", op_log_records_operations_in_order),
    ];
    for (name, scenario) in scenarios {
        eprintln!("contract scenario: {name}");
        let store = fresh_store(name);
        scenario(&store);
    }
}
