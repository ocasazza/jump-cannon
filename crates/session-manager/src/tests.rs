//! Tests: run the conformance suite against the embedded host (in-memory
//! and, natively, file-backed including the reopen persistence case), plus
//! slug unit tests. Follows the vault-data convention of a `src/tests.rs`
//! module.

use crate::conformance::check_worldhost;
use crate::embedded::block_on;
use crate::types::{SessionError, WorldId, WorldSpec};
use crate::{EmbeddedSessionManager, WorldHost};
use std::sync::Arc;

#[test]
fn conformance_in_memory() {
    let host = EmbeddedSessionManager::in_memory().unwrap();
    block_on(check_worldhost(Arc::new(host)));
}

/// File-backed: the suite closes the world; a fresh manager over the same
/// root must still list it and be able to re-open it (close ≠ delete).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn conformance_file_backed_with_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("worlds");
    let world_id = WorldId::from_name("Conformance World").unwrap();
    {
        let host = EmbeddedSessionManager::open(&root).unwrap();
        block_on(check_worldhost(Arc::new(host)));
    }
    let reopened = EmbeddedSessionManager::open(&root).unwrap();
    let worlds = block_on(reopened.worlds()).unwrap();
    let info = worlds
        .iter()
        .find(|w| w.id == world_id)
        .expect("closed world must persist across managers");
    assert_eq!(info.name, "Conformance World");
    assert_eq!(
        info.description.as_deref(),
        Some("created by the conformance suite")
    );
    assert_eq!(info.branches, 1);
    // Re-opening a persisted-but-closed world attaches a fresh handle.
    let handle = block_on(reopened.open_world(WorldSpec {
        name: "Conformance World".to_string(),
        description: None,
    }))
    .unwrap();
    assert_eq!(handle.id, world_id);
    let vcs = block_on(reopened.vcs(&handle.id)).unwrap();
    let snapshot = {
        let head = block_on(vcs.head("main")).unwrap().unwrap();
        block_on(vcs.materialize(&head)).unwrap()
    };
    assert!(
        snapshot
            .nodes
            .contains_key(&graph_vcs::NodeId("alpha".to_string())),
        "committed content must survive a manager restart"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn stray_world_files_are_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("worlds");
    std::fs::create_dir_all(&root).unwrap();
    // A world file with no manifest entry (e.g. manifest lost) is adopted.
    graph_vcs::MinigrafStore::open(root.join("orphan.graph")).unwrap();
    let host = EmbeddedSessionManager::open(&root).unwrap();
    let worlds = block_on(host.worlds()).unwrap();
    assert!(worlds.iter().any(|w| w.id.0 == "orphan"));
}

#[test]
fn unknown_world_errors() {
    let host = EmbeddedSessionManager::in_memory().unwrap();
    let id = WorldId("nope".to_string());
    match block_on(host.vcs(&id)) {
        Err(SessionError::WorldNotFound { .. }) => {}
        Err(e) => panic!("expected WorldNotFound, got {e:?}"),
        Ok(_) => panic!("expected WorldNotFound, got Ok"),
    }
    match block_on(host.close_world(&id)) {
        Err(SessionError::WorldNotFound { .. }) => {}
        other => panic!("expected WorldNotFound, got {other:?}"),
    }
    match block_on(host.open_world(WorldSpec {
        name: "!!!".to_string(),
        description: None,
    })) {
        Err(SessionError::InvalidName { .. }) => {}
        other => panic!("expected InvalidName, got {other:?}"),
    }
}

#[test]
fn world_id_slug_rules() {
    assert_eq!(WorldId::from_name("My World!").unwrap().0, "my-world");
    assert_eq!(WorldId::from_name("dots_and.dashes--ok").unwrap().0, "dots_and.dashes--ok");
    assert_eq!(WorldId::from_name("  spaced  out  ").unwrap().0, "spaced-out");
    assert!(WorldId::from_name("!!!").is_err());
    assert!(WorldId::parse("").is_err());
    assert!(WorldId::parse("not a slug").is_err());
    assert!(WorldId::parse("fine.slug_1-2").is_ok());
}

// ── World export / import ───────────────────────────────────────────────────

fn test_node(id: &str, title: &str) -> graph_vcs::VaultNode {
    let mut node = graph_vcs::VaultNode::default();
    node.id = id.to_string();
    node.meta.title = title.to_string();
    node.meta.tags = vec!["t1".to_string(), "t2".to_string()];
    node
}

/// Commits carry no `PartialEq`; compare through the sorted-key JSON value,
/// the same canonicalization the store uses for content addressing.
fn canonical(value: &impl serde::Serialize) -> String {
    serde_json::to_string(&serde_json::to_value(value).unwrap()).unwrap()
}

/// Build a world with history: two commits on main, a feature branch with
/// its own commit, and a merge back into main. Returns (manager, world id).
fn build_history_world() -> (EmbeddedSessionManager, WorldId) {
    use graph_vcs::{GraphOp, VaultEdge};
    let host = EmbeddedSessionManager::in_memory().unwrap();
    let handle = block_on(host.open_world(WorldSpec {
        name: "history".to_string(),
        description: Some("with branches and a merge".to_string()),
    }))
    .unwrap();
    let vcs = block_on(host.vcs(&handle.id)).unwrap();
    block_on(vcs.commit(
        "main",
        vec![
            GraphOp::UpsertNode(test_node("a", "Alpha")),
            GraphOp::UpsertNode(test_node("b", "Beta")),
            GraphOp::UpsertEdge(VaultEdge {
                source: "a".to_string(),
                target: "b".to_string(),
            }),
        ],
        "local",
        "add a and b",
    ))
    .unwrap();
    let head = block_on(vcs.head("main")).unwrap().unwrap();
    block_on(vcs.create_branch("feature", &head)).unwrap();
    block_on(vcs.commit(
        "feature",
        vec![GraphOp::UpsertNode(test_node("c", "Gamma"))],
        "local",
        "add c on feature",
    ))
    .unwrap();
    block_on(vcs.merge("main", "feature", "local", "merge feature")).unwrap();
    (host, handle.id)
}

#[test]
fn export_import_round_trip() {
    let (host, id) = build_history_world();
    let export = host.export_world(&id).unwrap();
    // The wire shape round-trips through JSON (the browser download/upload
    // and the localStorage persistence both go through serde_json).
    let json = serde_json::to_string_pretty(&export).unwrap();
    let parsed: crate::WorldExport = serde_json::from_str(&json).unwrap();

    let fresh = EmbeddedSessionManager::in_memory().unwrap();
    let handle = fresh.import_world("history", parsed).unwrap();
    assert_eq!(handle.id, id);

    let original = block_on(host.vcs(&id)).unwrap();
    let imported = block_on(fresh.vcs(&handle.id)).unwrap();
    for branch in ["main", "feature"] {
        let log_a = block_on(original.log(branch, 100)).unwrap();
        let log_b = block_on(imported.log(branch, 100)).unwrap();
        assert_eq!(
            canonical(&log_a),
            canonical(&log_b),
            "commit log on {branch} must survive export/import"
        );
        let head_a = block_on(original.head(branch)).unwrap().unwrap();
        let head_b = block_on(imported.head(branch)).unwrap().unwrap();
        assert_eq!(head_a, head_b);
        let snap_a = block_on(original.materialize(&head_a)).unwrap();
        let snap_b = block_on(imported.materialize(&head_b)).unwrap();
        assert_eq!(
            snap_a.canonical_json(),
            snap_b.canonical_json(),
            "materialized {branch} head must survive export/import"
        );
    }
    // Branch listing is identical too.
    assert_eq!(
        canonical(&block_on(original.branches()).unwrap()),
        canonical(&block_on(imported.branches()).unwrap())
    );
}

#[test]
fn export_unknown_world_fails() {
    let host = EmbeddedSessionManager::in_memory().unwrap();
    match host.export_world(&WorldId("nope".to_string())) {
        Err(SessionError::WorldNotFound { .. }) => {}
        other => panic!("expected WorldNotFound, got {other:?}"),
    }
}

#[test]
fn import_existing_world_fails() {
    let (host, id) = build_history_world();
    let export = host.export_world(&id).unwrap();
    match host.import_world("history", export) {
        Err(SessionError::WorldExists { .. }) => {}
        other => panic!("expected WorldExists, got {other:?}"),
    }
}

#[test]
fn import_rejects_unknown_format_version() {
    let (host, id) = build_history_world();
    let mut export = host.export_world(&id).unwrap();
    export.format_version = 999;
    let fresh = EmbeddedSessionManager::in_memory().unwrap();
    match fresh.import_world("history", export) {
        Err(SessionError::Store(graph_vcs::VcsError::Corrupt { .. })) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
    // A failed import must not leave a half-registered world behind.
    assert!(block_on(fresh.worlds()).unwrap().is_empty());
}

/// Native file-backed: an imported world lands as a `<slug>.graph` file plus
/// a manifest entry, so a fresh manager over the same root re-lists it with
/// its history intact.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn import_file_backed_persists() {
    let (source, id) = build_history_world();
    let export = source.export_world(&id).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("worlds");
    let imported_id = {
        let host = EmbeddedSessionManager::open(&root).unwrap();
        host.import_world("history", export).unwrap().id
    };
    assert!(root.join(format!("{}.graph", imported_id.0)).exists());

    let reopened = EmbeddedSessionManager::open(&root).unwrap();
    let handle = block_on(reopened.open_world(WorldSpec {
        name: "history".to_string(),
        description: None,
    }))
    .unwrap();
    let vcs = block_on(reopened.vcs(&handle.id)).unwrap();
    let source_vcs = block_on(source.vcs(&id)).unwrap();
    assert_eq!(
        canonical(&block_on(source_vcs.log("main", 100)).unwrap()),
        canonical(&block_on(vcs.log("main", 100)).unwrap()),
        "file-backed import must preserve the full commit log"
    );
}

/// Per-world GPU compute endpoints with the broker disabled (no
/// `JUMP_CANNON_SM_GPU_TEMPLATE`): `compute` stays `Null`, the session
/// endpoint reports `{"enabled": false}`, dispatch/park answer with the soft
/// error envelope, and dispatch/park stay writer-gated.
#[cfg(feature = "server")]
mod gpu_broker_disabled {
    use crate::kubernetes::KubernetesSessionManager;
    use crate::server::{router, ServerConfig};
    use crate::types::{ComputeHandle, WorldSpec};
    use crate::WorldHost;
    use std::sync::Arc;

    async fn spawn_server() -> (tempfile::TempDir, Arc<KubernetesSessionManager>, String) {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            KubernetesSessionManager::open(&dir.path().join("worlds")).unwrap(),
        );
        let app = router(manager.clone(), ServerConfig::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (dir, manager, format!("http://{addr}"))
    }

    #[tokio::test]
    async fn compute_is_null_without_broker() {
        let (_dir, manager, _base) = spawn_server().await;
        let handle = manager
            .open_world(WorldSpec {
                name: "gpu-less".to_string(),
                description: None,
            })
            .await
            .unwrap();
        assert_eq!(
            manager.compute(&handle.id).await.unwrap(),
            ComputeHandle::Null
        );
    }

    #[tokio::test]
    async fn session_endpoints_report_disabled() {
        let (_dir, manager, base) = spawn_server().await;
        let handle = manager
            .open_world(WorldSpec {
                name: "gpu-less".to_string(),
                description: None,
            })
            .await
            .unwrap();
        // The world was opened identity-less, so its writer is "system".
        let client = reqwest::Client::new();
        let world = &handle.id.0;

        let session: serde_json::Value = client
            .get(format!("{base}/api/worlds/{world}/compute/session"))
            .header("x-user", "anyone")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session, serde_json::json!({ "enabled": false }));

        let dispatch: serde_json::Value = client
            .post(format!("{base}/api/worlds/{world}/compute/dispatch"))
            .header("x-user", "system")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(dispatch["ok"], false);
        assert!(dispatch["error"].as_str().unwrap().contains("not enabled"));

        let park: serde_json::Value = client
            .post(format!("{base}/api/worlds/{world}/compute/park"))
            .header("x-user", "system")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(park["ok"], false);

        // Dispatch/park are writer-only: a non-writer gets 403 even with the
        // broker disabled.
        let forbidden = client
            .post(format!("{base}/api/worlds/{world}/compute/dispatch"))
            .header("x-user", "intruder")
            .send()
            .await
            .unwrap();
        assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    }
}

/// Run the conformance suite against the HTTP client backed by a live
/// in-process server (feature `server`). The joining identity is discovered
/// by the suite from the world ACL, so the client's configured user becomes
/// the world's creator (and sole writer) here.
#[cfg(feature = "server")]
mod http_conformance {
    use crate::conformance::check_worldhost;
    use crate::kubernetes::KubernetesSessionManager;
    use crate::server::{router, ServerConfig};
    use crate::types::UserIdentity;
    use crate::HttpSessionManager;
    use std::sync::Arc;

    #[tokio::test]
    async fn conformance_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            Arc::new(KubernetesSessionManager::open(&dir.path().join("worlds")).unwrap());
        let app = router(manager, ServerConfig::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let user = UserIdentity {
            name: "conformance".to_string(),
            groups: Vec::new(),
        };
        let host = HttpSessionManager::connect(format!("http://{addr}"), user)
            .await
            .unwrap();
        check_worldhost(Arc::new(host)).await;
    }
}
