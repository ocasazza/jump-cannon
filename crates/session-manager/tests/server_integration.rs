//! End-to-end tests for the multi-user session-manager server: auth, the
//! world/VCS REST API, ACL gating, sessions, and the nested per-world graph
//! serving chain (world mux → WorldImporter → push-trigger rebuild).
#![cfg(feature = "server")]

use graph_vcs::GraphOp;
use serde_json::{json, Value};
use session_manager::kubernetes::KubernetesSessionManager;
use session_manager::server::{router, ServerConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vault_data::VaultNode;

async fn spawn_server() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(KubernetesSessionManager::open(&dir.path().join("worlds")).unwrap());
    let app = router(manager, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), dir)
}

fn as_user(request: reqwest::RequestBuilder, user: &str) -> reqwest::RequestBuilder {
    request.header("x-user", user)
}

fn node(id: &str, title: &str) -> Value {
    serde_json::to_value(GraphOp::UpsertNode(VaultNode {
        id: id.to_string(),
        meta: vault_data::NodeMeta {
            title: title.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }))
    .unwrap()
}

async fn create_world(http: &reqwest::Client, base: &str, user: &str, name: &str) -> String {
    let resp = as_user(http.post(format!("{base}/api/worlds")), user)
        .json(&json!({ "name": name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "create world {name:?}");
    let handle: Value = resp.json().await.unwrap();
    handle["id"].as_str().unwrap().to_string()
}

async fn commit(
    http: &reqwest::Client,
    base: &str,
    user: &str,
    world: &str,
    branch: &str,
    ops: Vec<Value>,
    message: &str,
) -> reqwest::Response {
    as_user(
        http.post(format!("{base}/api/worlds/{world}/vcs/commits")),
        user,
    )
    .json(&json!({ "branch": branch, "ops": ops, "message": message }))
    .send()
    .await
    .unwrap()
}

#[tokio::test]
async fn auth_header_required() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();

    // /healthz is the only exemption.
    let resp = http.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Missing header → 401 JSON.
    let resp = http.get(format!("{base}/api/worlds")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["kind"], "unauthorized");

    // Empty/whitespace header → 401. The graph-serving surface is gated too.
    let resp = as_user(http.get(format!("{base}/api/worlds")), "  ")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = http
        .get(format!("{base}/worlds/anything/graph/ids"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Present → 200.
    let resp = as_user(http.get(format!("{base}/api/worlds")), "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn world_create_commit_serves_graph() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();

    let id = create_world(&http, &base, "alice", "Alpha World").await;
    assert_eq!(id, "alpha-world");

    // Duplicate open → 409.
    let resp = as_user(http.post(format!("{base}/api/worlds")), "alice")
        .json(&json!({ "name": "Alpha World" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // A new world serves one valid empty graph.
    let resp = as_user(
        http.get(format!("{base}/worlds/alpha-world/graph/ids")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let ids: Vec<String> = resp.json().await.unwrap();
    assert!(ids.is_empty());

    // Unknown world → JSON 404.
    let resp = as_user(http.get(format!("{base}/worlds/nope/graph/ids")), "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["kind"], "world_not_found");

    // Commit on main, then the push-trigger rebuild must land in the served
    // graph (this exercises mux → WorldImporter → push trigger end to end).
    let resp = commit(
        &http,
        &base,
        "alice",
        "alpha-world",
        "main",
        vec![node("node-1", "First node")],
        "add node",
    )
    .await;
    assert_eq!(resp.status(), 200);
    let commit: Value = resp.json().await.unwrap();
    assert_eq!(commit["author"], "alice");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let resp = as_user(
            http.get(format!("{base}/worlds/alpha-world/graph/ids")),
            "alice",
        )
        .send()
        .await
        .unwrap();
        assert_eq!(resp.status(), 200);
        let ids: Vec<String> = resp.json().await.unwrap();
        if ids == ["node-1"] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "served graph never picked up the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // /graph/init is served too (protobuf init manifest; the revision rides
    // in the message body, not a header).
    let resp = as_user(
        http.get(format!("{base}/worlds/alpha-world/graph/init")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(!resp.bytes().await.unwrap().is_empty());

    // The world reports no compute until M4 wires the GPU broker.
    let resp = as_user(
        http.get(format!("{base}/api/worlds/alpha-world/compute")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap(), json!("null"));
}

#[tokio::test]
async fn merge_conflict_then_resolve() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();
    create_world(&http, &base, "alice", "Beta").await;

    // Base commit on main; branch feature from it.
    let resp = commit(&http, &base, "alice", "beta", "main", vec![node("n", "v1")], "base").await;
    assert_eq!(resp.status(), 200);
    let base_commit: Value = resp.json().await.unwrap();
    let resp = as_user(
        http.post(format!("{base}/api/worlds/beta/vcs/branches")),
        "alice",
    )
    .json(&json!({ "name": "feature", "from_commit": base_commit["id"] }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);

    // Diverge: both sides edit the same node title.
    commit(&http, &base, "alice", "beta", "main", vec![node("n", "v2")], "main edit").await;
    commit(
        &http,
        &base,
        "alice",
        "beta",
        "feature",
        vec![node("n", "v3")],
        "feature edit",
    )
    .await;

    // Merge records the conflict without blocking.
    let resp = as_user(
        http.post(format!("{base}/api/worlds/beta/vcs/merges")),
        "alice",
    )
    .json(&json!({ "into": "main", "from": "feature", "message": "merge feature" }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let report: Value = resp.json().await.unwrap();
    assert_eq!(report["status"], "merged");
    assert_eq!(report["conflicts"].as_array().unwrap().len(), 1);

    let resp = as_user(
        http.get(format!("{base}/api/worlds/beta/vcs/conflicts?branch=main")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap().as_array().unwrap().len(), 1);

    // Resolve theirs → conflicts clear, the merged value is published.
    let resp = as_user(
        http.post(format!("{base}/api/worlds/beta/vcs/resolutions")),
        "alice",
    )
    .json(&json!({
        "branch": "main",
        "resolutions": [{ "node_id": "n", "choice": "Theirs" }],
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = as_user(
        http.get(format!("{base}/api/worlds/beta/vcs/conflicts?branch=main")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert!(resp.json::<Value>().await.unwrap().as_array().unwrap().is_empty());

    let resp = as_user(
        http.get(format!("{base}/api/worlds/beta/vcs/log?branch=main&limit=1")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    let head = resp.json::<Value>().await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = as_user(
        http.get(format!("{base}/api/worlds/beta/vcs/snapshots/{head}")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let snapshot: Value = resp.json().await.unwrap();
    assert_eq!(snapshot["nodes"]["n"]["meta"]["title"], "v3");
}

#[tokio::test]
async fn acl_write_gating() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();
    create_world(&http, &base, "alice", "Gamma").await;

    // The creator is the sole initial writer.
    let resp = as_user(
        http.get(format!("{base}/api/worlds/gamma/members")),
        "eve",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        json!({ "readers": [], "writers": ["alice"] })
    );

    // A non-writer cannot commit, replace the ACL, or close the world…
    let resp = commit(&http, &base, "bob", "gamma", "main", vec![node("x", "x")], "bob").await;
    assert_eq!(resp.status(), 403);
    let resp = as_user(http.put(format!("{base}/api/worlds/gamma/acl")), "bob")
        .json(&json!({ "readers": [], "writers": ["bob"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let resp = as_user(http.delete(format!("{base}/api/worlds/gamma")), "bob")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // …but any authenticated user may read.
    let resp = as_user(
        http.get(format!("{base}/api/worlds/gamma/vcs/log?branch=main")),
        "eve",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);

    // The writer adds bob; bob can now commit.
    let resp = as_user(http.put(format!("{base}/api/worlds/gamma/acl")), "alice")
        .json(&json!({ "readers": [], "writers": ["alice", "bob"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = commit(&http, &base, "bob", "gamma", "main", vec![node("x", "x")], "bob").await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn sessions_join_leave() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();
    create_world(&http, &base, "alice", "Delta").await;

    // Join is idempotent per (world, user).
    let resp = as_user(
        http.post(format!("{base}/api/worlds/delta/sessions")),
        "bob",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let session: Value = resp.json().await.unwrap();
    assert_eq!(session["user"]["name"], "bob");
    let sid = session["id"].as_str().unwrap().to_string();

    let resp = as_user(
        http.post(format!("{base}/api/worlds/delta/sessions")),
        "bob",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.json::<Value>().await.unwrap()["id"], sid);

    let resp = as_user(
        http.get(format!("{base}/api/worlds/delta/sessions")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    let listed: Value = resp.json().await.unwrap();
    assert!(listed.as_array().unwrap().iter().any(|s| s["id"] == sid));

    // Only the owner (or a world writer) may detach a session.
    let resp = as_user(
        http.delete(format!("{base}/api/worlds/delta/sessions/{sid}")),
        "eve",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 403);

    let resp = as_user(
        http.delete(format!("{base}/api/worlds/delta/sessions/{sid}")),
        "bob",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = as_user(
        http.get(format!("{base}/api/worlds/delta/sessions")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert!(resp.json::<Value>().await.unwrap().as_array().unwrap().is_empty());

    // Leaving twice → 404.
    let resp = as_user(
        http.delete(format!("{base}/api/worlds/delta/sessions/{sid}")),
        "bob",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn close_unmounts_graph_serving_and_vcs() {
    let (base, _dir) = spawn_server().await;
    let http = reqwest::Client::new();
    create_world(&http, &base, "alice", "Epsilon").await;

    let resp = as_user(http.delete(format!("{base}/api/worlds/epsilon")), "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // After close, the VCS and serving surfaces are gone (files stay).
    let resp = as_user(
        http.get(format!("{base}/api/worlds/epsilon/vcs/log")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = as_user(
        http.get(format!("{base}/worlds/epsilon/graph/ids")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 404);

    // Re-opening the closed world keeps its history (close is not delete).
    let resp = as_user(http.post(format!("{base}/api/worlds")), "alice")
        .json(&json!({ "name": "Epsilon" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = as_user(
        http.get(format!("{base}/api/worlds/epsilon/vcs/log?branch=main")),
        "alice",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let log: Value = resp.json().await.unwrap();
    assert!(log
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["message"] == "world created"));
}
