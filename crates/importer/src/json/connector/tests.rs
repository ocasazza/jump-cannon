//! Connector tests: every test drives the connector through a fixture
//! [`JsonTransport`], so no test in this crate ever reaches a real network.
//!
//! The fixture transport carries only what the connector needs to inspect:
//! a route table of `URL -> Result<Vec<u8>, ImportError>` and a request log
//! tests read to assert URL shape, count, and ordering.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, ImportProgress, NoProgress, SourceConnector,
    Transport,
};
use parking_lot::Mutex;

use super::{HttpJsonConnector, JsonTransport, CONTENT_TYPE};
use crate::{InstanceConfig, ValidatedPackage};

const BASE_URL: &str = "http://api.example.test";
const ROOT: &str = "http://api.example.test";

/// Two node collections, a preflight, and `limit_offset` pagination — enough
/// surface to exercise every connector capability without dragging in an
/// edge-rule schema. `page_size = 2` so tests can use small fixture pages.
const PACKAGE: &str = r#"
format_version = 3

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[limits]
nodes = 50

[[schema.edge_types]]
key = "mentions"
directed = true

[parser]
engine = "json"
page_size = 2

[[parser.variables]]
name = "bank"
default = "omp"

[parser.preflight]
path = "/v1/banks"
items_pointer = "/items"
id_pointer = "/bank_id"
variable = "bank"
subject = "bank"

[[parser.collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
query = { state = "valid" }
paginate = { style = "limit_offset" }
items_pointer = "/items"
total_pointer = "/total"

[parser.collections.nodes]
id_pointer = "/id"
node_type = "memory"

[parser.collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"

[[parser.collections]]
name = "entities"
path = "/v1/banks/{bank}/entities"
paginate = { style = "limit_offset" }

[parser.collections.nodes]
id_pointer = "/id"
node_type = "entity"

[parser.collections.nodes.title]
pointer = "/name"
fallback_prefix = "entity"
"#;

/// A package with no preflight and a single paginated collection. `page_size
/// = 2` to keep fixture pages small.
const PACKAGE_NO_PREFLIGHT: &str = r#"
format_version = 3

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[limits]
nodes = 50

[[schema.edge_types]]
key = "mentions"
directed = true

[parser]
engine = "json"
page_size = 2

[[parser.variables]]
name = "bank"
default = "omp"

[[parser.collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[parser.collections.nodes]
id_pointer = "/id"
node_type = "memory"

[parser.collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"
"#;

/// A package with pagination set to `none` and a static query — used by
/// tests that want to assert single-request + well-formed-query behavior.
const PACKAGE_NONE_PAGINATION: &str = r#"
format_version = 3

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[schema.edge_types]]
key = "mentions"
directed = true

[parser]
engine = "json"

[[parser.variables]]
name = "bank"
default = "omp"

[[parser.collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
query = { state = "valid" }

[parser.collections.nodes]
id_pointer = "/id"
node_type = "memory"

[parser.collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"
"#;

/// A package tuned for the record-bound failure test: page_size = 1,
/// nodes = 2. The connector must therefore fail when the server keeps
/// returning full pages.
const PACKAGE_TIGHT_BOUND: &str = r#"
format_version = 3

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[limits]
nodes = 2

[[schema.edge_types]]
key = "mentions"
directed = true

[parser]
engine = "json"
page_size = 1

[[parser.variables]]
name = "bank"
default = "omp"

[[parser.collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[parser.collections.nodes]
id_pointer = "/id"
node_type = "memory"

[parser.collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"
"#;

fn package(bytes: &str) -> ValidatedPackage {
    ValidatedPackage::from_toml_bytes(bytes.as_bytes()).expect("package validates")
}

fn instance(token: Option<&str>) -> InstanceConfig {
    InstanceConfig {
        source_id: "omp".into(),
        base_url: BASE_URL.into(),
        variables: BTreeMap::from([("bank".to_string(), "omp".to_string())]),
        token: token.map(str::to_string),
        poll_interval_ms: 60_000,
    }
}

fn build_connector(pkg: &str, transport: Box<dyn JsonTransport>) -> HttpJsonConnector {
    let package = package(pkg);
    let variables = package
        .resolve_variables(&BTreeMap::from([("bank".to_string(), "omp".to_string())]))
        .expect("variables resolve");
    HttpJsonConnector::new(package, instance(None), variables, transport).expect("connector builds")
}

/// A deterministic, in-memory [`JsonTransport`]. Two pieces of state:
/// `routes` for URL → response, and `requests` for the test to inspect.
#[derive(Clone)]
struct FixtureTransport {
    inner: Arc<FixtureTransportInner>,
}

struct FixtureTransportInner {
    routes: BTreeMap<String, Result<Vec<u8>, ImportError>>,
    requests: Mutex<Vec<String>>,
}

impl FixtureTransport {
    fn new(routes: BTreeMap<String, Result<Vec<u8>, ImportError>>) -> Self {
        Self {
            inner: Arc::new(FixtureTransportInner {
                routes,
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.inner.requests.lock().clone()
    }

    fn request_count(&self) -> usize {
        self.inner.requests.lock().len()
    }
}

impl JsonTransport for FixtureTransport {
    fn get<'a>(&'a self, url: &'a str) -> ImportFuture<'a, Result<Vec<u8>, ImportError>> {
        let result = self
            .inner
            .routes
            .get(url)
            .cloned()
            .unwrap_or_else(|| Err(ImportError::SourceRead {
                origin: url.to_string(),
                message: format!("no fixture for {url}"),
            }));
        let mut reqs = self.inner.requests.lock();
        reqs.push(url.to_string());
        drop(reqs);
        Box::pin(async move { result })
    }
}

fn ok_response(body: &[u8]) -> Result<Vec<u8>, ImportError> {
    Ok(body.to_vec())
}

fn body(json: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json).expect("serialize")
}

fn pages(iter: impl IntoIterator<Item = serde_json::Value>) -> Vec<u8> {
    let pages: Vec<_> = iter.into_iter().collect();
    body(serde_json::json!({ "items": pages, "total": pages.len() }))
}

fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[cfg(feature = "native")]
#[test]
fn debug_redacts_the_token() {
    use super::ReqwestTransport;

    let instance = instance(Some("ghp_supersecret_token"));
    let transport = ReqwestTransport::new(&instance, crate::Limits::default(), 120)
        .expect("transport builds");
    let debug = format!("{transport:?}");
    assert!(
        !debug.contains("ghp_supersecret_token"),
        "token leaks through Debug: {debug}"
    );
    assert!(
        debug.contains("<redacted>"),
        "Debug must mark the redacted token: {debug}"
    );
    assert!(
        debug.contains("base_url"),
        "Debug should describe the transport: {debug}"
    );
}

#[test]
fn debug_redacts_the_token_on_instance_config() {
    let instance = instance(Some("ghp_supersecret_token"));
    let debug = format!("{instance:?}");
    assert!(!debug.contains("ghp_supersecret_token"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[test]
fn capabilities_list_one_scope_per_collection_plus_preflight() {
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks"),
            ok_response(&body(serde_json::json!({ "items": [{ "bank_id": "omp" }] }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0"),
            ok_response(&body(serde_json::json!({ "items": [], "total": 0 }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/entities?limit=2&offset=0"),
            ok_response(&body(serde_json::json!({ "items": [], "total": 0 }))),
        ),
    ]));
    let connector = build_connector(PACKAGE, Box::new(transport));

    let read = connector.capabilities(Effect::Read);
    let expected = vec![
        Capability::new(Effect::Read, Transport::Http, format!("{ROOT}/v1/banks")),
        Capability::new(
            Effect::Read,
            Transport::Http,
            format!("{ROOT}/v1/banks/omp/memories"),
        ),
        Capability::new(
            Effect::Read,
            Transport::Http,
            format!("{ROOT}/v1/banks/omp/entities"),
        ),
    ];
    assert_eq!(read, expected, "Read capabilities must match expected scopes");

    let watch = connector.capabilities(Effect::Watch);
    let mut expected_watch = expected.clone();
    for capability in &mut expected_watch {
        capability.effect = Effect::Watch;
    }
    assert_eq!(watch, expected_watch, "Watch capabilities mirror Read scopes");

    // The scope list is deterministic and stable across calls.
    assert_eq!(connector.capabilities(Effect::Read), read);

    let write = connector.capabilities(Effect::Write);
    assert!(write.is_empty(), "connector opts out of Write");
}

#[test]
fn capabilities_omit_preflight_when_undeclared() {
    let transport = FixtureTransport::new(BTreeMap::new());
    let connector = build_connector(PACKAGE_NO_PREFLIGHT, Box::new(transport));
    let read = connector.capabilities(Effect::Read);
    assert_eq!(read.len(), 1);
    assert_eq!(
        read[0],
        Capability::new(
            Effect::Read,
            Transport::Http,
            format!("{ROOT}/v1/banks/omp/memories"),
        )
    );
}

#[test]
fn preflight_success_unlocks_collection_reads() {
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks"),
            ok_response(&body(serde_json::json!({
                "items": [
                    { "bank_id": "omp" },
                    { "bank_id": "jira-ithelp" },
                ]
            }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0"),
            ok_response(&body(serde_json::json!({ "items": [], "total": 0 }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/entities?limit=2&offset=0"),
            ok_response(&body(serde_json::json!({ "items": [], "total": 0 }))),
        ),
    ]));
    let connector = build_connector(PACKAGE, Box::new(transport.clone()));

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 2, "preflight + 2 collections → 2 records");
    assert_eq!(
        transport.request_count(),
        3,
        "preflight + 2 collections → 3 requests"
    );
}

#[test]
fn preflight_not_found_lists_available_values() {
    // The package's preflight variable is `bank`; the resolved value is
    // `omp`. The fixture returns two other banks.
    let transport = FixtureTransport::new(BTreeMap::from([(
        format!("{ROOT}/v1/banks"),
        ok_response(&body(serde_json::json!({
            "items": [
                { "bank_id": "confluence-itkb" },
                { "bank_id": "jira-ithelp" },
            ]
        }))),
    )]));
    let connector = build_connector(PACKAGE, Box::new(transport));

    let error = run(connector.read(&NoProgress)).expect_err("preflight must fail");
    match error {
        ImportError::SourceRead { origin, message } => {
            assert_eq!(origin, format!("{ROOT}/v1/banks"));
            assert!(
                message.contains("bank \"omp\" not found"),
                "message should name the requested value: {message}"
            );
            assert!(
                message.contains("available banks: confluence-itkb, jira-ithelp"),
                "message should list available values: {message}"
            );
        }
        other => panic!("expected SourceRead, got {other:?}"),
    }
}

#[test]
fn preflight_not_found_renders_none_when_empty() {
    let transport = FixtureTransport::new(BTreeMap::from([(
        format!("{ROOT}/v1/banks"),
        ok_response(&body(serde_json::json!({ "items": [] }))),
    )]));
    let connector = build_connector(PACKAGE, Box::new(transport));

    let error = run(connector.read(&NoProgress)).expect_err("preflight must fail");
    let message = match error {
        ImportError::SourceRead { message, .. } => message,
        other => panic!("expected SourceRead, got {other:?}"),
    };
    assert!(
        message.contains("available banks: (none)"),
        "empty list must render as (none): {message}"
    );
}

#[test]
fn limit_offset_walks_to_exhaustion() {
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=0"),
            ok_response(&pages(vec![
                serde_json::json!({ "id": "m1" }),
                serde_json::json!({ "id": "m2" }),
            ])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=2"),
            ok_response(&pages(vec![
                serde_json::json!({ "id": "m3" }),
                serde_json::json!({ "id": "m4" }),
            ])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=4"),
            ok_response(&pages(vec![
                serde_json::json!({ "id": "m5" }),
                serde_json::json!({ "id": "m6" }),
            ])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=6"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m7" })])),
        ),
    ]));
    let connector = build_connector(PACKAGE_NO_PREFLIGHT, Box::new(transport.clone()));

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 4, "expected 4 pages");

    let requests = transport.requests();
    assert_eq!(requests.len(), 4, "expected 4 GET requests");
    assert_eq!(
        requests,
        vec![
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=0"),
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=2"),
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=4"),
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=6"),
        ],
    );

    // Each record is tagged with its collection and zero-based page index.
    assert_eq!(
        records
            .iter()
            .map(|r| r.metadata["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
    );
    for record in &records {
        assert_eq!(record.metadata["collection"].as_str(), Some("memories"));
        assert_eq!(record.content_type, CONTENT_TYPE);
        assert!(record.origin.starts_with(ROOT));
    }
}

/// A page-window package - the OpenAlex shape: `page`/`per-page` query
/// parameters and a `{count}`-templated record cap, with no offset
/// parameter. `page_size = 2` bounds the window so tests can vary the cap.
const PACKAGE_PAGE_NUMBER: &str = r#"
format_version = 3

[metadata]
id = "test.windows"
name = "Test windows"
version = "1.0.0"

[limits]
nodes = 50

[[schema.edge_types]]
key = "cites"
directed = true

[parser]
engine = "json"
page_size = 2

[[parser.variables]]
name = "count"
default = "6"

[[parser.collections]]
name = "works"
path = "/works"
query = { search = "chem" }
paginate = { style = "page_number", max_records = "{count}" }
items_pointer = "/results"

[parser.collections.nodes]
id_pointer = "/id"
node_type = "work"

[parser.collections.nodes.title]
pointer = "/title"
fallback_prefix = "work"
"#;

/// The same shape with no `max_records`: the walk runs to a short window,
/// bounded loudly by `limits.nodes`.
const PACKAGE_PAGE_NUMBER_UNCAPPED: &str = r#"
format_version = 3

[metadata]
id = "test.windows"
name = "Test windows"
version = "1.0.0"

[limits]
nodes = 50

[[schema.edge_types]]
key = "cites"
directed = true

[parser]
engine = "json"
page_size = 2

[[parser.variables]]
name = "count"
default = "6"

[[parser.collections]]
name = "works"
path = "/works"
query = { search = "chem" }
paginate = { style = "page_number" }
items_pointer = "/results"

[parser.collections.nodes]
id_pointer = "/id"
node_type = "work"

[parser.collections.nodes.title]
pointer = "/title"
fallback_prefix = "work"
"#;

/// [`build_connector`] for packages whose variables are not `bank`: the
/// supplied map must name exactly the package's declared variables.
fn build_connector_vars(
    pkg: &str,
    transport: Box<dyn JsonTransport>,
    supplied: BTreeMap<String, String>,
) -> HttpJsonConnector {
    let package = package(pkg);
    let variables = package
        .resolve_variables(&supplied)
        .expect("variables resolve");
    HttpJsonConnector::new(package, instance(None), variables, transport).expect("connector builds")
}

/// One fixture page of works under `/results`.
fn works(items: &[&str]) -> Vec<u8> {
    let results: Vec<_> = items
        .iter()
        .map(|id| serde_json::json!({ "id": id, "title": format!("work {id}") }))
        .collect();
    body(serde_json::json!({ "results": results }))
}

#[test]
fn page_number_walks_constant_windows_to_the_cap() {
    // count = 6 (the default) with page_size = 2: ceil(6/2) = 3 full
    // windows serve the cap, and the walk must stop after page 3 instead
    // of requesting a fourth.
    let page = |n: usize| format!("{ROOT}/works?search=chem&page={n}&per-page=2");
    let transport = FixtureTransport::new(BTreeMap::from([
        (page(1), ok_response(&works(&["w1", "w2"]))),
        (page(2), ok_response(&works(&["w3", "w4"]))),
        (page(3), ok_response(&works(&["w5", "w6"]))),
    ]));
    let connector = build_connector_vars(
        PACKAGE_PAGE_NUMBER,
        Box::new(transport.clone()),
        BTreeMap::new(),
    );

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 3, "one record per fetched window");
    assert_eq!(
        records
            .iter()
            .map(|r| r.metadata["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "records keep their zero-based walk index"
    );
    assert_eq!(
        transport.requests(),
        vec![page(1), page(2), page(3)],
        "the walk stops once the cap is served"
    );
}

#[test]
fn page_number_cap_below_the_page_size_shrinks_the_window() {
    // count = 1: a single window of size 1 serves the cap exactly; a
    // full page_size window would import more records than the cap names.
    let transport = FixtureTransport::new(BTreeMap::from([(
        format!("{ROOT}/works?search=chem&page=1&per-page=1"),
        ok_response(&works(&["w1"])),
    )]));
    let connector = build_connector_vars(
        PACKAGE_PAGE_NUMBER,
        Box::new(transport.clone()),
        BTreeMap::from([("count".to_string(), "1".to_string())]),
    );

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 1);
    assert_eq!(
        transport.requests(),
        vec![format!("{ROOT}/works?search=chem&page=1&per-page=1")],
        "the window shrinks to the cap instead of importing page_size records"
    );
}

#[test]
fn page_number_stops_on_a_short_window_even_with_the_cap_unmet() {
    // count = 6, but the scope runs dry: page 2 returns one record. A
    // short window means the server has no more pages; the walk must stop
    // rather than keep paging for the records the cap still wants.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/works?search=chem&page=1&per-page=2"),
            ok_response(&works(&["w1", "w2"])),
        ),
        (
            format!("{ROOT}/works?search=chem&page=2&per-page=2"),
            ok_response(&works(&["w3"])),
        ),
    ]));
    let connector = build_connector_vars(
        PACKAGE_PAGE_NUMBER,
        Box::new(transport.clone()),
        BTreeMap::new(),
    );

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 2);
    assert_eq!(
        transport.requests(),
        vec![
            format!("{ROOT}/works?search=chem&page=1&per-page=2"),
            format!("{ROOT}/works?search=chem&page=2&per-page=2"),
        ],
        "a short window ends the walk before the cap is met"
    );
}

#[test]
fn page_number_without_a_cap_walks_to_exhaustion() {
    // No `max_records`: the walk ends only on a short window, with
    // `limits.nodes` as the loud backstop.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/works?search=chem&page=1&per-page=2"),
            ok_response(&works(&["w1", "w2"])),
        ),
        (
            format!("{ROOT}/works?search=chem&page=2&per-page=2"),
            ok_response(&works(&["w3", "w4"])),
        ),
        (
            format!("{ROOT}/works?search=chem&page=3&per-page=2"),
            ok_response(&works(&["w5"])),
        ),
    ]));
    let connector = build_connector_vars(
        PACKAGE_PAGE_NUMBER_UNCAPPED,
        Box::new(transport.clone()),
        BTreeMap::new(),
    );

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 3);
    assert_eq!(transport.request_count(), 3);
}

#[test]
fn page_number_cap_of_zero_is_rejected_loudly() {
    // A zero cap names no records at all - a package or value bug, not an
    // empty import. It must fail before any request is made.
    let transport = FixtureTransport::new(BTreeMap::new());
    let connector = build_connector_vars(
        PACKAGE_PAGE_NUMBER,
        Box::new(transport.clone()),
        BTreeMap::from([("count".to_string(), "0".to_string())]),
    );

    let error = run(connector.read(&NoProgress)).expect_err("a zero cap must fail");
    assert!(
        error.to_string().contains("max_records"),
        "the error must name the cap: {error:?}"
    );
    assert_eq!(transport.request_count(), 0);
}

/// An [`ImportProgress`] that records every event as a flat string log so a
/// test can assert the stage/advance/finish shape a paged pull emits.
#[derive(Default)]
struct RecordingProgress {
    events: Mutex<Vec<String>>,
}

impl ImportProgress for RecordingProgress {
    fn stage(&self, label: &str) -> u64 {
        let mut events = self.events.lock();
        events.push(format!("stage:{label}"));
        events.len() as u64
    }
    fn advance(&self, stage: u64, fraction: Option<f32>, detail: &str) {
        self.events
            .lock()
            .push(format!("advance:{stage}:{fraction:?}:{detail}"));
    }
    fn finish(&self, stage: u64) {
        self.events.lock().push(format!("finish:{stage}"));
    }
    fn fail(&self, stage: u64, reason: &str) {
        self.events.lock().push(format!("fail:{stage}:{reason}"));
    }
    fn log(&self, message: &str) {
        self.events.lock().push(format!("log:{message}"));
    }
}

#[test]
fn progress_reports_one_stage_three_advances_and_a_finish() {
    // Three pages: two full then a short one, so the connector walks to
    // exhaustion over exactly three requests.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=0"),
            ok_response(&pages(vec![
                serde_json::json!({ "id": "m1" }),
                serde_json::json!({ "id": "m2" }),
            ])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=2"),
            ok_response(&pages(vec![
                serde_json::json!({ "id": "m3" }),
                serde_json::json!({ "id": "m4" }),
            ])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=2&offset=4"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m5" })])),
        ),
    ]));
    let connector = build_connector(PACKAGE_NO_PREFLIGHT, Box::new(transport));
    let progress = RecordingProgress::default();

    let records = run(connector.read(&progress)).expect("read succeeds");
    assert_eq!(records.len(), 3, "three pages produce three records");

    let events = progress.events.lock().clone();
    assert_eq!(
        events.iter().filter(|e| e.starts_with("stage:")).count(),
        1,
        "one stage per collection: {events:?}"
    );
    assert!(
        events.contains(&"stage:Fetching memories from api.example.test".to_string()),
        "stage names the collection and host: {events:?}"
    );
    assert_eq!(
        events.iter().filter(|e| e.starts_with("finish:")).count(),
        1,
        "the stage finishes exactly once: {events:?}"
    );

    let advances: Vec<&String> = events
        .iter()
        .filter(|e| e.starts_with("advance:"))
        .collect();
    // Each page now emits two advances: a pre-request "requesting page N…"
    // then a post-parse detail with the record total. Three pages = 6 advances.
    assert_eq!(advances.len(), 6, "two advances per page (pre-request + result): {events:?}");
    // Filter to only the post-parse detail advances (those containing "records").
    let result_advances: Vec<&String> = advances.iter().filter(|a| a.contains("records")).collect();
    assert_eq!(result_advances.len(), 3, "three result advances: {result_advances:?}");
    assert!(result_advances[0].contains("2 records"), "{result_advances:?}");
    assert!(result_advances[1].contains("4 records"), "{result_advances:?}");
    assert!(result_advances[2].contains("5 records"), "{result_advances:?}");
    assert!(
        advances.iter().all(|a| a.contains("None")),
        "no declared total means None fraction: {advances:?}"
    );
}

#[test]
fn queries_with_static_query_join_with_amp_not_double_amp() {
    // PACKAGE has a `state = "valid"` static query on `memories`. With
    // limit_offset pagination, the connector must join it with `&`, never
    // `&&`, and never produce `??`.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks"),
            ok_response(&body(serde_json::json!({ "items": [{ "bank_id": "omp" }] }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m1" })])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/entities?limit=2&offset=0"),
            ok_response(&pages(vec![])),
        ),
    ]));
    let connector = build_connector(PACKAGE, Box::new(transport.clone()));
    let _ = run(connector.read(&NoProgress));
    let requests = transport.requests();
    assert!(
        requests.iter().any(|url| url
            == &format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0")),
        "memories URL with static query must use `&` and not `&&`: {requests:?}"
    );
    for url in &requests {
        assert!(!url.contains("??"), "URL has duplicate `?`: {url}");
        assert!(!url.contains("&&"), "URL has duplicate `&`: {url}");
    }
}

#[test]
fn queries_without_static_query_start_with_question() {
    // The `entities` collection has no static query. The limit_offset
    // pagination must therefore start with `?limit=…&offset=…`, never
    // `??limit=…`.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks"),
            ok_response(&body(serde_json::json!({ "items": [{ "bank_id": "omp" }] }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0"),
            ok_response(&pages(vec![])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/entities?limit=2&offset=0"),
            ok_response(&pages(vec![])),
        ),
    ]));
    let connector = build_connector(PACKAGE, Box::new(transport.clone()));
    let _ = run(connector.read(&NoProgress));
    let requests = transport.requests();
    assert!(
        requests.iter().any(|url| url
            == &format!("{ROOT}/v1/banks/omp/entities?limit=2&offset=0")),
        "entities URL without static query must start with `?`: {requests:?}"
    );
    for url in &requests {
        assert!(!url.contains("??"), "URL has duplicate `?`: {url}");
        assert!(!url.contains("&&"), "URL has duplicate `&`: {url}");
    }
}

#[test]
fn none_pagination_emits_one_request() {
    let transport = FixtureTransport::new(BTreeMap::from([(
        format!("{ROOT}/v1/banks/omp/memories?state=valid"),
        ok_response(&body(serde_json::json!({
            "items": [
                { "id": "m1" },
                { "id": "m2" },
                { "id": "m3" },
            ]
        }))),
    )]));
    let connector = build_connector(PACKAGE_NONE_PAGINATION, Box::new(transport.clone()));

    let records = run(connector.read(&NoProgress)).expect("read succeeds");
    assert_eq!(records.len(), 1, "Pagination::None must emit one record");
    assert_eq!(
        transport.requests(),
        vec![format!("{ROOT}/v1/banks/omp/memories?state=valid")],
    );
    assert_eq!(
        records[0].metadata["page"].as_u64(),
        Some(0),
        "page metadata is zero-based"
    );
}

#[test]
fn record_bound_failure_names_the_collection_and_bound() {
    // page_size = 1, nodes = 2. The server returns 5 full pages of 1.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=1&offset=0"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m1" })])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=1&offset=1"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m2" })])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=1&offset=2"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m3" })])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=1&offset=3"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m4" })])),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?limit=1&offset=4"),
            ok_response(&pages(vec![serde_json::json!({ "id": "m5" })])),
        ),
    ]));
    let connector = build_connector(PACKAGE_TIGHT_BOUND, Box::new(transport));

    let error = run(connector.read(&NoProgress)).expect_err("bound must fail");
    let message = match error {
        ImportError::SourceRead { message, .. } => message,
        other => panic!("expected SourceRead, got {other:?}"),
    };
    assert!(
        message.contains("collection memories"),
        "message must name the collection: {message}"
    );
    assert!(
        message.contains("2 record bound"),
        "message must name the bound: {message}"
    );
    assert!(
        message.contains("still returned full pages"),
        "message must explain why we failed: {message}"
    );
}

#[test]
fn total_pointer_over_bound_fails_loudly_with_both_numbers() {
    // Use a package with a total_pointer and nodes = 50. Server
    // reports total = 1000. The preflight in PACKAGE doesn't apply here.
    let transport = FixtureTransport::new(BTreeMap::from([
        (
            format!("{ROOT}/v1/banks"),
            ok_response(&body(serde_json::json!({ "items": [{ "bank_id": "omp" }] }))),
        ),
        (
            format!("{ROOT}/v1/banks/omp/memories?state=valid&limit=2&offset=0"),
            ok_response(&body(serde_json::json!({
                "items": [],
                "total": 1000,
            }))),
        ),
    ]));
    let connector = build_connector(PACKAGE, Box::new(transport));

    let error = run(connector.read(&NoProgress)).expect_err("total over bound must fail");
    let message = match error {
        ImportError::SourceRead { message, .. } => message,
        other => panic!("expected SourceRead, got {other:?}"),
    };
    assert!(
        message.contains("total 1000"),
        "message must include the server's total: {message}"
    );
    assert!(
        message.contains("50") && message.contains("bound"),
        "message must include the record bound: {message}"
    );
    assert!(
        message.contains("collection memories"),
        "message must name the collection: {message}"
    );
}

#[test]
fn non_two_xx_propagates_as_source_read() {
    let transport = FixtureTransport::new(BTreeMap::from([(
        format!("{ROOT}/v1/banks"),
        Err(ImportError::SourceRead {
            origin: format!("{ROOT}/v1/banks"),
            message: "HTTP 404: not found".into(),
        }),
    )]));
    let connector = build_connector(PACKAGE, Box::new(transport));

    let error = run(connector.read(&NoProgress)).expect_err("preflight must surface the transport error");
    match error {
        ImportError::SourceRead { origin, message } => {
            assert_eq!(origin, format!("{ROOT}/v1/banks"));
            assert!(message.contains("404"), "{message}");
            assert!(message.contains("not found"), "{message}");
        }
        other => panic!("expected SourceRead, got {other:?}"),
    }
}

#[test]
fn unresolved_placeholder_is_a_programming_error() {
    // The package validator rejects placeholders that do not name a declared
    // variable, but we can still construct a connector with a variables map
    // that omits one — for example, by binding nothing. The connector must
    // fail loudly.
    let transport = FixtureTransport::new(BTreeMap::new());
    let package = package(PACKAGE_NO_PREFLIGHT);
    let empty = BTreeMap::new();
    let connector = HttpJsonConnector::new(package, instance(None), empty, Box::new(transport))
        .expect("connector builds");

    let error = run(connector.read(&NoProgress)).expect_err("unbound placeholder must fail");
    let message = match error {
        ImportError::InvalidDescriptor { message } => message,
        ImportError::SourceRead { message, .. } => message,
        other => panic!("expected InvalidDescriptor or SourceRead, got {other:?}"),
    };
    assert!(
        message.contains("unresolved variable") || message.contains("references"),
        "message must name the unresolved placeholder: {message}"
    );
}

#[test]
fn user_agent_constant_is_what_we_send() {
    assert_eq!(super::USER_AGENT, "jump-cannon-http-json-importer");
}
