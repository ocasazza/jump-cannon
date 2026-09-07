//! HTTPS byte connector — GET one URL into bytes.
//!
//! [`HttpTransport`] is the only effectful seam: one GET returning bytes plus
//! the response metadata the connector reports. [`ReqwestTransport`] (native)
//! and [`GlooTransport`] (wasm32) are the production implementations; tests
//! substitute fixtures and never touch a network. The discipline mirrors the
//! retired http-json connector: the bearer token is runtime configuration
//! (never a package field, never a URL parameter), is redacted from `Debug`,
//! and cannot leak through error messages.

use std::collections::BTreeMap;
use std::fmt;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, SourceConnector, SourceRecord, Transport,
};

/// `User-Agent` header attached to every production request.
pub const USER_AGENT: &str = "jump-cannon-importer-connectors";

/// Media type reported when the response carries no usable `Content-Type`.
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Metadata key recording the HTTP status code of the fetch.
pub const HTTP_STATUS_KEY: &str = "http.status";

/// Default response bound: 64 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Default request timeout in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

/// Hard bounds: a configured limit above these is a configuration error, not
/// a runtime surprise.
const HARD_MAX_RESPONSE_BYTES: usize = 1024 * 1024 * 1024;
const HARD_MAX_TIMEOUT_SECONDS: u64 = 300;

/// Maximum bytes of a non-2xx body included in the error message. Bodies
/// beyond this are truncated with an ellipsis so diagnostics stay bounded.
const ERROR_BODY_EXCERPT_BYTES: usize = 256;

/// One completed HTTP GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// Effectful HTTP boundary. One method: a GET that returns raw bytes. The
/// connector owns capability declaration and record shape; the transport owns
/// authentication, headers, streaming, and byte-bound enforcement.
pub trait HttpTransport: Send + Sync {
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer_token: Option<&'a str>,
        max_response_bytes: usize,
    ) -> ImportFuture<'a, Result<HttpResponse, ImportError>>;
}

/// Runtime configuration for one HTTPS source. The bearer token is bound at
/// runtime — never inside a package — and is redacted from `Debug`.
#[derive(Clone)]
pub struct HttpsConfig {
    pub url: String,
    pub bearer_token: Option<String>,
    pub max_response_bytes: usize,
    pub timeout_seconds: u64,
}

impl fmt::Debug for HttpsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsConfig")
            .field("url", &self.url)
            .field("bearer_token", &self.bearer_token.as_ref().map(|_| "<redacted>"))
            .field("max_response_bytes", &self.max_response_bytes)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

impl HttpsConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            bearer_token: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub fn with_timeout_seconds(mut self, timeout_seconds: u64) -> Self {
        self.timeout_seconds = timeout_seconds;
        self
    }

    pub fn validate(&self) -> Result<(), ImportError> {
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "https: url must start with http:// or https://, got {:?}",
                    self.url
                ),
            });
        }
        if self.max_response_bytes == 0 || self.max_response_bytes > HARD_MAX_RESPONSE_BYTES {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "https: max_response_bytes must be between 1 and {HARD_MAX_RESPONSE_BYTES}, got {}",
                    self.max_response_bytes
                ),
            });
        }
        if self.timeout_seconds == 0 || self.timeout_seconds > HARD_MAX_TIMEOUT_SECONDS {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "https: timeout_seconds must be between 1 and {HARD_MAX_TIMEOUT_SECONDS}, got {}",
                    self.timeout_seconds
                ),
            });
        }
        Ok(())
    }
}

/// Effectful source connector for one HTTPS URL. Emits exactly one
/// [`SourceRecord`] per read, with the response status in metadata.
pub struct HttpsConnector {
    config: HttpsConfig,
    transport: Box<dyn HttpTransport>,
}

impl fmt::Debug for HttpsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsConnector")
            .field("config", &self.config)
            // The transport owns the wire; its own Debug must redact secrets.
            .field("transport", &"<HttpTransport>")
            .finish()
    }
}

impl HttpsConnector {
    pub fn new(
        config: HttpsConfig,
        transport: Box<dyn HttpTransport>,
    ) -> Result<Self, ImportError> {
        config.validate()?;
        Ok(Self { config, transport })
    }

    /// Production constructor: reqwest (rustls) on native targets.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn production(config: HttpsConfig) -> Result<Self, ImportError> {
        let transport = ReqwestTransport::new(config.timeout_seconds)?;
        Self::new(config, Box::new(transport))
    }

    /// Production constructor: gloo-net on wasm32 (browser fetch API; the
    /// browser owns timeouts).
    #[cfg(target_arch = "wasm32")]
    pub fn production(config: HttpsConfig) -> Result<Self, ImportError> {
        Self::new(config, Box::new(GlooTransport))
    }

    pub fn config(&self) -> &HttpsConfig {
        &self.config
    }
}

impl SourceConnector for HttpsConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        match effect {
            Effect::Read | Effect::Watch => {
                vec![Capability::new(
                    effect,
                    Transport::Http,
                    self.config.url.clone(),
                )]
            }
            _ => Vec::new(),
        }
    }

    fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move {
            let response = self
                .transport
                .get(
                    &self.config.url,
                    self.config.bearer_token.as_deref(),
                    self.config.max_response_bytes,
                )
                .await?;
            let mut metadata = BTreeMap::new();
            metadata.insert(
                HTTP_STATUS_KEY.to_string(),
                serde_json::Value::Number(response.status.into()),
            );
            Ok(vec![SourceRecord {
                origin: self.config.url.clone(),
                content_type: response
                    .content_type
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
                bytes: response.bytes,
                metadata,
            }])
        })
    }
}

/// Strip `;` parameters from a `Content-Type` header value.
fn media_type(value: &str) -> String {
    value.split(';').next().unwrap_or_default().trim().to_string()
}

fn truncate_excerpt(bytes: &[u8], limit: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let slice = if bytes.len() <= limit {
        bytes
    } else {
        &bytes[..limit]
    };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if bytes.len() > limit {
        text.push('…');
    }
    text
}

/// Production [`HttpTransport`] over one shared `reqwest::Client` (rustls).
/// The bearer token is applied per request as a header — never a URL
/// parameter — and reqwest's error display never includes request headers,
/// so the token cannot leak through [`ImportError`] messages.
#[cfg(not(target_arch = "wasm32"))]
pub struct ReqwestTransport {
    client: reqwest::Client,
    timeout_seconds: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl fmt::Debug for ReqwestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestTransport")
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ReqwestTransport {
    pub fn new(timeout_seconds: u64) -> Result<Self, ImportError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| ImportError::InvalidDescriptor {
                message: format!("https: failed to build HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            timeout_seconds,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpTransport for ReqwestTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer_token: Option<&'a str>,
        max_response_bytes: usize,
    ) -> ImportFuture<'a, Result<HttpResponse, ImportError>> {
        Box::pin(async move {
            let mut request = self.client.get(url);
            if let Some(token) = bearer_token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(|error| ImportError::SourceRead {
                origin: url.to_string(),
                message: format!("HTTP request failed: {error}"),
            })?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(media_type)
                .filter(|value| !value.is_empty());
            if !status.is_success() {
                let body = response.bytes().await.unwrap_or_default();
                let excerpt = truncate_excerpt(&body, ERROR_BODY_EXCERPT_BYTES);
                return Err(ImportError::SourceRead {
                    origin: url.to_string(),
                    message: format!("HTTP {status}: {excerpt}"),
                });
            }
            if let Some(length) = response.content_length() {
                if length as usize > max_response_bytes {
                    return Err(ImportError::SourceRead {
                        origin: url.to_string(),
                        message: format!(
                            "response Content-Length {length} exceeds the {max_response_bytes} byte bound"
                        ),
                    });
                }
            }
            let mut bytes = Vec::new();
            let mut stream = response;
            while let Some(chunk) =
                stream
                    .chunk()
                    .await
                    .map_err(|error| ImportError::SourceRead {
                        origin: url.to_string(),
                        message: format!("HTTP stream failed: {error}"),
                    })?
            {
                if bytes.len() + chunk.len() > max_response_bytes {
                    return Err(ImportError::SourceRead {
                        origin: url.to_string(),
                        message: format!(
                            "response stream exceeds the {max_response_bytes} byte bound"
                        ),
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status: status.as_u16(),
                content_type,
                bytes,
            })
        })
    }
}

/// Production [`HttpTransport`] for wasm32 over the browser fetch API. The
/// browser enforces its own timeouts; the byte bound is checked against
/// `Content-Length` when present and against the decoded body otherwise.
#[cfg(target_arch = "wasm32")]
pub struct GlooTransport;

#[cfg(target_arch = "wasm32")]
impl fmt::Debug for GlooTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlooTransport").finish()
    }
}

#[cfg(target_arch = "wasm32")]
impl HttpTransport for GlooTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer_token: Option<&'a str>,
        max_response_bytes: usize,
    ) -> ImportFuture<'a, Result<HttpResponse, ImportError>> {
        // gloo-net's futures are !Send (they hold JsValues), but wasm32 is
        // single-threaded: run the fetch on the local event loop and bridge
        // the Send payload through a oneshot so the returned future is Send.
        let url = url.to_string();
        let bearer_token = bearer_token.map(str::to_string);
        let (tx, rx) = futures_channel::oneshot::channel();
        wasm_bindgen_futures::spawn_local(fetch_then_send(
            url.clone(),
            bearer_token,
            max_response_bytes,
            tx,
        ));
        Box::pin(async move {
            rx.await.map_err(|_| ImportError::SourceRead {
                origin: url,
                message: "fetch task was dropped before completing".to_string(),
            })?
        })
    }
}

#[cfg(target_arch = "wasm32")]
async fn fetch_then_send(
    url: String,
    bearer_token: Option<String>,
    max_response_bytes: usize,
    tx: futures_channel::oneshot::Sender<Result<HttpResponse, ImportError>>,
) {
    let _ = tx.send(fetch(url, bearer_token, max_response_bytes).await);
}

#[cfg(target_arch = "wasm32")]
async fn fetch(
    url: String,
    bearer_token: Option<String>,
    max_response_bytes: usize,
) -> Result<HttpResponse, ImportError> {
    let mut request = gloo_net::http::Request::get(&url);
    if let Some(token) = &bearer_token {
        let header = format!("Bearer {token}");
        request = request.header("Authorization", &header);
    }
    let response = request.send().await.map_err(|error| ImportError::SourceRead {
        origin: url.clone(),
        message: format!("HTTP request failed: {error}"),
    })?;
    let status = response.status();
    let headers = response.headers();
    let content_type = headers
        .get("content-type")
        .map(|value| media_type(&value))
        .filter(|value| !value.is_empty());
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        let excerpt = truncate_excerpt(body.as_bytes(), ERROR_BODY_EXCERPT_BYTES);
        return Err(ImportError::SourceRead {
            origin: url,
            message: format!("HTTP {status}: {excerpt}"),
        });
    }
    if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > max_response_bytes {
            return Err(ImportError::SourceRead {
                origin: url,
                message: format!(
                    "response Content-Length {length} exceeds the {max_response_bytes} byte bound"
                ),
            });
        }
    }
    let bytes = response.binary().await.map_err(|error| ImportError::SourceRead {
        origin: url.clone(),
        message: format!("HTTP body read failed: {error}"),
    })?;
    if bytes.len() > max_response_bytes {
        return Err(ImportError::SourceRead {
            origin: url,
            message: format!("response body exceeds the {max_response_bytes} byte bound"),
        });
    }
    Ok(HttpResponse {
        status,
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests;
