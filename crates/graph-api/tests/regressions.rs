//! HTTP-level regression tests for the axum router.
//!
//! Each test pins a server-side bug we already paid for once. They drive
//! the `Router` returned by [`graph_api::router`] via tower's `oneshot`,
//! avoiding the need for a real TCP socket / async runtime spin-up.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use prost::Message;
use tower::ServiceExt; // for `oneshot`

use data_loader::{
    Capability, Effect, HostedImporter, ImportError, ImportFuture, Importer, ImporterDescriptor,
    LoadResult, Loader, Transport,
};
use graph_api::proto::{Init, NodeMeta};
use graph_api::state::GraphSnapshot;
use graph_api::AppState;
use vault_data::{VaultEdge, VaultGraph, VaultNode};

/// A stub loader that always returns an empty graph. Used in tests that
/// don't need a real data source.
struct EmptyLoader;

impl Loader for EmptyLoader {
    fn name(&self) -> &str {
        "empty"
    }

    fn load(&self) -> LoadResult {
        LoadResult {
            graph: VaultGraph::new(),
            unresolved: Vec::new(),
        }
    }
}

struct DeclaredButUngrantedWrite;

impl Importer for DeclaredButUngrantedWrite {
    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new(
            "ungranted-write",
            "Ungranted write",
            "1",
            vec![
                Capability::new(
                    Effect::Read,
                    Transport::Filesystem,
                    "/tmp/jump-cannon-test-empty-vault",
                ),
                Capability::new(
                    Effect::ContentWrite,
                    Transport::Filesystem,
                    "/tmp/jump-cannon-test-empty-vault",
                ),
            ],
        )
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async {
            Ok(LoadResult {
                graph: VaultGraph::new(),
                unresolved: Vec::new(),
            })
        })
    }
}

fn trust_test_importer(importer: Box<dyn Importer>) -> HostedImporter {
    let grants = importer.descriptor().capabilities;
    HostedImporter::new(importer, grants).unwrap()
}

/// Build an `AppState` over an empty `VaultGraph`. No vault-search
/// subprocess, no asset dir — enough to exercise the protobuf endpoints.
fn empty_state() -> AppState {
    state_with_graph(VaultGraph::new())
}

fn state_with_graph(graph: VaultGraph) -> AppState {
    AppState::new(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        trust_test_importer(Box::new(EmptyLoader)),
        graph,
        None,
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
}

fn two_node_graph(prefix: &str) -> VaultGraph {
    let mut graph = VaultGraph::new();
    graph.add_node(VaultNode {
        id: format!("{prefix}-a"),
        ..Default::default()
    });
    graph.add_node(VaultNode {
        id: format!("{prefix}-b"),
        ..Default::default()
    });
    graph.add_edge(VaultEdge {
        source: format!("{prefix}-a"),
        target: format!("{prefix}-b"),
    });
    graph
}

fn response_revision(resp: &axum::response::Response) -> u64 {
    resp.headers()
        .get("x-graph-revision")
        .expect("X-Graph-Revision header")
        .to_str()
        .expect("revision header is text")
        .parse()
        .expect("revision header is u64")
}

/// `/node/<missing-id>` regression: previously returned 404 + a noisy
/// `[graph-renderer]` error log. Now returns a stub `NodeMeta` with
/// `doctype = Some("external")` so the renderer can show *something*.
#[tokio::test]
async fn node_meta_stub_for_missing_id() {
    let state = empty_state();
    let app = graph_api::router(state);

    // The route is `/node/:id`, so the renderer URL-encodes embedded
    // slashes. We do the same here: the *decoded* id is a deep vault
    // path, which is exactly the shape that originally returned 404.
    let decoded_id = "some/deeply/nested/path/Missing.md";
    let encoded_id = decoded_id.replace('/', "%2F");
    let req = Request::builder()
        .uri(format!("/node/{encoded_id}"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "missing-id node lookup must not 404 — see KB-404 stub",
    );
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(
        ct, "application/x-protobuf",
        "stub must be served as protobuf"
    );

    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let meta = NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta");

    assert_eq!(
        meta.doctype.as_deref(),
        Some("external"),
        "stub must mark itself with doctype=\"external\""
    );
    assert!(
        meta.tags.is_empty(),
        "stub must have empty tags; got {:?}",
        meta.tags,
    );
    assert_eq!(meta.pagerank, 0.0, "stub pagerank must be zero");
    assert_eq!(meta.degree, 0, "stub degree must be zero");
    assert_eq!(meta.community, 0, "stub community must be zero");
    assert_eq!(
        meta.id, decoded_id,
        "stub id must echo the (decoded) request path"
    );
    // Title = last path segment, folder = everything before. Pin both so
    // a future "smarter" id-splitter doesn't silently drift.
    assert_eq!(meta.title, "Missing.md");
    assert_eq!(meta.folder, "some/deeply/nested/path");
    assert!(!meta.content_readable);
    assert!(!meta.content_writable);
}

/// A non-Obsidian importer cannot reach the legacy vault filesystem writer,
/// even if a client guesses a valid-looking relative path.
#[tokio::test]
async fn vault_write_requires_obsidian_content_effect() {
    let app = graph_api::router(empty_state());
    let body = serde_json::to_vec(&serde_json::json!({
        "path": "some/note",
        "body": "replacement"
    }))
    .unwrap();
    let req = Request::builder()
        .method("PUT")
        .uri("/vault/page")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn declared_but_ungranted_content_write_is_forbidden() {
    let importer = HostedImporter::new(
        Box::new(DeclaredButUngrantedWrite),
        [Capability::new(
            Effect::Read,
            Transport::Filesystem,
            "/tmp/jump-cannon-test-empty-vault",
        )],
    )
    .unwrap();
    let state = AppState::new(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        importer,
        VaultGraph::new(),
        None,
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    );
    let app = graph_api::router(state);
    let body = serde_json::to_vec(&serde_json::json!({
        "path": "some/note",
        "body": "replacement"
    }))
    .unwrap();
    let req = Request::builder()
        .method("PUT")
        .uri("/vault/page")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Every independently fetched graph buffer identifies the exact snapshot it
/// came from. This is what lets the frontend reject a reload that lands
/// between `/graph/init` and the bulk requests.
#[tokio::test]
async fn graph_endpoints_advertise_one_snapshot_revision() {
    let state = state_with_graph(two_node_graph("first"));
    let revision = state.snapshot().revision;
    let app = graph_api::router(state);

    let init_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/init")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("init served");
    assert_eq!(init_resp.status(), StatusCode::OK);
    let init = Init::decode(
        to_bytes(init_resp.into_body(), 1 << 20)
            .await
            .expect("init body")
            .as_ref(),
    )
    .expect("decode Init");
    assert_eq!(init.graph_revision, revision);

    for path in [
        "/graph/ids",
        "/graph/positions",
        "/graph/edges",
        "/graph/metrics/community",
        "/graph/meta_summary",
        "/graph/csr.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap_or_else(|e| panic!("{path} served: {e}"));
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert_eq!(response_revision(&resp), revision, "{path}");
    }
}

/// Equal node counts are not graph identity. Swapping to a different graph of
/// the same size must advance the revision and expose the replacement IDs.
#[tokio::test]
async fn same_cardinality_snapshot_swap_changes_revision() {
    let state = state_with_graph(two_node_graph("before"));
    let first_revision = state.snapshot().revision;
    state
        .inner
        .snapshot
        .store(Arc::new(GraphSnapshot::build(two_node_graph("after"))));
    let second_revision = state.snapshot().revision;
    assert_ne!(first_revision, second_revision);

    let resp = graph_api::router(state)
        .oneshot(
            Request::builder()
                .uri("/graph/ids")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("ids served");
    assert_eq!(response_revision(&resp), second_revision);
    let ids: Vec<String> =
        serde_json::from_slice(&to_bytes(resp.into_body(), 1 << 20).await.expect("ids body"))
            .expect("ids json");
    assert_eq!(ids, ["after-a", "after-b"]);
}

/// Belt-and-braces: keep `AppState` constructable from outside the crate.
/// If a future refactor makes `AppState::new` private, this test fails to
/// compile and reminds us to ship a public test-only constructor instead
/// of breaking integration tests silently.
#[allow(dead_code)]
fn _state_constructor_is_public() -> AppState {
    AppState::new(
        std::path::PathBuf::new(),
        trust_test_importer(Box::new(EmptyLoader)),
        VaultGraph::new(),
        None,
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
}

// Silence "unused import" if Arc ever becomes unused; keeps the test
// file honest with whatever helpers it actually exercises today.
#[allow(dead_code)]
fn _arc_keepalive() -> Option<Arc<()>> {
    None
}

// ── POST /generate (server-side tvix backend) ───────────────────────────────

/// A valid generate-expression evaluated server-side returns the expected
/// `{ ok: true, graph: { nodes, links } }` with the right counts. This is the
/// PRIMARY WASM non-freeze backend: the heavy `eval_graph` runs here, not on
/// the browser thread.
#[tokio::test]
async fn generate_ok_returns_graph_counts() {
    let app = graph_api::router(empty_state());

    // A hand-written toGraphJSON-shaped attrset: 3 nodes, 2 links. No library
    // import needed — keeps the assertion about counts, not the embedded lib.
    let expr = r#"{
        nodes = [ { id = "a"; type = "x"; } { id = "b"; } { id = "c"; } ];
        links = [ { source = "a"; target = "b"; } { source = "b"; target = "c"; } ];
    }"#;
    let body = serde_json::to_vec(&serde_json::json!({ "expr": expr })).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/generate")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(v["ok"], serde_json::json!(true), "resp: {v}");
    let graph = &v["graph"];
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(graph["links"].as_array().unwrap().len(), 2);
    // The optional `type` field round-trips, and absent kinds stay absent.
    assert_eq!(graph["nodes"][0]["type"], serde_json::json!("x"));
    assert!(graph["nodes"][1].get("type").is_none());
}

/// `/generate` is evaluation-only: returning a browser-owned graph must not
/// silently replace graph-api's active vault snapshot, even if the generated
/// graph happens to have the same node count.
#[tokio::test]
async fn generate_does_not_replace_active_graph_or_revision() {
    let state = state_with_graph(two_node_graph("hosted"));
    let before_revision = state.snapshot().revision;
    let before_ids = state.snapshot().idx_to_id.clone();
    let app = graph_api::router(state.clone());

    let expr = r#"{
        nodes = [ { id = "generated-a"; } { id = "generated-b"; } ];
        links = [ { source = "generated-a"; target = "generated-b"; } ];
    }"#;
    let body = serde_json::to_vec(&serde_json::json!({ "expr": expr })).unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/generate")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("generate served");
    assert_eq!(resp.status(), StatusCode::OK);
    let generated: serde_json::Value = serde_json::from_slice(
        &to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("generate body"),
    )
    .expect("generate json");
    assert_eq!(generated["ok"], serde_json::json!(true));
    assert_eq!(generated["graph"]["nodes"].as_array().unwrap().len(), 2);

    assert_eq!(state.snapshot().revision, before_revision);
    assert_eq!(state.snapshot().idx_to_id, before_ids);
}

/// The embedded graph library is reachable server-side too: a `starGen` via the
/// in-VFS combinators evaluates to the expected star shape.
#[tokio::test]
async fn generate_uses_embedded_library() {
    let app = graph_api::router(empty_state());
    let expr = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.starGen { nodes = 5; prefix = "n"; })
    "#;
    let body = serde_json::to_vec(&serde_json::json!({ "expr": expr })).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/generate")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(v["ok"], serde_json::json!(true), "resp: {v}");
    assert_eq!(v["graph"]["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(v["graph"]["links"].as_array().unwrap().len(), 4);
}

/// A bad expression returns the soft-error envelope: HTTP 200 with
/// `{ ok: false, error }` (NOT a 5xx), so the client surfaces the eval message
/// inline exactly like the local path.
#[tokio::test]
async fn generate_bad_expr_is_soft_error() {
    let app = graph_api::router(empty_state());
    let body = serde_json::to_vec(&serde_json::json!({ "expr": "let x = in" })).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/generate")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "eval failure must be a soft error, not a 5xx",
    );
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(
        v["error"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
        "expected a non-empty error message; got {v}",
    );
    assert!(v.get("graph").is_none(), "no graph on error: {v}");
}

/// Valid Nix + valid JSON but NOT a `{ nodes, links }` graph is also a soft
/// error, with the shape-mismatch message surfaced.
#[tokio::test]
async fn generate_non_graph_result_is_soft_error() {
    let app = graph_api::router(empty_state());
    let body = serde_json::to_vec(&serde_json::json!({ "expr": "{ foo = 1; }" })).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/generate")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(
        v["error"].as_str().unwrap_or("").contains("nodes, links"),
        "expected a shape-mismatch error; got {v}",
    );
}
