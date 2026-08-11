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
    Capability, ContentSchema, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect,
    HostedImporter, ImportError, ImportFuture, Importer, ImporterDescriptor, ImporterSchema,
    LoadResult, Loader, SearchDocument, TagHierarchySchema, Transport,
};
use graph_api::importer_catalog::ImporterCatalog;
use graph_api::proto::{Init, MetaSummary, NodeMeta};
use graph_api::state::{GraphSnapshot, SnapshotSource};
use graph_api::AppState;
use vault_data::{VaultEdge, VaultGraph, VaultNode};

/// A stub loader that always returns an empty graph. Used in tests that
/// don't need a real data source.
struct EmptyLoader;

fn test_schema() -> ImporterSchema {
    ImporterSchema::new(
        "generate",
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(4),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(2)
                .facetable(),
        ],
        vec![EdgeTypeSchema::directed("reference", "test edge")],
        TagHierarchySchema::slash(),
    )
}

fn load_result(mut graph: VaultGraph) -> LoadResult {
    // The shared identity contract requires namespaced node IDs; rewrite the
    // fixtures' bare IDs into the `generate:test:` namespace.
    let bare_ids: Vec<String> = graph.nodes.keys().cloned().collect();
    for bare in &bare_ids {
        let node = graph.nodes.shift_remove(bare).expect("fixture node");
        let namespaced = format!("generate:test:{bare}");
        graph.nodes.insert(
            namespaced.clone(),
            VaultNode {
                id: namespaced,
                ..node
            },
        );
    }
    for edge in &mut graph.edges {
        edge.source = format!("generate:test:{}", edge.source);
        edge.target = format!("generate:test:{}", edge.target);
    }
    let search_documents = graph
        .nodes
        .values_mut()
        .map(|node| {
            if node.meta.source_id.is_empty() {
                node.meta.source_id = "test".into();
            }
            if node.meta.title.is_empty() {
                node.meta.title = node.id.clone();
            }
            SearchDocument::new(&node.id)
                .with("id", node.id.clone())
                .with("title", node.meta.title.clone())
                .with("tags", serde_json::json!(node.meta.tags))
        })
        .collect();
    LoadResult {
        graph,
        search_documents,
        unresolved: Vec::new(),
    }
}

impl Loader for EmptyLoader {
    fn name(&self) -> &str {
        "empty"
    }

    fn schema(&self) -> ImporterSchema {
        test_schema()
    }

    fn load(&self) -> LoadResult {
        load_result(VaultGraph::new())
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
                    Effect::ContentRead,
                    Transport::Filesystem,
                    "/tmp/jump-cannon-test-empty-vault",
                ),
                Capability::new(
                    Effect::ContentWrite,
                    Transport::Filesystem,
                    "/tmp/jump-cannon-test-empty-vault",
                ),
            ],
            test_schema().with_content(ContentSchema {
                readable: true,
                writable: true,
                media_types: vec!["text/markdown".into()],
            }),
        )
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async { Ok(load_result(VaultGraph::new())) })
    }
}

fn trust_test_importer(importer: Box<dyn Importer>) -> HostedImporter {
    let grants = importer.descriptor().capabilities;
    HostedImporter::new(importer, grants).unwrap()
}

/// Build an `AppState` over an empty `VaultGraph`. No asset dir — enough
/// to exercise the protobuf endpoints.
fn empty_state() -> AppState {
    state_with_graph(VaultGraph::new())
}

fn state_with_graph(graph: VaultGraph) -> AppState {
    AppState::new(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        trust_test_importer(Box::new(EmptyLoader)),
        load_result(graph),
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
    .unwrap()
}

fn state_with_catalog(catalog: ImporterCatalog) -> AppState {
    AppState::new_with_importer_catalog(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        trust_test_importer(Box::new(EmptyLoader)),
        load_result(VaultGraph::new()),
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
        catalog,
    )
    .unwrap()
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

#[tokio::test]
async fn importer_catalog_is_read_only_sorted_and_sanitized() {
    let raw = r#"{
      "selected": "lavender-ingest-okf",
      "sources": {
        "other-vault": {
          "displayName": "Other vault",
          "kind": "obsidian"
        },
        "lavender-ingest-okf": {
          "displayName": "Lavender ingest OKF",
          "description": "Deployment-provisioned read-only OKF repository",
          "kind": "okf",
          "sourceId": "lavender-ingest",
          "filesystemRescanIntervalSeconds": 60,
          "source": {
            "volumeName": "lavender-okf-repository",
            "existingClaim": "lavender-okf-shared",
            "mountPath": "/var/lib/lavender/okf-repository",
            "path": "/var/lib/lavender/okf-repository/okf",
            "readOnly": true
          },
          "producer": {
            "chart": "lavender-ingest",
            "defaultClaim": "lavender-ingest-okf",
            "repositoryRoot": "/data/okf-repository",
            "workflowInput": "/data/okf-repository/okf",
            "existingClaimValuePath": "okf.persistence.existingClaim",
            "existingClaimValue": "lavender-okf-shared"
          }
        }
      }
    }"#;
    let catalog = ImporterCatalog::parse(Some(raw), data_loader::SourceKind::Okf).unwrap();
    let app = graph_api::router(state_with_catalog(catalog));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/importers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("importer catalog body"),
    )
    .expect("importer catalog JSON");

    assert_eq!(body["activation"], "helm_rollout");
    assert_eq!(body["selected"], "lavender-ingest-okf");
    assert_eq!(body["active"]["kind"], "okf");
    assert_eq!(body["active"]["importer"]["id"], "empty");
    assert_eq!(body["sources"][0]["id"], "lavender-ingest-okf");
    assert_eq!(body["sources"][1]["id"], "other-vault");
    assert_eq!(
        body["sources"][0]["source"]["path"],
        "/var/lib/lavender/okf-repository/okf"
    );
    assert_eq!(body["sources"][0]["source"]["readOnly"], true);
    assert!(body.get("capabilities").is_none());
    assert!(!body.to_string().contains("token"));

    let mutation = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/importers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mutation.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn importer_catalog_default_state_keeps_active_identity_without_a_kind() {
    // AppState::new predates deployment catalogs and remains the compatibility
    // constructor for embedders. Its endpoint still succeeds and advertises
    // the active importer identity, while kind is absent until a trusted host
    // supplies the runtime source kind through new_with_importer_catalog.
    let response = graph_api::router(empty_state())
        .oneshot(
            Request::builder()
                .uri("/importers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("default importer catalog body"),
    )
    .expect("default importer catalog JSON");
    assert_eq!(body["activation"], "helm_rollout");
    assert!(body.get("selected").is_none());
    assert!(body["active"].get("kind").is_none());
    assert_eq!(body["active"]["importer"]["id"], "empty");
    assert_eq!(body["active"]["importer"]["name"], "empty");
    assert!(!body["active"]["importer"]["version"]
        .as_str()
        .unwrap()
        .is_empty());
    assert_eq!(body["sources"], serde_json::json!([]));
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
        load_result(VaultGraph::new()),
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
    .unwrap();
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
    let loaded = load_result(two_node_graph("after"));
    state.inner.snapshot.store(Arc::new(
        GraphSnapshot::build(
            loaded.graph,
            SnapshotSource::new("test", "Test", "1"),
            test_schema(),
            loaded.search_documents,
        )
        .unwrap(),
    ));
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
    assert_eq!(ids, ["generate:test:after-a", "generate:test:after-b"]);
}

#[tokio::test]
async fn search_matches_returns_every_dense_index_with_its_snapshot_revision() {
    let mut graph = VaultGraph::new();
    for i in 0..75 {
        graph.add_node(VaultNode {
            id: format!("matching-{i:03}"),
            meta: vault_data::NodeMeta {
                title: "Shared match term".into(),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    graph.add_node(VaultNode {
        id: "not-matching".into(),
        meta: vault_data::NodeMeta {
            title: "Unrelated document".into(),
            ..Default::default()
        },
        ..Default::default()
    });

    let state = state_with_graph(graph);
    let revision = state.snapshot().revision;
    let mut expected: Vec<u32> = state
        .snapshot()
        .id_to_idx
        .iter()
        .filter_map(|(id, &index)| id.starts_with("generate:test:matching-").then_some(index))
        .collect();
    expected.sort_unstable();

    let response = graph_api::router(state)
        .oneshot(
            Request::builder()
                .uri("/search/matches?q=shared")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("search matches served");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_revision(&response), revision);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
    let body = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("search matches body");
    let actual: Vec<u32> = body
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        actual.len(),
        75,
        "the endpoint must not inherit the 50-hit UI limit"
    );
}

#[tokio::test]
async fn search_matches_rejects_invalid_query_syntax() {
    let response = graph_api::router(state_with_graph(two_node_graph("query")))
        .oneshot(
            Request::builder()
                .uri("/search/matches?q=secret%3Avalue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("invalid search matches served");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn schema_search_and_facets_share_the_importer_contract() {
    let mut graph = VaultGraph::new();
    graph.add_node(VaultNode {
        id: "schema-node".into(),
        meta: vault_data::NodeMeta {
            source_id: "test".into(),
            title: "Opaque title".into(),
            tags: vec!["revenue".into()],
            ..Default::default()
        },
        ..Default::default()
    });
    let state = state_with_graph(graph);
    let revision = state.snapshot().revision;
    let app = graph_api::router(state);

    let schema_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/schema")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("schema served");
    assert_eq!(schema_response.status(), StatusCode::OK);
    let schema: serde_json::Value = serde_json::from_slice(
        &to_bytes(schema_response.into_body(), 1 << 20)
            .await
            .expect("schema body"),
    )
    .expect("schema json");
    assert_eq!(schema["graph_revision"], revision);
    assert_eq!(schema["source"]["id"], "empty");
    assert_eq!(schema["schema"]["schema_version"], 2);
    assert_eq!(schema["schema"]["tag_hierarchy"]["separator"], "/");
    assert!(schema["schema"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["key"] == "tags"
            && field["searchable"] == true
            && field["facetable"] == true));

    let search_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/search/rich?q=tags%3Arevenue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("search served");
    assert_eq!(search_response.status(), StatusCode::OK);
    let search: serde_json::Value = serde_json::from_slice(
        &to_bytes(search_response.into_body(), 1 << 20)
            .await
            .expect("search body"),
    )
    .expect("search json");
    assert_eq!(search["total"], 1);
    assert_eq!(search["results"][0]["id"], "generate:test:schema-node");

    let invalid_query = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/search/rich?q=secret%3Avalue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("invalid search served");
    assert_eq!(invalid_query.status(), StatusCode::BAD_REQUEST);

    let facets_response = app
        .oneshot(
            Request::builder()
                .uri("/graph/meta_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("facets served");
    assert_eq!(facets_response.status(), StatusCode::OK);
    assert_eq!(response_revision(&facets_response), revision);
    let facets = MetaSummary::decode(
        to_bytes(facets_response.into_body(), 1 << 20)
            .await
            .expect("facets body")
            .as_ref(),
    )
    .expect("decode facets");
    assert_eq!(facets.fields, ["tags"]);
    assert_eq!(facets.buckets.len(), 1);
    assert_eq!(facets.buckets[0].value, "revenue");
    assert_eq!(facets.buckets[0].node_idx, [0]);
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
        load_result(VaultGraph::new()),
        None,
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
    .unwrap()
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
