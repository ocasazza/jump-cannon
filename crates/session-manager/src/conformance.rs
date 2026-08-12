//! Reusable [`WorldHost`] conformance suite.
//!
//! Any implementation — embedded, Kubernetes, HTTP client — plugs in by
//! calling [`check_worldhost`]. The suite asserts the interface contract
//! (panic-style, like normal tests) and returns once the host has survived
//! every check:
//!
//! 1. Descriptor sanity (`multi_user` consistent with `sessions()`).
//! 2. `open_world` appears in `worlds()` with one branch (`main`); opening
//!    the same name again fails with [`SessionError::WorldExists`].
//! 3. The world's `vcs` store round-trips a small [`GraphOp`] batch.
//! 4. `compute` returns the variant expected for the host kind.
//! 5. If a session directory exists: join → listed → members non-empty →
//!    leave → empty, plus the single-user bound for `multi_user == false`.
//! 6. `close_world` makes `vcs` fail with [`SessionError::WorldNotFound`].
//!
//! Compiled for this crate's tests always; downstream implementations enable
//! the `conformance` feature.

use crate::types::{
    ComputeHandle, HostKind, SessionError, UserIdentity, WorldId, WorldSpec,
};
use crate::WorldHost;
use graph_vcs::{GraphOp, NodeId, VaultEdge, VaultNode};
use std::sync::Arc;

/// Run the full conformance suite against `host`, panicking on the first
/// contract violation.
pub async fn check_worldhost(host: Arc<dyn WorldHost>) {
    let host = host.as_ref();

    // 1. Descriptor sanity.
    let descriptor = host.descriptor();
    assert!(
        !descriptor.id.is_empty(),
        "descriptor id must be non-empty"
    );
    if descriptor.multi_user {
        assert!(
            host.sessions().is_some(),
            "multi-user host must expose a session directory"
        );
    }

    // 2. open_world → listed with one branch; duplicate open → WorldExists.
    let spec = WorldSpec {
        name: "Conformance World".to_string(),
        description: Some("created by the conformance suite".to_string()),
    };
    let handle = host
        .open_world(spec.clone())
        .await
        .expect("open_world failed");
    let expected_id = WorldId::from_name(&spec.name).expect("spec name must slug");
    assert_eq!(handle.id, expected_id, "handle id must be the name slug");
    let worlds = host.worlds().await.expect("worlds failed");
    let info = worlds
        .iter()
        .find(|w| w.id == handle.id)
        .expect("opened world must be listed");
    assert_eq!(info.name, spec.name, "listing must keep the display name");
    assert_eq!(
        info.description, spec.description,
        "listing must keep the description"
    );
    assert_eq!(info.branches, 1, "a new world has exactly the main branch");
    match host.open_world(spec).await {
        Err(SessionError::WorldExists { .. }) => {}
        other => panic!("re-opening an open world must fail with WorldExists, got {other:?}"),
    }

    // 3. vcs round-trip: commit a node with frontmatter plus an edge, then
    //    materialize the head.
    let vcs = host.vcs(&handle.id).await.expect("vcs failed");
    let mut node = VaultNode {
        id: "alpha".to_string(),
        ..Default::default()
    };
    node.meta.title = "alpha".to_string();
    node.meta
        .frontmatter
        .insert("status".to_string(), serde_json::Value::String("draft".to_string()));
    let commit = vcs
        .commit(
            "main",
            vec![
                GraphOp::UpsertNode(node),
                GraphOp::UpsertEdge(VaultEdge {
                    source: "alpha".to_string(),
                    target: "beta".to_string(),
                }),
            ],
            "conformance",
            "add alpha and edge",
        )
        .await
        .expect("commit failed");
    let snapshot = vcs.materialize(&commit.id).await.expect("materialize failed");
    let stored = snapshot
        .nodes
        .get(&NodeId("alpha".to_string()))
        .expect("node alpha must survive the round-trip");
    assert_eq!(
        stored.meta.frontmatter.get("status"),
        Some(&serde_json::Value::String("draft".to_string())),
        "frontmatter must survive the round-trip"
    );
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.source == "alpha" && e.target == "beta"),
        "edge must survive the round-trip"
    );
    let log = vcs.log("main", 10).await.expect("log failed");
    assert!(
        log.iter().any(|c| c.message == "world created"),
        "every world is born with an initial 'world created' commit"
    );

    // 4. compute variant matches the host kind.
    let compute = host.compute(&handle.id).await.expect("compute failed");
    match descriptor.kind {
        HostKind::Embedded => assert_eq!(
            compute,
            ComputeHandle::InProcess,
            "embedded hosts report in-process compute"
        ),
        // Later kinds wire real endpoints; any Ok variant is acceptable.
        HostKind::Kubernetes | HostKind::Remote => {}
    }

    // 5. Session directory contract, when present.
    if let Some(directory) = host.sessions() {
        // Discover a joinable identity from the ACL rather than hard-coding
        // one: single-user hosts admit only their local user.
        let acl = directory
            .members(&handle.id)
            .await
            .expect("members failed");
        assert!(
            !acl.readers.is_empty() || !acl.writers.is_empty(),
            "members must be non-empty"
        );
        let user = UserIdentity {
            name: acl
                .writers
                .first()
                .or(acl.readers.first())
                .expect("non-empty acl")
                .clone(),
            groups: Vec::new(),
        };
        let session = directory
            .join(&handle.id, &user)
            .await
            .expect("join failed");
        let again = directory
            .join(&handle.id, &user)
            .await
            .expect("re-join failed");
        assert_eq!(session.id, again.id, "joining twice is idempotent");
        let live = directory
            .sessions(&handle.id)
            .await
            .expect("sessions failed");
        assert!(
            live.iter().any(|s| s.id == session.id),
            "joined session must be listed"
        );
        if !descriptor.multi_user {
            // A second distinct identity must NOT produce a second concurrent
            // session: either rejected or folded into the existing one.
            let intruder = UserIdentity {
                name: "conformance-intruder".to_string(),
                groups: Vec::new(),
            };
            match directory.join(&handle.id, &intruder).await {
                Err(_) => {}
                Ok(other) => assert_eq!(
                    other.id, session.id,
                    "single-user host must not hold two concurrent sessions"
                ),
            }
        }
        directory.leave(&session.id).await.expect("leave failed");
        let live = directory
            .sessions(&handle.id)
            .await
            .expect("sessions after leave failed");
        assert!(
            live.iter().all(|s| s.id != session.id),
            "left session must be gone"
        );
    }

    // 6. close_world → vcs fails with WorldNotFound.
    host.close_world(&handle.id).await.expect("close_world failed");
    match host.vcs(&handle.id).await {
        Err(SessionError::WorldNotFound { .. }) => {}
        Err(e) => panic!("vcs on a closed world must fail with WorldNotFound, got {e:?}"),
        Ok(_) => panic!("vcs on a closed world must fail with WorldNotFound, got Ok"),
    }
}
