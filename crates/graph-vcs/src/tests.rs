//! End-to-end tests for [`MinigrafStore`]. The store-level parity scenarios
//! live in [`crate::contract`] (shared with the TerminusDB backend's gated
//! harness in `tests/terminus_contract.rs`); this module adds the
//! minigraf-specific coverage (fact-limit chunking, file-backed reopen, lock
//! handling) and the pure merge/diff unit tests.

use crate::contract;
use crate::merge::{diff_snapshots, merge_snapshots, Snapshot};
use crate::model::{GraphOp, NodeId};
use crate::{MinigrafStore, VcsStore};
use std::future::Future;
use vault_data::{EdgeId, VaultEdge, VaultNode};

/// Minimal executor: the store's futures wrap synchronous work, so a single
/// poll always completes; no async runtime dependency.
fn block_on<F: Future>(future: F) -> F::Output {
    contract::block_on(future)
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

// ── Shared contract scenarios (parity with TerminusStore) ───────────────────

#[test]
fn commit_materialize_roundtrip() {
    contract::commit_materialize_roundtrip(&store());
}

#[test]
fn large_node_roundtrip() {
    contract::large_node_roundtrip(&store());
}

#[test]
fn branch_and_diff() {
    contract::branch_and_diff(&store());
}

#[test]
fn merge_disjoint_nodes_auto_merges() {
    contract::merge_disjoint_nodes_auto_merges(&store());
}

#[test]
fn merge_different_frontmatter_keys_auto_merges() {
    contract::merge_different_frontmatter_keys_auto_merges(&store());
}

#[test]
fn merge_same_attribute_conflicts_then_resolve() {
    contract::merge_same_attribute_conflicts_then_resolve(&store());
}

#[test]
fn merge_delete_vs_edit_conflicts() {
    contract::merge_delete_vs_edit_conflicts(&store());
}

#[test]
fn edge_merge_semantics() {
    contract::edge_merge_semantics(&store());
}

#[test]
fn rebase_replays_commits() {
    contract::rebase_replays_commits(&store());
}

#[test]
fn op_log_records_operations_in_order() {
    contract::op_log_records_operations_in_order(&store());
}

// ── Minigraf-specific coverage ──────────────────────────────────────────────

/// A node whose serialized JSON exceeds the 4080-byte fact limit is
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

/// File-backed store: commit, drop, reopen, and the head/log survive.
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
