//! Connector tests: every test drives the connector through a fixture
//! [`JsonTransport`], so no test in this crate ever reaches a real network.
//!
//! The fixture transport carries only what the connector needs to inspect:
//! a route table of `URL -> Result<Vec<u8>, ImportError>` and a request log
//! tests read to assert URL shape, count, and ordering.

use std::collections::BTreeMap;
use std::sync::Arc;

use data_loader::{Capability, Effect, ImportError, ImportFuture, SourceConnector, Transport};
use parking_lot::Mutex;

use crate::connector::{
    HttpJsonConnector, JsonTransport, ReqwestTransport, CONTENT_TYPE, USER_AGENT,
};
use crate::manifest::{Limits, ValidatedPackage};
use crate::InstanceConfig;

const BASE_URL: &str = "http://api.example.test";
const ROOT: &str = "http://api.example.test";

/// Two node collections, a preflight, and `limit_offset` pagination — enough
/// surface to exercise every connector capability without dragging in an
/// edge-rule schema. `page_size = 2` so tests can use small fixture pages.
const PACKAGE: &str = r#"
format_version = 1

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[variables]]
name = "bank"
default = "omp"

[preflight]
path = "/v1/banks"
items_pointer = "/items"
id_pointer = "/bank_id"
variable = "bank"
subject = "bank"

[[collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
query = { state = "valid" }
paginate = { style = "limit_offset" }
items_pointer = "/items"
total_pointer = "/total"

[collections.nodes]
id_pointer = "/id"
node_type = "memory"

[collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"

[[collections]]
name = "entities"
path = "/v1/banks/{bank}/entities"
paginate = { style = "limit_offset" }

[collections.nodes]
id_pointer = "/id"
node_type = "entity"

[collections.nodes.title]
pointer = "/name"
fallback_prefix = "entity"

[limits]
page_size = 2
max_records = 50

[schema]
[[schema.edge_types]]
key = "mentions"
directed = true
"#;

/// A package with no preflight and a single paginated collection. `page_size
/// = 2` to keep fixture pages small.
const PACKAGE_NO_PREFLIGHT: &str = r#"
format_version = 1

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[variables]]
name = "bank"
default = "omp"

[[collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[collections.nodes]
id_pointer = "/id"
node_type = "memory"

[collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"

[limits]
page_size = 2
max_records = 50

[schema]
[[schema.edge_types]]
key = "mentions"
directed = true
"#;

/// A package with pagination set to `none` and a static query — used by
/// tests that want to assert single-request + well-formed-query behavior.
const PACKAGE_NONE_PAGINATION: &str = r#"
format_version = 1

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[variables]]
name = "bank"
default = "omp"

[[collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
query = { state = "valid" }

[collections.nodes]
id_pointer = "/id"
node_type = "memory"

[collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"

[schema]
[[schema.edge_types]]
key = "mentions"
directed = true
"#;

/// A package tuned for the record-bound failure test: page_size = 1,
/// max_records = 2. The connector must therefore fail when the server keeps
/// returning full pages.
const PACKAGE_TIGHT_BOUND: &str = r#"
format_version = 1

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[variables]]
name = "bank"
default = "omp"

[[collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[collections.nodes]
id_pointer = "/id"
node_type = "memory"

[collections.nodes.title]
pointer = "/text"
fallback_prefix = "memory"

[limits]
page_size = 1
max_records = 2

[schema]
[[schema.edge_types]]
key = "mentions"
directed = true
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

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn debug_redacts_the_token() {
    let instance = instance(Some("ghp_supersecret_token"));
    let transport = ReqwestTransport::new(&instance, Limits::default()).expect("transport builds");
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

    let records = run(connector.read()).expect("read succeeds");
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

    let error = run(connector.read()).expect_err("preflight must fail");
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

    let error = run(connector.read()).expect_err("preflight must fail");
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

    let records = run(connector.read()).expect("read succeeds");
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
    let _ = run(connector.read());
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
    let _ = run(connector.read());
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

    let records = run(connector.read()).expect("read succeeds");
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
    // page_size = 1, max_records = 2. The server returns 5 full pages of 1.
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

    let error = run(connector.read()).expect_err("bound must fail");
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
    // Use a package with a total_pointer and max_records = 50. Server
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

    let error = run(connector.read()).expect_err("total over bound must fail");
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

    let error = run(connector.read()).expect_err("preflight must surface the transport error");
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

    let error = run(connector.read()).expect_err("unbound placeholder must fail");
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
    assert_eq!(USER_AGENT, "jump-cannon-http-json-importer");
}
