//! HTTP-level regression tests for the axum router.
//!
//! Each test pins a server-side bug we already paid for once. They drive
//! the `Router` returned by [`graph_api::router`] via tower's `oneshot`,
//! avoiding the need for a real TCP socket / async runtime spin-up.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use prost::Message;
use tower::ServiceExt; // for `oneshot`

use data_loader::{
    Capability, ContentSchema, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect,
    HostedImporter, ImportError, ImportFuture, ImportOutcome, ImportProgress, Importer,
    ImporterDescriptor, ImporterSchema, LoadResult, Loader, SearchDocument, TagHierarchySchema,
    Transport,
};
use graph_api::importer_catalog::ImporterCatalog;
use graph_api::proto::{Init, MetaSummary, NodeMeta};
use graph_api::source_host::{SourceHost, SwitchConfig};
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

    fn import<'a>(
        &'a self,
        _progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<ImportOutcome, ImportError>> {
        Box::pin(async { Ok(ImportOutcome::Loaded(load_result(VaultGraph::new()))) })
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
    graph.add_edge(VaultEdge::new(format!("{prefix}-a"), format!("{prefix}-b")));
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

    // Without the runtime-switch gate the catalog stays read-only: the
    // mutation route refuses before it even looks at the body.
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
    assert_eq!(mutation.status(), StatusCode::FORBIDDEN);
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
        "/graph/edge-kinds",
        "/graph/edge-kinds.bin",
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
            &data_loader::NoProgress,
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

/// The edge-kind palette and per-edge index buffer are the wire contract the
/// Style panel's "Kind" edge colors decode: declared kinds lead the palette,
/// untyped edges are slot 0, and the buffer is parallel to `/graph/edges`.
#[tokio::test]
async fn edge_kinds_serve_a_palette_and_a_parallel_index_buffer() {
    let mut graph = VaultGraph::new();
    for id in ["a", "b", "c"] {
        graph.add_node(VaultNode {
            id: id.into(),
            ..Default::default()
        });
    }
    graph.add_edge(VaultEdge {
        kind: Some("reference".into()),
        ..VaultEdge::new("a", "b")
    });
    graph.add_edge(VaultEdge::new("b", "c"));
    graph.add_edge(VaultEdge {
        kind: Some("undeclared".into()),
        ..VaultEdge::new("c", "a")
    });
    let app = graph_api::router(state_with_graph(graph));

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/graph/edge-kinds").body(Body::empty()).unwrap())
        .await
        .expect("palette served");
    assert_eq!(resp.status(), StatusCode::OK);
    let palette: serde_json::Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 1 << 20).await.expect("body"))
            .expect("palette json");
    assert_eq!(palette["kinds"], serde_json::json!(["reference", "undeclared"]));

    let resp = app
        .oneshot(Request::builder().uri("/graph/edge-kinds.bin").body(Body::empty()).unwrap())
        .await
        .expect("index buffer served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let slots: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(slots, [1, 0, 2]);
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

// ── Runtime per-viewer importer source switching ────────────────────────────
//
// The `x-jump-cannon-source` header selects a catalog source per request.
// Selecting a non-default source requires runtime switching to be enabled
// (`SwitchConfig` with a required group) AND the caller's groups header to
// contain that group. Writes/compute stay on the deployment default.

const SWITCH_GROUP: &str = "kubernetes-clients";
const GROUPS_HEADER: &str = "x-netbird-groups";
const SOURCE_HEADER: &str = "x-jump-cannon-source";

/// A tiny on-disk Obsidian vault fixture: one note.
fn fixture_vault(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("jump-cannon-switch-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fixture vault dir");
    std::fs::write(
        dir.join("alt-note.md"),
        "# Alt Note\n\nalternate vault body\n",
    )
    .expect("fixture note");
    dir
}

/// Catalog with the fixture vault as a runnable alternate, a non-runnable
/// GitHub entry, and an OKF entry whose root does not exist (build failure).
fn switch_catalog_json(vault: &std::path::Path) -> String {
    serde_json::json!({
        "selected": "default-gen",
        "sources": {
            "default-gen": { "displayName": "Default generated", "kind": "pest" },
            "alt-vault": {
                "displayName": "Alt vault",
                "kind": "obsidian",
                "source": {
                    "volumeName": "alt-vault",
                    "existingClaim": "alt-vault",
                    "mountPath": vault.to_str().unwrap(),
                    "path": vault.to_str().unwrap(),
                    "readOnly": false
                }
            },
            "gh": { "displayName": "GH", "kind": "github", "sourceId": "gh" },
            "broken-okf": {
                "displayName": "Broken OKF",
                "kind": "okf",
                "sourceId": "broken",
                "source": {
                    "volumeName": "broken-okf",
                    "existingClaim": "broken-okf",
                    "mountPath": "/nonexistent-jump-cannon-test",
                    "path": "/nonexistent-jump-cannon-test/okf",
                    "readOnly": true
                }
            }
        }
    })
    .to_string()
}

fn switch_host(catalog_raw: &str, group: Option<&str>) -> SourceHost {
    let catalog = ImporterCatalog::parse_with_runtime_switch(
        Some(catalog_raw),
        data_loader::SourceKind::Pest,
        group.is_some(),
    )
    .expect("switch catalog parses");
    SourceHost::new(
        state_with_catalog(catalog),
        SwitchConfig::new(group.map(str::to_owned), GROUPS_HEADER),
    )
}

fn source_request(path: &str, source: &str, groups: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(path).header(SOURCE_HEADER, source);
    if let Some(groups) = groups {
        builder = builder.header(GROUPS_HEADER, groups);
    }
    builder.body(Body::empty()).unwrap()
}

fn source_status_request(id: &str, groups: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(format!("/importers/sources/{id}/status"));
    if let Some(groups) = groups {
        builder = builder.header(GROUPS_HEADER, groups);
    }
    builder.body(Body::empty()).unwrap()
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("response body"),
    )
    .expect("response JSON")
}

/// Gate closed (no required group configured): the selection header is ignored
/// and every request serves the deployment default — today's exact behavior.
#[tokio::test]
async fn runtime_switch_disabled_ignores_selection_header() {
    let vault = fixture_vault("disabled");
    let catalog = ImporterCatalog::parse(
        Some(&switch_catalog_json(&vault)),
        data_loader::SourceKind::Pest,
    )
    .expect("strict catalog parse");
    let app = graph_api::router_with_host(SourceHost::default_only(state_with_catalog(catalog)));

    let response = app
        .clone()
        .oneshot(source_request("/graph/ids", "alt-vault", None))
        .await
        .expect("ids served");
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<String> = serde_json::from_slice(
        &to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("ids body"),
    )
    .expect("ids JSON");
    assert!(
        ids.is_empty(),
        "header ignored: the default graph is served"
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/importers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("catalog served");
    let body = json_body(response).await;
    assert_eq!(body["runtimeSwitch"]["enabled"], false);
    assert_eq!(body["runtimeSwitch"]["allowed"], false);
    assert!(body["runtimeSwitch"]["requiredGroup"].is_null());
    let alt = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "alt-vault")
        .expect("alt-vault entry");
    assert_eq!(alt["runnable"], true);
    let gh = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "gh")
        .expect("gh entry");
    assert_eq!(gh["runnable"], false);
}

/// Gate open: selecting a non-default source without the required group (or
/// with the wrong one) is 403. The deployment default itself never requires
/// authorization, even when named explicitly.
#[tokio::test]
async fn runtime_switch_forbids_alternate_without_group_membership() {
    let vault = fixture_vault("forbidden");
    let host = switch_host(&switch_catalog_json(&vault), Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    for groups in [None, Some("someone-else")] {
        let response = app
            .clone()
            .oneshot(source_request("/graph/ids", "alt-vault", groups))
            .await
            .expect("ids served");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "groups header {groups:?} must be rejected"
        );
    }

    // Explicitly selecting the deployment default needs no group.
    let response = app
        .oneshot(source_request("/graph/ids", "default-gen", None))
        .await
        .expect("ids served");
    assert_eq!(response.status(), StatusCode::OK);
}

/// First authorized request builds the alternate lazily; its graph, revision,
/// and progress log are independent of the default's.
#[tokio::test]
async fn runtime_switch_builds_alternate_lazily_and_isolates_state() {
    let vault = fixture_vault("lazy");
    let host = switch_host(&switch_catalog_json(&vault), Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let default_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/ids")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("default ids served");
    assert_eq!(default_response.status(), StatusCode::OK);
    let default_revision = response_revision(&default_response);
    let default_ids: Vec<String> = serde_json::from_slice(
        &to_bytes(default_response.into_body(), 1 << 20)
            .await
            .expect("default ids body"),
    )
    .expect("default ids JSON");
    assert!(default_ids.is_empty());

    // The first authorized request spawns the build and answers 202 while it
    // runs — it never awaits the import.
    let building = app
        .clone()
        .oneshot(source_request(
            "/graph/ids",
            "alt-vault",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("alternate ids served");
    assert_eq!(
        building.status(),
        StatusCode::ACCEPTED,
        "the first request reports building, not the built graph"
    );

    // Poll until the background build completes and the alternate serves.
    let mut alt_response = None;
    for _ in 0..300 {
        let resp = app
            .clone()
            .oneshot(source_request(
                "/graph/ids",
                "alt-vault",
                Some(SWITCH_GROUP),
            ))
            .await
            .expect("alternate ids served");
        match resp.status() {
            StatusCode::OK => {
                alt_response = Some(resp);
                break;
            }
            StatusCode::ACCEPTED => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("unexpected status while building alternate: {other}"),
        }
    }
    let alt_response = alt_response.expect("alternate finishes building and serves");
    let alt_revision = response_revision(&alt_response);
    assert_ne!(alt_revision, default_revision);
    let alt_ids: Vec<String> = serde_json::from_slice(
        &to_bytes(alt_response.into_body(), 1 << 20)
            .await
            .expect("alternate ids body"),
    )
    .expect("alternate ids JSON");
    assert_eq!(alt_ids.len(), 1, "fixture vault has exactly one note");
    assert!(
        alt_ids[0].contains("alt-note"),
        "alternate id names the fixture note: {alt_ids:?}"
    );

    // A second request hits the cached serving state (same revision).
    let second = app
        .clone()
        .oneshot(source_request(
            "/graph/ids",
            "alt-vault",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("cached alternate served");
    assert_eq!(response_revision(&second), alt_revision);

    // Progress logs are per-source: the alternate's build emitted events,
    // the default's (constructed without a load) is empty.
    let default_progress = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/progress")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("default progress served");
    let default_progress = json_body(default_progress).await;
    // Boot import now reports granular snapshot-build stages; the
    // default log is expected to carry those events.
    assert!(default_progress["events"].as_array().unwrap().len() > 0);

    let alt_progress = app
        .clone()
        .oneshot(source_request("/progress", "alt-vault", Some(SWITCH_GROUP)))
        .await
        .expect("alternate progress served");
    let alt_progress = json_body(alt_progress).await;
    assert!(
        !alt_progress["events"].as_array().unwrap().is_empty(),
        "alternate build flows through its own progress log"
    );

    // /importers computes runtimeSwitch.allowed per request from the
    // caller's groups header.
    let allowed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/importers")
                .header(GROUPS_HEADER, SWITCH_GROUP)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("catalog served");
    let allowed = json_body(allowed).await;
    assert_eq!(allowed["runtimeSwitch"]["enabled"], true);
    assert_eq!(allowed["runtimeSwitch"]["allowed"], true);
    assert_eq!(allowed["runtimeSwitch"]["requiredGroup"], SWITCH_GROUP);
    // The response still describes the deployment default.
    assert_eq!(allowed["selected"], "default-gen");

    let denied = app
        .oneshot(
            Request::builder()
                .uri("/importers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("catalog served");
    let denied = json_body(denied).await;
    assert_eq!(denied["runtimeSwitch"]["enabled"], true);
    assert_eq!(denied["runtimeSwitch"]["allowed"], false);

    let _ = std::fs::remove_dir_all(&vault);
}

/// Writes, generation, and compute endpoints stay on the deployment default:
/// selecting an alternate is rejected with 400 even for authorized callers.
#[tokio::test]
async fn runtime_switch_rejects_writes_and_compute_on_alternates() {
    let vault = fixture_vault("writes");
    let host = switch_host(&switch_catalog_json(&vault), Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/vault/page")
                .header("content-type", "application/json")
                .header(SOURCE_HEADER, "alt-vault")
                .header(GROUPS_HEADER, SWITCH_GROUP)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"path": "x", "body": "y"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .expect("put served");
    assert_eq!(put.status(), StatusCode::BAD_REQUEST);

    let generate = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/generate")
                .header("content-type", "application/json")
                .header(SOURCE_HEADER, "alt-vault")
                .header(GROUPS_HEADER, SWITCH_GROUP)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"expr": "{ nodes = []; links = []; }"}))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .expect("generate served");
    assert_eq!(generate.status(), StatusCode::BAD_REQUEST);

    let compute = app
        .oneshot(source_request(
            "/compute/health",
            "alt-vault",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("compute health served");
    assert_eq!(compute.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&vault);
}

/// Error contract: unknown source id → 404; known but not runnable → 400;
/// a cached build failure → 503 (replayed, not rebuilt per request).
#[tokio::test]
async fn runtime_switch_error_contract() {
    let vault = fixture_vault("errors");
    let host = switch_host(&switch_catalog_json(&vault), Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let unknown = app
        .clone()
        .oneshot(source_request(
            "/graph/ids",
            "no-such-source",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("unknown served");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let not_runnable = app
        .clone()
        .oneshot(source_request("/graph/ids", "gh", Some(SWITCH_GROUP)))
        .await
        .expect("not-runnable served");
    assert_eq!(not_runnable.status(), StatusCode::BAD_REQUEST);

    // The OKF build (nonexistent root) fails asynchronously: the first request
    // reports 202 building, then the cached failure surfaces as 503 JSON.
    let mut failed = false;
    for _ in 0..300 {
        let broken = app
            .clone()
            .oneshot(source_request(
                "/graph/ids",
                "broken-okf",
                Some(SWITCH_GROUP),
            ))
            .await
            .expect("broken served");
        match broken.status() {
            StatusCode::SERVICE_UNAVAILABLE => {
                let body = json_body(broken).await;
                assert_eq!(body["status"], "failed");
                assert_eq!(body["source"], "broken-okf");
                assert!(
                    body["error"].is_string(),
                    "the 503 body carries the failure message"
                );
                failed = true;
                break;
            }
            StatusCode::ACCEPTED => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("unexpected status for a failing build: {other}"),
        }
    }
    assert!(failed, "the broken OKF build eventually caches as 503");

    // The cached failure is replayed, not rebuilt, on the next request.
    let again = app
        .oneshot(source_request(
            "/graph/ids",
            "broken-okf",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("broken served again");
    assert_eq!(again.status(), StatusCode::SERVICE_UNAVAILABLE);

    let _ = std::fs::remove_dir_all(&vault);
}

/// The `/importers/sources/:id/status` route is mounted and reports the
/// alternate's lifecycle without ever blocking or 503-ing: idle before a
/// build, 404 for an unknown id, 403 without the group, then building→serving.
#[tokio::test]
async fn source_status_route_reports_lifecycle() {
    let vault = fixture_vault("status-route");
    let host = switch_host(&switch_catalog_json(&vault), Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let idle = app
        .clone()
        .oneshot(source_status_request("alt-vault", Some(SWITCH_GROUP)))
        .await
        .expect("status served");
    assert_eq!(idle.status(), StatusCode::OK);
    let idle = json_body(idle).await;
    assert_eq!(idle["status"], "idle");
    assert_eq!(idle["source"], "alt-vault");

    let unknown = app
        .clone()
        .oneshot(source_status_request("no-such-source", Some(SWITCH_GROUP)))
        .await
        .expect("status served");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let forbidden = app
        .clone()
        .oneshot(source_status_request("alt-vault", None))
        .await
        .expect("status served");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // Kick off the build through a graph route, then poll the status route.
    let kick = app
        .clone()
        .oneshot(source_request("/graph/ids", "alt-vault", Some(SWITCH_GROUP)))
        .await
        .expect("kick served");
    assert_eq!(kick.status(), StatusCode::ACCEPTED);

    let mut serving = false;
    for _ in 0..300 {
        let status = app
            .clone()
            .oneshot(source_status_request("alt-vault", Some(SWITCH_GROUP)))
            .await
            .expect("status served");
        assert_eq!(status.status(), StatusCode::OK, "status never 503s");
        let body = json_body(status).await;
        match body["status"].as_str().expect("status string") {
            "serving" => {
                serving = true;
                break;
            }
            "building" => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(serving, "the alternate reaches serving through the status route");

    let _ = std::fs::remove_dir_all(&vault);
}

// ── Importer package definitions (GET/PUT /importers/:id/definition, POST /importers) ──

const TOML_PACKAGE: &str =
    include_str!("../../../charts/jump-cannon/packages/hindsight-memory-bank.toml");

/// A packages dir holding the shipped package under a catalog-declared
/// filename, plus a catalog binding it as a runnable httpjson source.
fn packages_fixture(tag: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("jump-cannon-pkgs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("packages dir");
    std::fs::write(dir.join("hindsight.toml"), TOML_PACKAGE).expect("package file");
    let catalog = serde_json::json!({
        "selected": "default-gen",
        "sources": {
            "default-gen": { "displayName": "Default generated", "kind": "pest" },
            "hindsight": {
                "displayName": "Hindsight",
                "kind": "httpjson",
                "httpJson": {
                    "package": "hindsight.toml",
                    "endpoint": "http://hindsight.invalid",
                    "variables": { "bank": "omp" }
                }
            }
        }
    })
    .to_string();
    (dir, catalog)
}

fn packages_host(dir: &std::path::Path, catalog_raw: &str, group: Option<&str>) -> SourceHost {
    let mut catalog = ImporterCatalog::parse_with_runtime_switch(
        Some(catalog_raw),
        data_loader::SourceKind::Pest,
        group.is_some(),
    )
    .expect("catalog parses");
    catalog.load_overlay(dir).expect("overlay merges");
    catalog
        .load_variables_overlay(dir)
        .expect("variables overlay applies");
    SourceHost::with_packages_dir(
        state_with_catalog(catalog),
        SwitchConfig::new(group.map(str::to_owned), GROUPS_HEADER),
        Some(dir.to_path_buf()),
    )
}

fn json_request(method: &str, path: &str, body: serde_json::Value, groups: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(groups) = groups {
        builder = builder.header(GROUPS_HEADER, groups);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn text_body(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body")
            .to_vec(),
    )
    .expect("utf-8 body")
}

/// GET exposes the authored package source with the read posture of the
/// catalog (no group needed); PUT carries the switch authorization, validates
/// before writing, and replaces the file atomically.
#[tokio::test]
async fn importer_definition_get_and_put_contract() {
    let (dir, catalog) = packages_fixture("definition");
    let app = graph_api::router_with_host(packages_host(&dir, &catalog, Some(SWITCH_GROUP)));

    let response = app
        .clone()
        .oneshot(Request::builder().uri("/importers/hindsight/definition").body(Body::empty()).unwrap())
        .await
        .expect("definition served");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["package"], "hindsight.toml");
    assert_eq!(body["writable"], true);
    assert_eq!(body["source"].as_str().unwrap(), TOML_PACKAGE);

    let unknown = app
        .clone()
        .oneshot(Request::builder().uri("/importers/nope/definition").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let not_package = app
        .clone()
        .oneshot(Request::builder().uri("/importers/default-gen/definition").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(not_package.status(), StatusCode::BAD_REQUEST);

    let forbidden = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/definition",
            serde_json::json!({ "source": TOML_PACKAGE }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let invalid = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/definition",
            serde_json::json!({ "source": "format_version = 1\n" }),
            Some(SWITCH_GROUP),
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(text_body(invalid).await.contains("format_version 3"));
    assert_eq!(
        std::fs::read_to_string(dir.join("hindsight.toml")).unwrap(),
        TOML_PACKAGE,
        "a rejected PUT never touches the file"
    );

    let edited = TOML_PACKAGE.replace(
        "name = \"Hindsight memory bank\"",
        "name = \"Edited bank\"",
    );
    let saved = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/definition",
            serde_json::json!({ "source": edited }),
            Some(SWITCH_GROUP),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = json_body(saved).await;
    assert_eq!(body["writable"], true);
    assert_eq!(std::fs::read_to_string(dir.join("hindsight.toml")).unwrap(), edited);
    assert!(
        std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry.unwrap().file_name().to_string_lossy().contains(".tmp-")
        }),
        "atomic write leaves no temp file behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// POST validates, writes the package file, appends the overlay catalog, and
/// publishes the source (origin `runtime`) — which a fresh boot re-merges.
#[tokio::test]
async fn importers_post_adds_runtime_source_and_persists_overlay() {
    let (dir, catalog) = packages_fixture("post");
    let app = graph_api::router_with_host(packages_host(&dir, &catalog, Some(SWITCH_GROUP)));
    let body = serde_json::json!({
        "id": "local-bank",
        "name": "Local bank",
        "description": "added at runtime",
        "package": "local-bank.toml",
        "source": TOML_PACKAGE,
        "endpoint": "http://hindsight.invalid",
        "variables": { "bank": "local" },
        "pollIntervalMs": 30000
    });

    let forbidden = app
        .clone()
        .oneshot(json_request("POST", "/importers", body.clone(), None))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let mut bad_id = body.clone();
    bad_id["id"] = serde_json::json!("Bad Id");
    let rejected = app
        .clone()
        .oneshot(json_request("POST", "/importers", bad_id, Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let mut escaping = body.clone();
    escaping["package"] = serde_json::json!("../escape.toml");
    let rejected = app
        .clone()
        .oneshot(json_request("POST", "/importers", escaping, Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let mut bad_source = body.clone();
    bad_source["source"] = serde_json::json!("format_version = 3\nnot-a-package = true\n");
    let rejected = app
        .clone()
        .oneshot(json_request("POST", "/importers", bad_source, Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(!dir.join("local-bank.toml").exists(), "rejected POST writes nothing");

    let created = app
        .clone()
        .oneshot(json_request("POST", "/importers", body.clone(), Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let item = json_body(created).await;
    assert_eq!(item["id"], "local-bank");
    assert_eq!(item["origin"], "runtime");
    assert_eq!(item["runnable"], true);
    assert_eq!(item["httpJson"]["package"], "local-bank.toml");
    assert_eq!(item["httpJson"]["pollIntervalMs"], 30000);
    assert_eq!(std::fs::read_to_string(dir.join("local-bank.toml")).unwrap(), TOML_PACKAGE);
    let overlay: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("catalog.local.json")).unwrap())
            .unwrap();
    assert_eq!(overlay["sources"]["local-bank"]["httpJson"]["variables"]["bank"], "local");

    let duplicate = app
        .clone()
        .oneshot(json_request("POST", "/importers", body.clone(), Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let catalog_now = app
        .clone()
        .oneshot(Request::builder().uri("/importers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed = json_body(catalog_now).await;
    let sources = listed["sources"].as_array().unwrap();
    let local = sources.iter().find(|s| s["id"] == "local-bank").expect("runtime source listed");
    assert_eq!(local["origin"], "runtime");
    assert!(sources.iter().filter(|s| s["id"] != "local-bank").all(|s| s["origin"] == "deployment"));

    // The new source's definition is served and editable like a chart one.
    let definition = app
        .clone()
        .oneshot(Request::builder().uri("/importers/local-bank/definition").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(definition.status(), StatusCode::OK);

    // A fresh host over the same dir re-merges the overlay at boot.
    let rebooted = graph_api::router_with_host(packages_host(&dir, &catalog, Some(SWITCH_GROUP)));
    let listed = json_body(
        rebooted
            .oneshot(Request::builder().uri("/importers").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(listed["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == "local-bank" && s["origin"] == "runtime"));

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Parameterised sources: coverage validation + live discovery ──────────────

/// A packages dir holding the shipped hindsight package plus a catalog binding
/// it as `hindsight` with the given `httpJson`/`parameters` JSON fragments.
fn param_packages_fixture(tag: &str, hindsight: serde_json::Value) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("jump-cannon-pkgs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("packages dir");
    std::fs::write(dir.join("hindsight.toml"), TOML_PACKAGE).expect("package file");
    let mut source = serde_json::json!({
        "displayName": "Hindsight",
        "kind": "httpjson",
    });
    let object = source.as_object_mut().unwrap();
    for (key, value) in hindsight.as_object().unwrap() {
        object.insert(key.clone(), value.clone());
    }
    let catalog = serde_json::json!({
        "selected": "default-gen",
        "sources": {
            "default-gen": { "displayName": "Default", "kind": "pest" },
            "hindsight": source,
        }
    })
    .to_string();
    (dir, catalog)
}

/// (c) Selecting an httpjson source whose package requires `bank` (no default)
/// but neither binds it in `variables` nor declares it as a parameter is a 400
/// naming the uncovered variable — a misconfigured catalog fails loudly rather
/// than as an opaque build failure.
#[tokio::test]
async fn selecting_source_missing_required_package_variable_is_bad_request() {
    let (dir, catalog) = param_packages_fixture(
        "coverage",
        serde_json::json!({
            "httpJson": {
                "package": "hindsight.toml",
                "endpoint": "http://hindsight.invalid",
                "variables": { "tenant": "default" }
            }
        }),
    );
    let host = packages_host(&dir, &catalog, Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let response = app
        .oneshot(source_request("/graph/ids", "hindsight", Some(SWITCH_GROUP)))
        .await
        .expect("served");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a required package variable that is neither bound nor parameterised is a 400"
    );
    let body = text_body(response).await;
    assert!(
        body.contains("bank"),
        "the error names the uncovered variable: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Spawn an in-process HTTP server that answers `GET /v1/default/banks` with the
/// given status and body, returning its base URL and the serving task.
async fn spawn_banks_server(
    status: StatusCode,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock banks server");
    let addr = listener.local_addr().expect("mock addr");
    let app = axum::Router::new().route(
        "/v1/default/banks",
        axum::routing::get(move || async move { (status, body) }),
    );
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

fn parameters_request(id: &str, groups: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(format!("/importers/sources/{id}/parameters"));
    if let Some(groups) = groups {
        builder = builder.header(GROUPS_HEADER, groups);
    }
    builder.body(Body::empty()).unwrap()
}

/// (d) `/parameters` runs live discovery against the bound API: a healthy
/// endpoint yields the discovered ids and labels; a 500 falls back to the
/// static `values` with the error surfaced.
#[tokio::test]
async fn parameters_route_discovers_values_and_falls_back_on_error() {
    // Healthy discovery endpoint.
    let (url, server) = spawn_banks_server(
        StatusCode::OK,
        r#"{"banks":[{"bank_id":"omp","name":"OMP"},{"bank_id":"jira-ithelp","name":"Jira ITHELP"}]}"#,
    )
    .await;
    let (dir, catalog) = param_packages_fixture(
        "discover-ok",
        serde_json::json!({
            "httpJson": {
                "package": "hindsight.toml",
                "endpoint": url,
                "variables": {}
            },
            "parameters": {
                "bank": {
                    "label": "Memory bank",
                    "discover": {
                        "path": "/v1/{tenant}/banks",
                        "itemsPointer": "/banks",
                        "idPointer": "/bank_id",
                        "labelPointer": "/name"
                    },
                    "values": ["omp"]
                }
            }
        }),
    );
    let host = packages_host(&dir, &catalog, Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let response = app
        .oneshot(parameters_request("hindsight", Some(SWITCH_GROUP)))
        .await
        .expect("parameters served");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["source"], "hindsight");
    let bank = &body["parameters"]["bank"];
    assert_eq!(bank["label"], "Memory bank");
    assert_eq!(bank["discovered"], true);
    assert!(bank["error"].is_null(), "no error on a healthy discovery");
    let values = bank["values"].as_array().expect("values array");
    assert!(
        values
            .iter()
            .any(|value| value["id"] == "omp" && value["label"] == "OMP"),
        "discovered omp with its label: {values:?}"
    );
    assert!(
        values
            .iter()
            .any(|value| value["id"] == "jira-ithelp" && value["label"] == "Jira ITHELP"),
        "discovered jira-ithelp with its label: {values:?}"
    );
    server.abort();
    let _ = std::fs::remove_dir_all(&dir);

    // Discovery endpoint returning 500: fall back to the static values, with the
    // error surfaced.
    let (url, server) = spawn_banks_server(StatusCode::INTERNAL_SERVER_ERROR, "boom").await;
    let (dir, catalog) = param_packages_fixture(
        "discover-500",
        serde_json::json!({
            "httpJson": {
                "package": "hindsight.toml",
                "endpoint": url,
                "variables": {}
            },
            "parameters": {
                "bank": {
                    "label": "Memory bank",
                    "discover": {
                        "path": "/v1/{tenant}/banks",
                        "itemsPointer": "/banks",
                        "idPointer": "/bank_id",
                        "labelPointer": "/name"
                    },
                    "values": ["fallback-a", "fallback-b"]
                }
            }
        }),
    );
    let host = packages_host(&dir, &catalog, Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host);

    let response = app
        .oneshot(parameters_request("hindsight", Some(SWITCH_GROUP)))
        .await
        .expect("parameters served");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let bank = &body["parameters"]["bank"];
    assert_eq!(bank["discovered"], false, "discovery failed");
    assert!(bank["error"].is_string(), "the discovery error is surfaced");
    let values = bank["values"].as_array().expect("values array");
    let ids: Vec<&str> = values.iter().map(|value| value["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["fallback-a", "fallback-b"],
        "falls back to the static values"
    );
    server.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Control-surface parity: the package's declared `[[parser.variables]]` are
/// the authoritative parameter list. A source whose catalog declares NO
/// `parameters` map still gets a picker per package variable (label defaults
/// to the variable name, default and description from the package), and a
/// selection may pick any package variable — not just catalog-declared
/// parameters.
#[tokio::test]
async fn parameters_route_enumerates_package_variables_without_catalog_parameters() {
    let (dir, catalog) = param_packages_fixture(
        "parity",
        serde_json::json!({
            "httpJson": {
                "package": "hindsight.toml",
                "endpoint": "http://hindsight.invalid",
                "variables": { "tenant": "default" }
            }
        }),
    );
    let host = packages_host(&dir, &catalog, Some(SWITCH_GROUP));
    let app = graph_api::router_with_host(host.clone());

    let response = app
        .oneshot(parameters_request("hindsight", Some(SWITCH_GROUP)))
        .await
        .expect("parameters served");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let tenant = &body["parameters"]["tenant"];
    assert!(tenant.is_object(), "tenant is a picker: {body}");
    assert_eq!(tenant["label"], "tenant", "no catalog label: the variable name");
    assert_eq!(tenant["default"], "default", "package default pre-selects");
    assert!(
        tenant["description"].as_str().unwrap_or("").contains("Tenant"),
        "the package description rides along: {tenant}"
    );
    let bank = &body["parameters"]["bank"];
    assert!(bank.is_object(), "bank is a picker: {body}");
    assert!(bank["default"].is_null(), "bank is required: no default");
    assert_eq!(
        body["parameters"].as_object().unwrap().len(),
        2,
        "exactly the package's declared variables: {body}"
    );

    // A selection picking `bank` — a package variable, not a catalog
    // parameter — is accepted and spawns the build (202).
    let app = graph_api::router_with_host(host.clone());
    let response = app
        .oneshot(source_request("/graph/ids", "hindsight?bank=omp", Some(SWITCH_GROUP)))
        .await
        .expect("served");
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "picking any package variable is a valid selection"
    );


    // A selection naming a variable the package does not declare is a 400
    // naming the declared variables — the package-aware coverage check is
    // the gate the sync layer defers to. (`bank` rides along so the
    // required-variable check passes and the name check is what fires.)
    let response = graph_api::router_with_host(host)
        .oneshot(source_request("/graph/ids", "hindsight?bank=omp&quer=x", Some(SWITCH_GROUP)))
        .await
        .expect("served");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = text_body(response).await;
    assert!(body.contains("quer"), "the error names the bogus variable: {body}");
    assert!(body.contains("bank"), "the error lists declared variables: {body}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// GET/PUT /importers/:id/variables: the declared/current contract with the
/// catalog's read posture, switch-gated mutation, key validation against the
/// package's declared variables, persistence to `variables.local.json`, the
/// in-memory catalog update, and invalidation of the running alternate.
#[tokio::test]
async fn importer_variables_get_and_put_contract() {
    let (dir, catalog) = packages_fixture("variables");
    let app = graph_api::router_with_host(packages_host(&dir, &catalog, Some(SWITCH_GROUP)));

    // GET: declared `[[parser.variables]]` of the bound package (tenant has
    // a default, bank is required) and the catalog entry's current values.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/importers/hindsight/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let declared = body["declared"].as_array().unwrap();
    assert_eq!(declared.len(), 2, "{declared:?}");
    assert_eq!(declared[0]["name"], "tenant");
    assert_eq!(declared[0]["default"], "default");
    assert_eq!(
        declared[0]["description"],
        "Tenant path segment: /v1/{tenant}/banks/..."
    );
    assert_eq!(declared[1]["name"], "bank");
    assert_eq!(declared[1]["default"], serde_json::Value::Null);
    assert_eq!(body["current"]["bank"], "omp");
    assert!(
        body["current"].get("tenant").is_none(),
        "unset variables are simply absent from current"
    );

    // Unknown id → 404; non-httpjson binding → 400.
    let unknown = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/importers/nope/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let not_package = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/importers/default-gen/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_package.status(), StatusCode::BAD_REQUEST);

    // PUT without the switch group → 403, before any body is even parsed.
    let forbidden = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/variables",
            serde_json::json!({ "variables": { "bank": "relay" } }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // PUT naming an undeclared variable → 400 naming the offender, and
    // nothing is written.
    let rejected = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/variables",
            serde_json::json!({ "variables": { "query": "openalex" } }),
            Some(SWITCH_GROUP),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(
        text_body(rejected).await.contains("\"query\""),
        "the 400 must name the offending key"
    );
    assert!(!dir.join("variables.local.json").exists());

    // Selecting the source spawns a lazy build and answers 202 while it runs
    // (the fixture endpoint is unreachable, so it settles as a cached failure
    // in the background) — either way an alternate entry now exists to
    // invalidate.
    let first = app
        .clone()
        .oneshot(source_request("/graph/ids", "hindsight", Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);

    // Valid PUT → 200 with the post-write effective state, persisted to
    // variables.local.json, and published to the in-memory catalog.
    let saved = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/variables",
            serde_json::json!({ "variables": { "tenant": "default", "bank": "relay" } }),
            Some(SWITCH_GROUP),
        ))
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let body = json_body(saved).await;
    assert_eq!(body["declared"].as_array().unwrap().len(), 2);
    assert_eq!(body["current"]["bank"], "relay");
    assert_eq!(body["current"]["tenant"], "default");

    let overlay: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("variables.local.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(overlay["sources"]["hindsight"]["variables"]["bank"], "relay");
    assert_eq!(
        overlay["sources"]["hindsight"]["variables"]["tenant"],
        "default"
    );

    let listed = json_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/importers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let hindsight = listed["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "hindsight")
        .expect("hindsight listed");
    assert_eq!(hindsight["httpJson"]["variables"]["bank"], "relay");

    // The alternate was invalidated: the next selection starts a fresh
    // build (202 with a live building status) instead of replaying the
    // pre-PUT cached outcome.
    let rebuilt = app
        .clone()
        .oneshot(source_request("/graph/ids", "hindsight", Some(SWITCH_GROUP)))
        .await
        .unwrap();
    assert_eq!(rebuilt.status(), StatusCode::ACCEPTED);
    let rebuilt_body = json_body(rebuilt).await;
    assert_eq!(
        rebuilt_body["status"], "building",
        "fresh build after invalidation: {rebuilt_body}"
    );

    // GET reflects the replaced set; full replacement drops a key cleanly.
    let replaced = app
        .clone()
        .oneshot(json_request(
            "PUT",
            "/importers/hindsight/variables",
            serde_json::json!({ "variables": { "bank": "solo" } }),
            Some(SWITCH_GROUP),
        ))
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::OK);
    let body = json_body(replaced).await;
    assert_eq!(body["current"]["bank"], "solo");
    assert!(
        body["current"].get("tenant").is_none(),
        "full replacement drops unset variables: {}",
        body["current"]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Boot applies `variables.local.json` onto matching catalog entries
/// (unknown ids are skipped, never fatal).
#[tokio::test]
async fn variables_overlay_applies_at_boot() {
    let (dir, catalog) = packages_fixture("vars-boot");
    std::fs::write(
        dir.join("variables.local.json"),
        r#"{ "sources": {
            "hindsight": { "variables": { "tenant": "default", "bank": "boot-bank" } },
            "removed-source": { "variables": { "bank": "gone" } }
        } }"#,
    )
    .unwrap();
    let app = graph_api::router_with_host(packages_host(&dir, &catalog, Some(SWITCH_GROUP)));

    let listed = json_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/importers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let hindsight = listed["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "hindsight")
        .expect("hindsight listed");
    assert_eq!(hindsight["httpJson"]["variables"]["bank"], "boot-bank");

    // GET reports the boot-applied value as the current one.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/importers/hindsight/variables")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["current"]["bank"], "boot-bank");
    let _ = std::fs::remove_dir_all(&dir);
}
