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
        kind: None,
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

/// The typed-edge discovery endpoints expose the snapshot's distinct node
/// types (from `meta.doctype`) and edge kinds with the shared revision.
#[tokio::test]
async fn typed_edge_discovery_endpoints_reflect_the_snapshot() {
    let mut graph = two_node_graph("typed");
    graph
        .nodes
        .get_mut("typed-a")
        .expect("node a")
        .meta
        .doctype = Some("element".to_string());
    graph.edges.push(vault_data::VaultEdge {
        source: "typed-a".to_string(),
        target: "typed-b".to_string(),
        kind: Some("double".to_string()),
    });
    graph.edges.push(vault_data::VaultEdge {
        source: "typed-a".to_string(),
        target: "typed-b".to_string(),
        kind: Some("single".to_string()),
    });
    let state = state_with_graph(graph);
    let revision = state.snapshot().revision;
    let app = graph_api::router(state);

    let types_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/nodes/types")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("types served");
    assert_eq!(types_resp.status(), StatusCode::OK);
    let types = json_body(types_resp).await;
    assert_eq!(types["graph_revision"], revision);
    assert_eq!(types["types"], serde_json::json!(["element"]));

    // Per-node view: "typed-a" is the typed node (index 0 into ["element"]),
    // "typed-b" is untyped.
    let types_bin_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/nodes/types.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("types.bin served");
    assert_eq!(types_bin_resp.status(), StatusCode::OK);
    let bytes = to_bytes(types_bin_resp.into_body(), 1 << 20)
        .await
        .expect("types.bin body");
    let node_indices: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(node_indices, vec![0, u32::MAX]);

    let kinds_resp = app.clone()
        .oneshot(
            Request::builder()
                .uri("/graph/edges/kinds")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("kinds served");
    assert_eq!(kinds_resp.status(), StatusCode::OK);
    let kinds = json_body(kinds_resp).await;
    assert_eq!(kinds["graph_revision"], revision);
    assert_eq!(kinds["kinds"], serde_json::json!(["double", "single"]));

    // The binary view is per-edge, aligned with `/graph/edges`: both pushed
    // edges resolve — "double" → table index 0, "single" → index 1.
    let bin_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/graph/edges/kinds.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("kinds.bin served");
    assert_eq!(bin_resp.status(), StatusCode::OK);
    let bytes = to_bytes(bin_resp.into_body(), 1 << 20)
        .await
        .expect("kinds.bin body");
    assert_eq!(bytes.len(), 12, "three u32 indices (one untyped edge)");
    let indices: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(indices, vec![u32::MAX, 0, 1]);
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
            "default-gen": { "displayName": "Default generated", "kind": "generate" },
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
        data_loader::SourceKind::Generate,
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
        data_loader::SourceKind::Generate,
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

    let alt_response = app
        .clone()
        .oneshot(source_request(
            "/graph/ids",
            "alt-vault",
            Some(SWITCH_GROUP),
        ))
        .await
        .expect("alternate ids served");
    assert_eq!(alt_response.status(), StatusCode::OK);
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
    assert_eq!(default_progress["events"].as_array().unwrap().len(), 0);

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

    for attempt in 0..2 {
        let broken = app
            .clone()
            .oneshot(source_request(
                "/graph/ids",
                "broken-okf",
                Some(SWITCH_GROUP),
            ))
            .await
            .expect("broken served");
        assert_eq!(
            broken.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "attempt {attempt}: cached build failure must surface as 503"
        );
    }

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
            "default-gen": { "displayName": "Default generated", "kind": "generate" },
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
        data_loader::SourceKind::Generate,
        group.is_some(),
    )
    .expect("catalog parses");
    catalog.load_overlay(dir).expect("overlay merges");
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

// --- static assets: compression + revalidation --------------------------------
//
// The release wasm bundle is ~12 MB and crosses a cluster proxy on every cold
// load. Shipping it raw made one long HTTP/2 stream that a keepalive-ping
// proxy reset mid-body, which the browser reports as
// `ERR_HTTP2_PING_FAILED` + `WebAssembly compilation aborted: Response body
// loading was aborted`. These tests pin the two properties that fix it.

fn state_with_assets(dir: std::path::PathBuf) -> AppState {
    AppState::new(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        trust_test_importer(Box::new(EmptyLoader)),
        load_result(VaultGraph::new()),
        Some(dir),
        graph_api::compute_broker::ComputeBroker::new(),
        Arc::new(graph_api::progress::ProgressLog::new()),
    )
    .unwrap()
}

/// A dist directory holding one compressible "wasm" payload.
fn asset_fixture(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("jump-cannon-assets-{unique}-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
    dir
}

/// A client that accepts gzip (and not brotli) gets the bundle gzipped, with
/// the headers a proxy and a browser cache both need, and it decompresses
/// back to the exact bytes on disk.
#[tokio::test]
async fn wasm_asset_is_gzipped_for_clients_that_accept_it() {
    // Repetitive but non-trivial payload: compresses well, like real wasm.
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let dir = asset_fixture("jump-cannon-ui_bg.wasm", &payload);
    let app = graph_api::router(state_with_assets(dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "gzip, deflate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");

    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(
        headers.get("content-encoding").map(|v| v.to_str().unwrap()),
        Some("gzip"),
        "a 12 MB bundle must not cross the proxy uncompressed"
    );
    assert_eq!(
        headers.get("content-type").map(|v| v.to_str().unwrap()),
        Some("application/wasm"),
        "streaming compilation needs the wasm MIME"
    );
    assert!(headers.get("etag").is_some(), "a reload needs a validator");
    assert_eq!(
        headers.get("vary").map(|v| v.to_str().unwrap()),
        Some("accept-encoding"),
        "a shared cache must not serve gzip to an identity client"
    );
    assert_eq!(
        headers.get("cache-control").map(|v| v.to_str().unwrap()),
        Some("no-cache"),
        "Trunk uses stable filenames: immutable caching would pin a stale bundle"
    );

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(
        body.len() < payload.len() / 2,
        "gzip must actually shorten the transfer: {} vs {}",
        body.len(),
        payload.len()
    );
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(std::io::Cursor::new(body.as_ref())),
        &mut decoded,
    )
    .expect("body is valid gzip");
    assert_eq!(decoded, payload, "decompressed bytes must be the bundle");
}

/// Brotli wins when the client offers it (~19% smaller than gzip on the real
/// bundle), and `br;q=0` is honoured as a refusal rather than a preference.
#[tokio::test]
async fn brotli_is_preferred_and_a_refusal_is_honoured() {
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let dir = asset_fixture("jump-cannon-ui_bg.wasm", &payload);
    let state = state_with_assets(dir);

    let response = graph_api::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "gzip, deflate, br")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    assert_eq!(
        response.headers().get("content-encoding").map(|v| v.to_str().unwrap()),
        Some("br"),
        "the smallest encoding the client accepts must win"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let mut decoded = Vec::new();
    brotli::BrotliDecompress(&mut std::io::Cursor::new(body.as_ref()), &mut decoded)
        .expect("body is valid brotli");
    assert_eq!(decoded, payload, "decompressed bytes must be the bundle");

    // A client that explicitly refuses brotli falls back to gzip, not br.
    let refused = graph_api::router(state)
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "br;q=0, gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    assert_eq!(
        refused.headers().get("content-encoding").map(|v| v.to_str().unwrap()),
        Some("gzip"),
        "br;q=0 means the client refuses brotli"
    );
}

/// A client without `Accept-Encoding` still gets the raw bytes — correctness
/// before optimisation.
#[tokio::test]
async fn asset_falls_back_to_identity_without_accept_encoding() {
    let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let dir = asset_fixture("jump-cannon-ui_bg.wasm", &payload);
    let app = graph_api::router(state_with_assets(dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("content-encoding").is_none(),
        "identity client must not receive gzip"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), payload.as_slice());
}

/// The reload path: a browser holding the current bundle re-streams nothing,
/// and a browser holding a stale one gets the new bytes.
#[tokio::test]
async fn holding_the_current_etag_revalidates_to_304() {
    let payload: Vec<u8> = (0..80_000u32).map(|i| (i % 251) as u8).collect();
    let dir = asset_fixture("jump-cannon-ui_bg.wasm", &payload);
    let state = state_with_assets(dir);

    let first = graph_api::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    let etag = first
        .headers()
        .get("etag")
        .expect("first response carries a validator")
        .to_str()
        .unwrap()
        .to_string();

    let reload = graph_api::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "gzip")
                .header("if-none-match", etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    assert_eq!(reload.status(), StatusCode::NOT_MODIFIED);
    let reload_body = to_bytes(reload.into_body(), usize::MAX).await.unwrap();
    assert!(reload_body.is_empty(), "304 must carry no body");

    let stale = graph_api::router(state)
        .oneshot(
            Request::builder()
                .uri("/jump-cannon-ui_bg.wasm")
                .header("accept-encoding", "gzip")
                .header("if-none-match", "\"0000000000000000\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    assert_eq!(
        stale.status(),
        StatusCode::OK,
        "a stale validator must re-stream the bundle"
    );
}

/// Dev-mode freshness: `trunk watch` rewrites the dist in place, so the
/// compressed copy must follow the file, not outlive it.
#[tokio::test]
async fn rebuilt_asset_invalidates_the_compressed_copy() {
    let dir = asset_fixture("jump-cannon-ui_bg.wasm", b"first build payload, repeated. ");
    let state = state_with_assets(dir.clone());

    let get = |state: AppState| async move {
        let response = graph_api::router(state)
            .oneshot(
                Request::builder()
                    .uri("/jump-cannon-ui_bg.wasm")
                    .header("accept-encoding", "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router served");
        let etag = response
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap().to_string());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(std::io::Cursor::new(body.as_ref())),
            &mut decoded,
        )
        .expect("gzip body");
        (etag, decoded)
    };

    let (first_etag, first_body) = get(state.clone()).await;
    assert_eq!(first_body, b"first build payload, repeated. ");

    // Rewrite with a different length (what a rebuild does).
    std::fs::write(
        dir.join("jump-cannon-ui_bg.wasm"),
        b"second build payload, longer than the first one. ",
    )
    .unwrap();

    let (second_etag, second_body) = get(state).await;
    assert_eq!(
        second_body, b"second build payload, longer than the first one. ",
        "a rebuilt dist must be served, not the cached copy"
    );
    assert_ne!(
        first_etag, second_etag,
        "a new bundle needs a new validator or browsers keep the old one"
    );
}
