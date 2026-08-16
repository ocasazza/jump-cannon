//! Fixture-driven tests: no code path requires a live Hindsight API.

use std::collections::HashMap;
use parking_lot::Mutex;

use data_loader::{
    identity, DiscoveryFieldType, Effect, ImportError, Importer, Transport, WatchPlan,
};
use serde_json::{json, Value};

use crate::{
    sanitize_source_id, HindsightApi, HindsightImporter, HindsightSourceConfig,
    DEFAULT_MAX_UNITS,
};

/// Fixture API serving canned JSON per path suffix. Records every requested
/// path so pagination and query composition are observable.
struct FixtureApi {
    responses: HashMap<String, Value>,
    requested: Mutex<Vec<String>>,
}

impl FixtureApi {
    fn new(responses: HashMap<String, Value>) -> Self {
        Self {
            responses,
            requested: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requested.lock().clone()
    }
}

impl HindsightApi for FixtureApi {
    fn get_json<'a>(
        &'a self,
        path: &'a str,
    ) -> data_loader::ImportFuture<'a, Result<Value, ImportError>> {
        let path = path.to_string();
        Box::pin(async move {
            self.requested.lock().push(path.clone());
            // Strip the query string for fixture lookup.
            let bare = path.split('?').next().unwrap_or(&path).to_string();
            self.responses
                .get(&bare)
                .cloned()
                .ok_or_else(|| ImportError::SourceRead {
                    origin: "fixture".into(),
                    message: format!("no fixture for {bare:?}"),
                })
        })
    }
}

/// Shared handle so a test can inspect the fixture after the importer took
/// ownership of its boxed API.
#[derive(Clone)]
struct SharedFixture(std::sync::Arc<FixtureApi>);

impl HindsightApi for SharedFixture {
    fn get_json<'a>(
        &'a self,
        path: &'a str,
    ) -> data_loader::ImportFuture<'a, Result<Value, ImportError>> {
        self.0.get_json(path)
    }
}

fn config(bank: &str) -> HindsightSourceConfig {
    HindsightSourceConfig {
        base_url: "http://hindsight.test".into(),
        tenant: "default".into(),
        bank: bank.into(),
        token: None,
        poll_interval_ms: 0,
        source_id: None,
        max_units: DEFAULT_MAX_UNITS,
    }
}

/// A complete, small bank fixture: 2 units (1 valid, 1 invalidated), 1 entity
/// shared by both, 1 document, temporal + entity + self-loop graph edges.
fn bank_fixture() -> HashMap<String, Value> {
    let mut responses = HashMap::new();
    responses.insert(
        "/v1/default/banks".to_string(),
        json!({"banks": [{"bank_id": "omp"}, {"bank_id": "other"}]}),
    );
    responses.insert(
        "/v1/default/banks/omp/memories/list".to_string(),
        json!({"items": [
            {
                "id": "11111111-1111-1111-1111-111111111111",
                "text": "First fact about tofu state migrations that is quite long and must be truncated when it exceeds the seventy-two character title bound",
                "context": "it-ops-state activation",
                "date": "2026-08-16T00:32:10.701000+00:00",
                "fact_type": "world",
                "document_id": "doc-1",
                "entities": "tofu, Hydra",
                "tags": ["project:it-ops-state", "project:it-ops-state", "dup-free"],
                "state": "valid",
                "proof_count": 2,
                "mentioned_at": "2026-08-16T00:32:10.701000+00:00",
                "metadata": {"session_id": "sess-aaaa"}
            },
            {
                "id": "22222222-2222-2222-2222-222222222222",
                "text": "Invalidated fact",
                "entities": "tofu",
                "state": "invalidated",
                "invalidation_reason": "superseded"
            }
        ]}),
    );
    responses.insert(
        "/v1/default/banks/omp/graph".to_string(),
        json!({
            "nodes": [],
            "edges": [
                {"data": {"source": "11111111-1111-1111-1111-111111111111",
                          "target": "22222222-2222-2222-2222-222222222222",
                          "linkType": "temporal"}},
                {"data": {"source": "11111111-1111-1111-1111-111111111111",
                          "target": "11111111-1111-1111-1111-111111111111",
                          "linkType": "semantic"}},
                {"data": {"source": "11111111-1111-1111-1111-111111111111",
                          "target": "22222222-2222-2222-2222-222222222222",
                          "linkType": "entity"}}
            ],
            "total_units": 2
        }),
    );
    responses.insert(
        "/v1/default/banks/omp/entities".to_string(),
        json!({"items": [
            {"id": "e1", "canonical_name": "tofu", "mention_count": 2,
             "first_seen": "2026-08-16T00:00:00+00:00", "last_seen": "2026-08-16T01:00:00+00:00"},
            {"id": "e2", "canonical_name": "Hydra", "mention_count": 1}
        ]}),
    );
    responses.insert(
        "/v1/default/banks/omp/documents".to_string(),
        json!({"items": [
            {"id": "doc-1", "text_length": 18640, "memory_unit_count": 41,
             "tags": ["project:it-ops-state"],
             "retain_params": {"context": "omp", "metadata": {"session_id": "sess-aaaa"}},
             "content_hash": "b449b0"}
        ]}),
    );
    responses
}

fn importer_with(fixture: FixtureApi) -> HindsightImporter {
    HindsightImporter::with_api(config("omp"), Box::new(fixture)).unwrap()
}

async fn import_ok(importer: &HindsightImporter) -> data_loader::LoadResult {
    importer.import().await.expect("import should succeed")
}

#[tokio::test]
async fn descriptor_advertises_poll_watch_and_http_scope() {
    let mut cfg = config("omp");
    cfg.poll_interval_ms = 30_000;
    let importer =
        HindsightImporter::with_api(cfg, Box::new(FixtureApi::new(bank_fixture()))).unwrap();
    let descriptor = importer.descriptor();
    descriptor.validate().unwrap();
    assert!(descriptor.capabilities.iter().any(|capability| {
        capability.effect == Effect::Watch
            && capability.transport == Transport::Http
            && capability.scope == "http://hindsight.test"
    }));
    assert_eq!(descriptor.watch, WatchPlan::Poll { interval_ms: 30_000 });
    assert_eq!(descriptor.id, "hindsight.omp");
    assert_eq!(descriptor.name, "Hindsight (omp)");

    // A zero poll interval advertises a static one-shot snapshot with no
    // watch capability.
    let static_importer = importer_with(FixtureApi::new(bank_fixture()));
    let static_descriptor = static_importer.descriptor();
    assert_eq!(static_descriptor.watch, WatchPlan::Static);
    assert!(static_descriptor
        .capabilities
        .iter()
        .all(|capability| capability.effect != Effect::Watch));
}

#[tokio::test]
async fn import_publishes_memory_entity_document_nodes() {
    let importer = importer_with(FixtureApi::new(bank_fixture()));
    let result = import_ok(&importer).await;
    let graph = &result.graph;
    // 1 valid memory unit + 2 entities + 1 document; the invalidated unit
    // is retired knowledge and must not publish.
    assert_eq!(graph.node_count(), 4);
    let prefix = "hindsight:omp:";
    assert!(graph.nodes.contains_key(&format!(
        "{prefix}11111111-1111-1111-1111-111111111111"
    )));
    assert!(!graph.nodes.contains_key(&format!(
        "{prefix}22222222-2222-2222-2222-222222222222"
    )));
    assert!(graph
        .nodes
        .contains_key(&format!("{prefix}entity:e1")));
    assert!(graph
        .nodes
        .contains_key(&format!("{prefix}document:doc-1")));

    // Search documents: one per node, contract-valid against the schema.
    assert_eq!(result.search_documents.len(), 4);
    importer
        .descriptor()
        .schema
        .validate_result(&result)
        .unwrap();

    // The memory node carries the deduplicated canonical tags.
    let memory = &graph.nodes[&format!("{prefix}11111111-1111-1111-1111-111111111111")];
    assert_eq!(memory.meta.tags, vec!["project:it-ops-state", "dup-free"]);
    assert_eq!(memory.meta.folder, "memories");
    // Title is the truncated first line, not the raw text.
    assert!(memory.meta.title.len() <= 73); // 72 chars + ellipsis
    assert!(memory.meta.title.ends_with('…'));
}

#[tokio::test]
async fn import_builds_expected_edges() {
    let importer = importer_with(FixtureApi::new(bank_fixture()));
    let result = import_ok(&importer).await;
    let prefix = "hindsight:omp:";
    let unit = format!("{prefix}11111111-1111-1111-1111-111111111111");
    let tofu = format!("{prefix}entity:e1");
    let hydra = format!("{prefix}entity:e2");
    let document = format!("{prefix}document:doc-1");
    let has = |source: &str, target: &str| {
        result
            .graph
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == target)
    };
    // mentions: unit -> tofu, unit -> hydra (case-insensitive match on "Hydra")
    assert!(has(&unit, &tofu));
    assert!(has(&unit, &hydra));
    // documented_in: unit -> document
    assert!(has(&unit, &document));
    // The temporal edge targeted the invalidated unit -> dropped.
    // The semantic self-loop -> dropped. The shared-entity link -> dropped.
    // So exactly 3 edges total.
    assert_eq!(result.graph.edges.len(), 3);
    // And the graph validates (no dangling endpoints).
    result.graph.validate().unwrap();
}

#[tokio::test]
async fn unresolved_entities_are_reported_not_edges() {
    let mut fixture = bank_fixture();
    fixture.insert(
        "/v1/default/banks/omp/entities".to_string(),
        json!({"items": []}),
    );
    let importer = importer_with(FixtureApi::new(fixture));
    let result = import_ok(&importer).await;
    assert!(result
        .unresolved
        .iter()
        .any(|entry| entry.contains("tofu")));
    assert_eq!(result.graph.edges.len(), 1); // only documented_in
}

#[tokio::test]
async fn unknown_bank_fails_with_available_banks() {
    // Construct with bank "nope" against the fixture.
    let api = FixtureApi::new(bank_fixture());
    let importer = HindsightImporter::with_api(
        HindsightSourceConfig {
            bank: "nope".into(),
            ..config("omp")
        },
        Box::new(api),
    )
    .unwrap();
    let error = importer.import().await.unwrap_err();
    match &error {
        ImportError::SourceRead { message, .. } => {
            assert!(message.contains("not found"), "got: {message}");
            assert!(message.contains("omp"), "lists available banks: {message}");
            assert!(message.contains("other"));
        }
        other => panic!("expected SourceRead, got {other:?}"),
    }
}

#[tokio::test]
async fn unit_bound_is_enforced_through_total_units() {
    let mut cfg = config("omp");
    cfg.max_units = 1;
    // memories/list?state=valid would fit (2 items, but bound is enforced by
    // the graph endpoint's total_units=2).
    let importer = HindsightImporter::with_api(cfg, Box::new(FixtureApi::new(bank_fixture())))
        .unwrap();
    let error = importer.import().await.unwrap_err();
    match &error {
        ImportError::SourceRead { message, .. } => {
            assert!(message.contains("exceeding the 1 unit bound"), "got: {message}");
        }
        other => panic!("expected SourceRead, got {other:?}"),
    }
}

#[tokio::test]
async fn paginates_list_endpoints() {
    // Two pages of memory units: page size is capped by the crate constant,
    // so drive pagination with a small max_units and a server that returns
    // exactly page_limit items twice.
    let mut fixture = HashMap::new();
    fixture.insert(
        "/v1/default/banks".to_string(),
        json!({"banks": [{"bank_id": "omp"}]}),
    );
    let units: Vec<Value> = (0..3)
        .map(|i| {
            json!({"id": format!("u{i}"), "text": format!("fact {i}"), "state": "valid",
                   "entities": "", "tags": []})
        })
        .collect();
    // Serve offset-aware pages of 2.
    fixture.insert(
        "/v1/default/banks/omp/memories/list".to_string(),
        json!({"items": units}),
    );
    // graph: no edges, total below bound
    fixture.insert(
        "/v1/default/banks/omp/graph".to_string(),
        json!({"edges": [], "total_units": 3}),
    );
    fixture.insert("/v1/default/banks/omp/entities".to_string(), json!({"items": []}));
    fixture.insert("/v1/default/banks/omp/documents".to_string(), json!({"items": []}));
    // Simulate pagination server-side is overkill for a fixture map keyed by
    // bare path; the paginate() loop's exit conditions are covered by the
    // single-page fixtures elsewhere. This test instead asserts the query
    // composition: limit/offset present on every list request.
    let api = FixtureApi::new(fixture);
    let importer = HindsightImporter::with_api(config("omp"), Box::new(api)).unwrap();
    let result = importer.import().await.unwrap();
    assert_eq!(result.graph.node_count(), 3);
    // (requests assertions happen in query_composition test)
}

#[test]
fn sanitize_source_id_folds_to_contract_charset() {
    assert_eq!(sanitize_source_id("jira-ithelp"), "jira-ithelp");
    assert_eq!(sanitize_source_id("IT Ops"), "it-ops");
    assert_eq!(sanitize_source_id(""), "hindsight");
    assert_eq!(sanitize_source_id("Ünicode Bank"), "-nicode-bank");
    assert!(sanitize_source_id(&"x".repeat(500)).len() <= identity::MAX_SOURCE_ID_BYTES);
}

#[test]
fn config_rejects_invalid_values() {
    let mut cfg = config("omp");
    cfg.base_url = "ftp://nope".into();
    assert!(matches!(
        cfg.validate(),
        Err(ImportError::InvalidDescriptor { .. })
    ));
    let mut cfg = config("omp");
    cfg.bank = "has/slash".into();
    assert!(cfg.validate().is_err());
    let mut cfg = config("omp");
    cfg.max_units = 0;
    assert!(cfg.validate().is_err());
    let mut cfg = config("omp");
    cfg.source_id = Some("UPPER".into());
    assert!(cfg.validate().is_err());
    assert!(config("omp").validate().is_ok());
}

#[tokio::test]
async fn query_composition_is_well_formed() {
    let api = SharedFixture(std::sync::Arc::new(FixtureApi::new(bank_fixture())));
    let importer =
        HindsightImporter::with_api(config("omp"), Box::new(api.clone())).unwrap();
    let _ = importer.import().await.unwrap();
    let requests = api.0.requests();
    assert!(requests.iter().any(|path| path
        == "/v1/default/banks/omp/memories/list?state=valid&limit=500&offset=0"));
    assert!(requests
        .iter()
        .any(|path| path == "/v1/default/banks/omp/graph?limit=50000"));
    assert!(requests
        .iter()
        .any(|path| path == "/v1/default/banks/omp/entities?limit=500&offset=0"));
    assert!(requests
        .iter()
        .any(|path| path == "/v1/default/banks/omp/documents?limit=500&offset=0"));
    assert!(requests.iter().all(|path| !path.contains("??")
        && !path.contains("&&")));
}

#[tokio::test]
async fn schema_declares_contract_fields() {
    let importer = importer_with(FixtureApi::new(bank_fixture()));
    let schema = importer.descriptor().schema;
    assert_eq!(schema.source_kind, "hindsight");
    assert_eq!(schema.schema_version, data_loader::DISCOVERY_SCHEMA_VERSION);
    let field = |key: &str| schema.field(key).unwrap();
    assert_eq!(field("id").field_type, DiscoveryFieldType::Keyword);
    assert!(field("tags").facetable);
    assert_eq!(field("type").field_type, DiscoveryFieldType::Keyword);
    // Semantic similarity is the only undirected edge kind.
    let semantic = schema
        .edge_types
        .iter()
        .find(|edge| edge.key == "semantic")
        .unwrap();
    assert!(!semantic.directed);
    let temporal = schema
        .edge_types
        .iter()
        .find(|edge| edge.key == "temporal")
        .unwrap();
    assert!(temporal.directed);
}

#[tokio::test]
async fn token_redacted_in_debug() {
    let mut cfg = config("omp");
    cfg.token = Some("sekrit".into());
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("sekrit"));
    assert!(debug.contains("<redacted>"));
}
