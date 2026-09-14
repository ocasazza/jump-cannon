//! Fixture-transport tests: no network, no real reqwest/gloo behavior. The
//! production transports are thin wrappers whose cap enforcement mirrors the
//! audited http-json streaming loop.

use std::sync::Arc;

use parking_lot::Mutex;

use super::*;
use data_loader::NoProgress;

struct FixtureTransport {
    response: Result<HttpResponse, String>,
    seen: Arc<Mutex<Vec<(String, Option<String>, usize)>>>,
}

impl FixtureTransport {
    fn ok(response: HttpResponse) -> Self {
        Self {
            response: Ok(response),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            response: Err(message.to_string()),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl HttpTransport for FixtureTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer_token: Option<&'a str>,
        max_response_bytes: usize,
    ) -> ImportFuture<'a, Result<HttpResponse, ImportError>> {
        self.seen.lock().push((
            url.to_string(),
            bearer_token.map(str::to_string),
            max_response_bytes,
        ));
        let response = self.response.clone().map_err(|message| ImportError::SourceRead {
            origin: url.to_string(),
            message,
        });
        Box::pin(async move { response })
    }
}

fn fixture_connector(response: HttpResponse, config: HttpsConfig) -> HttpsConnector {
    HttpsConnector::new(config, Box::new(FixtureTransport::ok(response))).unwrap()
}

fn ok_response() -> HttpResponse {
    HttpResponse {
        status: 200,
        content_type: None,
        bytes: Vec::new(),
    }
}

#[test]
fn declares_exact_url_capability_for_read_and_watch() {
    let connector = fixture_connector(
        ok_response(),
        HttpsConfig::new("https://api.example.com/v1/things"),
    );
    for effect in [Effect::Read, Effect::Watch] {
        assert_eq!(
            connector.capabilities(effect),
            vec![Capability::new(
                effect,
                Transport::Http,
                "https://api.example.com/v1/things"
            )]
        );
    }
    assert!(connector.capabilities(Effect::Write).is_empty());
    assert!(connector.capabilities(Effect::Search).is_empty());
}

#[tokio::test]
async fn emits_one_record_with_status_and_content_type() {
    let connector = fixture_connector(
        HttpResponse {
            status: 200,
            content_type: Some("application/json".to_string()),
            bytes: b"{\"ok\":true}".to_vec(),
        },
        HttpsConfig::new("https://api.example.com/v1/things"),
    );
    let records = connector.read(&NoProgress).await.unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.origin, "https://api.example.com/v1/things");
    assert_eq!(record.content_type, "application/json");
    assert_eq!(record.bytes, b"{\"ok\":true}");
    assert_eq!(
        record.metadata.get(HTTP_STATUS_KEY),
        Some(&serde_json::Value::Number(200.into()))
    );
}

#[tokio::test]
async fn defaults_content_type_when_absent() {
    let connector = fixture_connector(
        HttpResponse {
            status: 200,
            content_type: None,
            bytes: b"\x00\x01".to_vec(),
        },
        HttpsConfig::new("https://api.example.com/blob"),
    );
    let records = connector.read(&NoProgress).await.unwrap();
    assert_eq!(records[0].content_type, DEFAULT_CONTENT_TYPE);
}

#[tokio::test]
async fn passes_token_and_byte_cap_to_transport() {
    let transport = FixtureTransport::ok(ok_response());
    let seen = transport.seen.clone();
    let connector = HttpsConnector::new(
        HttpsConfig::new("https://api.example.com/secret")
            .with_bearer_token("s3cret")
            .with_max_response_bytes(1024),
        Box::new(transport),
    )
    .unwrap();
    connector.read(&NoProgress).await.unwrap();
    assert_eq!(
        seen.lock().as_slice(),
        &[(
            "https://api.example.com/secret".to_string(),
            Some("s3cret".to_string()),
            1024
        )]
    );
}

#[tokio::test]
async fn transport_error_propagates_as_source_read() {
    let connector = HttpsConnector::new(
        HttpsConfig::new("https://api.example.com/down"),
        Box::new(FixtureTransport::failing("connection refused")),
    )
    .unwrap();
    let error = connector.read(&NoProgress).await.unwrap_err();
    assert_eq!(
        error,
        ImportError::SourceRead {
            origin: "https://api.example.com/down".to_string(),
            message: "connection refused".to_string(),
        }
    );
}

#[tokio::test]
async fn write_is_unsupported() {
    let connector = fixture_connector(
        ok_response(),
        HttpsConfig::new("https://api.example.com/v1/things"),
    );
    let error = connector
        .write(data_loader::WriteRequest {
            origin: "https://api.example.com/v1/things".to_string(),
            content_type: "application/json".to_string(),
            bytes: Vec::new(),
            metadata: BTreeMap::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ImportError::UnsupportedEffect {
            effect: Effect::Write
        }
    );
}

#[test]
fn rejects_invalid_config() {
    for config in [
        HttpsConfig::new("ftp://example.com/x"),
        HttpsConfig::new("https://example.com").with_max_response_bytes(0),
        HttpsConfig::new("https://example.com").with_timeout_seconds(0),
    ] {
        assert!(matches!(
            config.validate(),
            Err(ImportError::InvalidDescriptor { .. })
        ));
    }
}

#[test]
fn debug_redacts_bearer_token() {
    let config = HttpsConfig::new("https://example.com").with_bearer_token("s3cret-token");
    let debug = format!("{config:?}");
    assert!(!debug.contains("s3cret-token"), "token leaked: {debug}");
    assert!(debug.contains("<redacted>"));
    let connector = fixture_connector(ok_response(), config);
    let debug = format!("{connector:?}");
    assert!(!debug.contains("s3cret-token"), "token leaked: {debug}");
}

#[test]
fn media_type_strips_parameters() {
    assert_eq!(media_type("application/json; charset=utf-8"), "application/json");
    assert_eq!(media_type("text/plain"), "text/plain");
}
