//! Integration tests for Inspector panel metrics.
//!
//! Follows the exact helpers from regressions.rs.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use prost::Message;
use tower::ServiceExt;

use data_loader::*;
use graph_api::proto::NodeMeta;
use graph_api::AppState;
use vault_data::{VaultEdge, VaultGraph, VaultNode};

// ── Copied from regressions.rs ───────────────────────────────────────────────

struct EmptyLoader;

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

fn trust_test_importer(importer: Box<dyn Importer>) -> HostedImporter {
    let grants = importer.descriptor().capabilities;
    HostedImporter::new(importer, grants).unwrap()
}

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

fn triangle_graph() -> VaultGraph {
    let mut g = VaultGraph::new();
    for id in ["A", "B", "C"] {
        g.add_node(VaultNode {
            id: id.to_string(),
            meta: vault_data::NodeMeta {
                title: id.to_string(),
                source_id: "test".into(),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    g.add_edge(VaultEdge::new("A", "B"));
    g.add_edge(VaultEdge::new("B", "C"));
    g.add_edge(VaultEdge::new("C", "A"));
    g
}

fn encode_id(s: &str) -> String {
    s.replace('/', "%2F")
}

fn state_with_node_metrics(id: &str, metrics: vault_data::NodeMetrics) -> AppState {
    let mut graph = triangle_graph();
    graph.nodes.get_mut(id).expect("fixture node").metrics = metrics;
    state_with_graph(graph)
}

async fn get_node_meta(state: AppState, id: &str) -> NodeMeta {
    let resp = graph_api::router(state)
        .oneshot(
            Request::builder()
                .uri(format!("/node/{}", encode_id(id)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn existing_fields_unchanged() {
    let state = state_with_graph(triangle_graph());
    let app = graph_api::router(state);
    let req = Request::builder()
        .uri(format!("/node/{}", encode_id("generate:test:A")))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let meta = NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta");
    assert_eq!(meta.id, "generate:test:A");
    assert_eq!(meta.title, "A");
    // degree=0: no layout pass runs on test graph, metrics are zero-initialized
    assert_eq!(meta.degree, 0);
    assert_eq!(meta.pagerank, 0.0);
    assert_eq!(meta.betweenness, 0.0);
    assert_eq!(meta.kcore, 0);
    assert_eq!(meta.community, 0);
    assert_eq!(meta.wcc, 0);
}

#[tokio::test]
async fn endpoint_works() {
    let state = state_with_graph(triangle_graph());
    let app = graph_api::router(state);
    let req = Request::builder()
        .uri(format!("/node/{}", encode_id("generate:test:B")))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let meta = NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta");
    assert_eq!(meta.id, "generate:test:B");
}

#[tokio::test]
async fn stub_for_missing_node() {
    let state = empty_state();
    let app = graph_api::router(state);
    let req = Request::builder()
        .uri("/node/missing-node")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let meta = NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta");
    assert_eq!(meta.doctype.as_deref(), Some("external"));
    assert_eq!(meta.tags.len(), 0);
}

// Inspector response integration: these fields are rendered by the UI from
// the server's NodeMeta payload.

#[tokio::test]
async fn energy_fields_reach_inspector_metadata() {
    let state = state_with_node_metrics(
        "A",
        vault_data::NodeMetrics {
            energy: Some(1.5),
            energy_attractive: Some(0.5),
            energy_repulsive: Some(1.0),
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:A").await;

    assert_eq!(meta.id, "generate:test:A");
    assert_eq!(meta.node_energy, Some(1.5));
    assert_eq!(meta.node_energy_attractive, Some(0.5));
    assert_eq!(meta.node_energy_repulsive, Some(1.0));
}

#[tokio::test]
async fn stability_fields_reach_inspector_metadata() {
    let state = state_with_node_metrics(
        "C",
        vault_data::NodeMetrics {
            stability_variance: Some(0.125),
            stability_drift: Some(0.25),
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:C").await;

    assert_eq!(meta.id, "generate:test:C");
    assert_eq!(meta.stability_variance, Some(0.125));
    assert_eq!(meta.stability_drift, Some(0.25));
}

#[tokio::test]
async fn anomaly_flag_reaches_inspector_metadata() {
    let state = state_with_node_metrics(
        "C",
        vault_data::NodeMetrics {
            anomaly_flag: true,
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:C").await;

    assert_eq!(meta.id, "generate:test:C");
    assert!(meta.anomaly_flag);
}

#[tokio::test]
async fn protobuf_roundtrip() {
    let meta = NodeMeta {
        id: "test:X".into(),
        title: "X".into(),
        ..Default::default()
    };
    let mut buf = Vec::new();
    meta.encode(&mut buf).expect("encode");
    let decoded = NodeMeta::decode(buf.as_ref()).expect("decode");
    assert_eq!(decoded.id, "test:X");
}

#[tokio::test]
async fn node_meta_degree_fields_reflect_stored_values() {
    let state = state_with_node_metrics(
        "A",
        vault_data::NodeMetrics {
            degree: 17,
            indegree: 7,
            outdegree: 10,
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:A").await;

    assert_eq!(meta.degree, 17);
    assert_eq!(meta.indegree, 7);
    assert_eq!(meta.outdegree, 10);
}

#[test]
fn inspector_metrics_survive_protobuf_round_trip() {
    let meta = NodeMeta {
        id: "guard-test".into(),
        node_energy: Some(1.5),
        node_energy_attractive: Some(0.5),
        node_energy_repulsive: Some(1.0),
        stability_variance: Some(0.25),
        stability_drift: Some(0.125),
        anomaly_flag: true,
        ..Default::default()
    };
    let mut buf = Vec::new();
    meta.encode(&mut buf).expect("encode");
    let decoded = NodeMeta::decode(buf.as_ref()).expect("decode");

    assert_eq!(decoded.id, "guard-test");
    assert_eq!(decoded.node_energy, Some(1.5));
    assert_eq!(decoded.node_energy_attractive, Some(0.5));
    assert_eq!(decoded.node_energy_repulsive, Some(1.0));
    assert_eq!(decoded.stability_variance, Some(0.25));
    assert_eq!(decoded.stability_drift, Some(0.125));
    assert!(decoded.anomaly_flag);
}

// ── Absent-field regressions ─────────────────────────────────────────────────
// Metrics that no layout or metadata pass produced must stay absent on the
// wire. A zero or fabricated value would show the Inspector a measurement that
// was never taken.

#[tokio::test]
async fn absent_energy_and_stability_fields_stay_none_without_layout_pass() {
    let state = state_with_graph(triangle_graph());
    let meta = get_node_meta(state, "generate:test:A").await;

    assert_eq!(meta.id, "generate:test:A");
    assert_eq!(meta.node_energy, None);
    assert_eq!(meta.node_energy_attractive, None);
    assert_eq!(meta.node_energy_repulsive, None);
    assert_eq!(meta.stability_variance, None);
    assert_eq!(meta.stability_drift, None);
    assert!(!meta.anomaly_flag);
}

#[tokio::test]
async fn node_meta_degree_is_zero_without_metrics_pass() {
    let state = state_with_graph(triangle_graph());
    let meta = get_node_meta(state, "generate:test:A").await;

    assert_eq!(meta.degree, 0);
    assert_eq!(meta.indegree, 0);
    assert_eq!(meta.outdegree, 0);
}

#[tokio::test]
async fn partial_energy_does_not_fabricate_sibling_components() {
    let state = state_with_node_metrics(
        "A",
        vault_data::NodeMetrics {
            energy: Some(2.0),
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:A").await;

    assert_eq!(meta.node_energy, Some(2.0));
    assert_eq!(meta.node_energy_attractive, None);
    assert_eq!(meta.node_energy_repulsive, None);
}

#[tokio::test]
async fn partial_stability_does_not_fabricate_drift() {
    let state = state_with_node_metrics(
        "C",
        vault_data::NodeMetrics {
            stability_variance: Some(0.5),
            ..Default::default()
        },
    );
    let meta = get_node_meta(state, "generate:test:C").await;

    assert_eq!(meta.stability_variance, Some(0.5));
    assert_eq!(meta.stability_drift, None);
}

#[tokio::test]
async fn missing_node_stub_reports_no_inspector_metrics() {
    let meta = get_node_meta(empty_state(), "missing-node").await;

    assert_eq!(meta.node_energy, None);
    assert_eq!(meta.node_energy_attractive, None);
    assert_eq!(meta.node_energy_repulsive, None);
    assert_eq!(meta.stability_variance, None);
    assert_eq!(meta.stability_drift, None);
    assert!(!meta.anomaly_flag);
    assert_eq!(meta.degree, 0);
}

#[test]
fn absent_optional_metrics_decode_as_none_not_zero() {
    let meta = NodeMeta {
        id: "absent-test".into(),
        ..Default::default()
    };
    let mut buf = Vec::new();
    meta.encode(&mut buf).expect("encode");
    let decoded = NodeMeta::decode(buf.as_ref()).expect("decode");

    assert_eq!(decoded.node_energy, None);
    assert_eq!(decoded.node_energy_attractive, None);
    assert_eq!(decoded.node_energy_repulsive, None);
    assert_eq!(decoded.stability_variance, None);
    assert_eq!(decoded.stability_drift, None);
    assert!(!decoded.anomaly_flag);
}

#[test]
fn explicit_zero_metrics_stay_present_on_wire() {
    let meta = NodeMeta {
        id: "zero-test".into(),
        node_energy: Some(0.0),
        stability_variance: Some(0.0),
        ..Default::default()
    };
    let mut buf = Vec::new();
    meta.encode(&mut buf).expect("encode");
    let decoded = NodeMeta::decode(buf.as_ref()).expect("decode");

    assert_eq!(decoded.node_energy, Some(0.0));
    assert_eq!(decoded.stability_variance, Some(0.0));
    assert_eq!(decoded.node_energy_attractive, None);
    assert_eq!(decoded.stability_drift, None);
}

#[test]
fn single_populated_metric_leaves_siblings_absent_after_encode() {
    let meta = NodeMeta {
        id: "partial-test".into(),
        node_energy: Some(1.5),
        ..Default::default()
    };
    let mut buf = Vec::new();
    meta.encode(&mut buf).expect("encode");
    let decoded = NodeMeta::decode(buf.as_ref()).expect("decode");

    assert_eq!(decoded.node_energy, Some(1.5));
    assert_eq!(decoded.node_energy_attractive, None);
    assert_eq!(decoded.node_energy_repulsive, None);
    assert_eq!(decoded.stability_variance, None);
    assert_eq!(decoded.stability_drift, None);
    assert!(!decoded.anomaly_flag);
}

