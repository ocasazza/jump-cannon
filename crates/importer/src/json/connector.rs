//! HTTP/JSON connector — the single json-engine component that touches a
//! network or filesystem.
//!
//! Everything downstream (the [`crate::JsonDecoder`] and the
//! [`crate::json::mapper::ManifestMapper`]) operates on bytes and decoded
//! values, so tests drive the whole importer through a fixture
//! [`JsonTransport`] implementation without ever reaching a real server.
//!
//! The seam is intentionally narrow: [`JsonTransport::get`] is the only
//! effectful method on the connector's surface. [`ReqwestTransport`] is the
//! production implementation (`native` feature); tests substitute their own.
//! Errors are [`data_loader::ImportError`] so the connector fits the same
//! pipeline as every other [`data_loader::SourceConnector`] in the repo.

use std::collections::BTreeMap;
use std::fmt;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, SourceConnector, SourceRecord, Transport,
};

use super::config::{Collection, JsonEngineConfig, Preflight, SOURCE_KIND};
use super::{InstanceConfig, RECORD_COLLECTION_KEY, RECORD_PAGE_KEY};
use crate::ValidatedPackage;
#[cfg(feature = "native")]
use crate::HARD_LIMITS;

/// `User-Agent` header attached to every production request. Distinct from
/// `Cargo.toml`'s package name so the server logs and the package identity do
/// not silently drift apart.
pub const USER_AGENT: &str = "jump-cannon-http-json-importer";

/// Media type the connector reports on every [`SourceRecord`]. The schema
/// advertises the same list, so the pipeline's media-type check accepts what
/// we emit.
pub const CONTENT_TYPE: &str = "application/json";

/// Maximum bytes of a non-2xx body included in the error message. Bodies
/// beyond this are truncated with an ellipsis so the diagnostic stays bounded.
#[cfg(feature = "native")]
const ERROR_BODY_EXCERPT_BYTES: usize = 256;

/// Effectful HTTP boundary. One method: a GET that returns raw bytes. The
/// connector owns URL composition, pagination, bounds enforcement, and error
/// shape; the transport owns authentication, headers, and the actual byte
/// stream.
pub trait JsonTransport: Send + Sync {
    fn get<'a>(&'a self, url: &'a str) -> ImportFuture<'a, Result<Vec<u8>, ImportError>>;
}

/// Production [`JsonTransport`]: one `reqwest::Client` shared across every
/// collection, with a per-instance bearer token and a per-package response
/// bound. The token is held as a header (never a URL parameter, never
/// reflected in error messages) and is redacted from [`Debug`].
#[cfg(feature = "native")]
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
    max_response_bytes: usize,
    timeout_seconds: u64,
}

#[cfg(feature = "native")]
impl fmt::Debug for ReqwestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestTransport")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("max_response_bytes", &self.max_response_bytes)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

#[cfg(feature = "native")]
impl ReqwestTransport {
    pub fn new(
        instance: &InstanceConfig,
        limits: crate::Limits,
        timeout_seconds: u64,
    ) -> Result<Self, ImportError> {
        let max_response_bytes = limits.input_bytes;
        if max_response_bytes == 0 || max_response_bytes > HARD_LIMITS.input_bytes {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "json engine: max_response_bytes must be between 1 and {}, got {max_response_bytes}",
                    HARD_LIMITS.input_bytes
                ),
            });
        }
        if timeout_seconds == 0
            || timeout_seconds > super::config::MAX_REQUEST_TIMEOUT_SECONDS
        {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "json engine: request_timeout_seconds must be between 1 and {}, got {timeout_seconds}",
                    super::config::MAX_REQUEST_TIMEOUT_SECONDS
                ),
            });
        }
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| ImportError::InvalidDescriptor {
                message: format!("json engine: failed to build HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            base_url: instance.root().to_string(),
            token: instance.token.clone(),
            max_response_bytes,
            timeout_seconds,
        })
    }
}

#[cfg(feature = "native")]
impl JsonTransport for ReqwestTransport {
    fn get<'a>(&'a self, url: &'a str) -> ImportFuture<'a, Result<Vec<u8>, ImportError>> {
        Box::pin(async move {
            let mut request = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, CONTENT_TYPE);
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            // reqwest's error display never includes request headers, so the
            // bearer token cannot leak through these messages.
            let response = request.send().await.map_err(|error| ImportError::SourceRead {
                origin: url.to_string(),
                message: format!("HTTP request failed: {error}"),
            })?;
            let status = response.status();
            if !status.is_success() {
                let body = response.bytes().await.unwrap_or_default();
                let excerpt = truncate_excerpt(&body, ERROR_BODY_EXCERPT_BYTES);
                return Err(ImportError::SourceRead {
                    origin: url.to_string(),
                    message: format!("HTTP {status}: {excerpt}"),
                });
            }
            if let Some(length) = response.content_length() {
                if length as usize > self.max_response_bytes {
                    return Err(ImportError::SourceRead {
                        origin: url.to_string(),
                        message: format!(
                            "response Content-Length {length} exceeds the {} byte bound",
                            self.max_response_bytes
                        ),
                    });
                }
            }
            let mut stream = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.chunk().await.map_err(|error| ImportError::SourceRead {
                origin: url.to_string(),
                message: format!("HTTP stream failed: {error}"),
            })? {
                if bytes.len() + chunk.len() > self.max_response_bytes {
                    return Err(ImportError::SourceRead {
                        origin: url.to_string(),
                        message: format!(
                            "response stream exceeds the {} byte bound",
                            self.max_response_bytes
                        ),
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })
    }
}

#[cfg(feature = "native")]
fn truncate_excerpt(bytes: &[u8], limit: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let slice = if bytes.len() <= limit { bytes } else { &bytes[..limit] };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if bytes.len() > limit {
        text.push('…');
    }
    text
}

/// Effectful source connector for one HTTP/JSON API instance. Holds the
/// validated package, the resolved variables, and the configured transport;
/// performs template expansion, pagination, and bound enforcement; emits one
/// [`SourceRecord`] per fetched page.
pub struct HttpJsonConnector {
    package: ValidatedPackage,
    instance: InstanceConfig,
    variables: BTreeMap<String, String>,
    transport: Box<dyn JsonTransport>,
}

impl fmt::Debug for HttpJsonConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpJsonConnector")
            .field("source_id", &self.instance.source_id)
            .field("base_url", &self.instance.base_url)
            .field("variables", &self.variables)
            // The transport owns the token; its own Debug redacts it.
            .field("transport", &"<JsonTransport>")
            .finish()
    }
}

impl HttpJsonConnector {
    pub fn new(
        package: ValidatedPackage,
        instance: InstanceConfig,
        variables: BTreeMap<String, String>,
        transport: Box<dyn JsonTransport>,
    ) -> Result<Self, ImportError> {
        instance.validate()?;
        // Reject non-json packages: every collection access below assumes the
        // json engine configuration exists.
        package
            .json_config()
            .map_err(|error| ImportError::InvalidDescriptor {
                message: error.to_string(),
            })?;
        Ok(Self {
            package,
            instance,
            variables,
            transport,
        })
    }

    /// The engine configuration; [`Self::new`] rejects non-json packages.
    fn config(&self) -> &JsonEngineConfig {
        self.package
            .json_config()
            .expect("connector construction rejects non-json packages")
    }

    /// The package's preflight endpoint, if any. The URL is the resolved
    /// root plus the resolved path — no query.
    fn preflight_url(&self, preflight: &Preflight) -> Result<String, ImportError> {
        let path = self.resolve_template("preflight.path", &preflight.path)?;
        Ok(format!("{}{}", self.instance.root(), path))
    }

    /// One collection's URL for the supplied query (empty string = no
    /// query). URL composition is deterministic: `<root><resolved_path>`,
    /// optionally suffixed with `?<query>`.
    fn collection_url(&self, collection: &Collection, query: &str) -> Result<String, ImportError> {
        let path = self.resolve_template(&format!("{}.path", collection.name), &collection.path)?;
        let mut url = format!("{}{}", self.instance.root(), path);
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
        Ok(url)
    }

    /// Resolve every `{name}` placeholder with its resolved value. A
    /// missing placeholder is a developer bug: the package validator already
    /// rejects placeholders that do not name a declared variable, so a
    /// missing one at this layer means the caller supplied a `variables`
    /// map that disagrees with the package — never silent.
    fn resolve_template(&self, field: &str, template: &str) -> Result<String, ImportError> {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else {
                return Err(ImportError::InvalidDescriptor {
                    message: format!("json engine: {field} has an unterminated {{ placeholder"),
                });
            };
            let name = &after[..end];
            let value = self.variables.get(name).ok_or_else(|| ImportError::InvalidDescriptor {
                message: format!(
                    "json engine: {field} references unresolved variable {name:?}; bound variables are [{}]",
                    self.variables.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            })?;
            out.push_str(&Self::encode_value(value));
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Percent-encode one resolved variable value for URL interpolation
    /// (RFC 3986 unreserved characters pass through; everything else —
    /// spaces, slashes, query syntax — is encoded). Static template text is
    /// never encoded: paths and query separators (`/ ? & =`) there are the
    /// package author's own URL structure.
    fn encode_value(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    /// Render the static query portion of a collection (its declared
    /// `query` map, with placeholders resolved). Empty when the collection
    /// declares no static query, so pagination can simply append with `?`
    /// or `&` depending on what's already there.
    fn static_query(&self, collection: &Collection) -> Result<String, ImportError> {
        let mut pairs = Vec::with_capacity(collection.query.len());
        for (key, value) in &collection.query {
            let resolved = self.resolve_template(&format!("{}.query.{key}", collection.name), value)?;
            pairs.push(format!("{key}={resolved}"));
        }
        Ok(pairs.join("&"))
    }

    /// Append the pagination parameters to the static query. Join with `&`
    /// when the static query is non-empty, otherwise start fresh with the
    /// pagination parameters (`?limit=…&offset=…`, never `??`).
    fn pagination_query(query: &str, page_size: usize, offset: usize) -> String {
        if query.is_empty() {
            format!("limit={page_size}&offset={offset}")
        } else {
            format!("{query}&limit={page_size}&offset={offset}")
        }
    }

    /// Page-number variant of [`Self::pagination_query`], for APIs paged by
    /// `{page_param}={index}&{size_param}={page_size}` (PRIDE's
    /// `page`/`pageSize`, DataCite's `page[number]`/`page[size]`).
    fn page_number_query(
        query: &str,
        params: &super::config::PageNumberParams,
        page_size: usize,
        page: usize,
    ) -> String {
        let paging = format!(
            "{}={}&{}={}",
            params.page_param, page, params.size_param, page_size
        );
        if query.is_empty() {
            paging
        } else {
            format!("{query}&{paging}")
        }
    }

    async fn run_preflight(&self, preflight: &Preflight) -> Result<(), ImportError> {
        let url = self.preflight_url(preflight)?;
        let bytes = self.transport.get(&url).await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            ImportError::SourceRead {
                origin: url.clone(),
                message: format!("preflight response is not JSON: {error}"),
            }
        })?;
        let items = value.pointer(&preflight.items_pointer).ok_or_else(|| ImportError::SourceRead {
            origin: url.clone(),
            message: format!(
                "preflight response has nothing at {}",
                preflight.items_pointer
            ),
        })?;
        let items = items.as_array().ok_or_else(|| ImportError::SourceRead {
            origin: url.clone(),
            message: format!(
                "preflight response {} is not an array",
                preflight.items_pointer
            ),
        })?;
        let requested = self.variables.get(&preflight.variable).ok_or_else(|| ImportError::InvalidDescriptor {
            message: format!(
                "json engine: preflight variable {:?} is not bound; resolved variables are [{}]",
                preflight.variable,
                self.variables.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
        })?;
        let mut available = Vec::new();
        for item in items {
            let id = item.pointer(&preflight.id_pointer).ok_or_else(|| ImportError::SourceRead {
                origin: url.clone(),
                message: format!(
                    "preflight item has nothing at {}",
                    preflight.id_pointer
                ),
            })?;
            let id = match id {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            if id == *requested {
                return Ok(());
            }
            available.push(id);
        }
        let available = if available.is_empty() {
            "(none)".to_string()
        } else {
            available.join(", ")
        };
        Err(ImportError::SourceRead {
            origin: url,
            message: format!(
                "{} {:?} not found; available {}s: {}",
                preflight.subject, requested, preflight.subject, available
            ),
        })
    }

    async fn run_collection(
        &self,
        collection: &Collection,
        records: &mut Vec<SourceRecord>,
    ) -> Result<(), ImportError> {
        let config = self.config();
        let page_size = config.page_size;
        let max_records = self.package.limits().nodes;
        let static_query = self.static_query(collection)?;
        let mut offset: usize = 0;
        let mut accumulated: usize = 0;
        let mut page_index: usize = 0;
        loop {
            if accumulated >= max_records {
                // The previous page was full, so the server likely still has
                // more data. Fail loudly rather than silently truncate.
                return Err(ImportError::SourceRead {
                    origin: format!("{SOURCE_KIND}:{}", collection.name),
                    message: format!(
                        "collection {}: {} record bound reached while server still returned full pages",
                        collection.name, max_records
                    ),
                });
            }
            let query = match &collection.paginate {
                super::config::Pagination::None => static_query.clone(),
                super::config::Pagination::LimitOffset => {
                    Self::pagination_query(&static_query, page_size, offset)
                }
                super::config::Pagination::PageNumber(params) => Self::page_number_query(
                    &static_query,
                    params,
                    page_size,
                    params.first_page + page_index,
                ),
            };
            let url = self.collection_url(collection, &query)?;
            let bytes = self.transport.get(&url).await?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| ImportError::SourceRead {
                origin: url.clone(),
                message: format!("response is not JSON: {error}"),
            })?;
            if let Some(total_pointer) = &collection.total_pointer {
                let total = value.pointer(total_pointer).ok_or_else(|| ImportError::SourceRead {
                    origin: url.clone(),
                    message: format!("response has nothing at {total_pointer}"),
                })?;
                let total = total.as_u64().ok_or_else(|| ImportError::SourceRead {
                    origin: url.clone(),
                    message: format!("response {total_pointer} is not a number"),
                })?;
                if total as usize > max_records {
                    return Err(ImportError::SourceRead {
                        origin: url.clone(),
                        message: format!(
                            "collection {}: server reports total {total} records, which exceeds the {} bound",
                            collection.name, max_records
                        ),
                    });
                }
            }
            let items = value.pointer(&collection.items_pointer).ok_or_else(|| ImportError::SourceRead {
                origin: url.clone(),
                message: format!(
                    "response has nothing at {}",
                    collection.items_pointer
                ),
            })?;
            let items = items.as_array().ok_or_else(|| ImportError::SourceRead {
                origin: url.clone(),
                message: format!(
                    "response {} is not an array",
                    collection.items_pointer
                ),
            })?;
            let page_len = items.len();
            let mut metadata = BTreeMap::new();
            metadata.insert(
                RECORD_COLLECTION_KEY.to_string(),
                serde_json::Value::String(collection.name.clone()),
            );
            metadata.insert(
                RECORD_PAGE_KEY.to_string(),
                serde_json::Value::Number(page_index.into()),
            );
            records.push(SourceRecord {
                origin: url,
                content_type: CONTENT_TYPE.to_string(),
                bytes,
                metadata,
            });
            accumulated += page_len;
            page_index += 1;
            match &collection.paginate {
                super::config::Pagination::None => break,
                super::config::Pagination::LimitOffset
                | super::config::Pagination::PageNumber(_) => {
                    if page_len < page_size {
                        break;
                    }
                    offset += page_size;
                }
            }
        }
        Ok(())
    }
}

impl SourceConnector for HttpJsonConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        match effect {
            Effect::Read | Effect::Watch => {
                let config = self.config();
                let mut out = Vec::with_capacity(
                    config.collections.len() + config.preflight.is_some() as usize,
                );
                if let Some(preflight) = &config.preflight {
                    match self.preflight_url(preflight) {
                        Ok(url) => out.push(Capability::new(effect, Transport::Http, url)),
                        Err(error) => {
                            tracing::error!(?error, "json engine: dropping preflight capability");
                            // Continue with collections; `read()` will fail
                            // loudly when the preflight URL is built.
                        }
                    }
                }
                for collection in &config.collections {
                    let path_field = format!("{}.path", collection.name);
                    match self.resolve_template(&path_field, &collection.path) {
                        Ok(path) => out.push(Capability::new(
                            effect,
                            Transport::Http,
                            format!("{}{}", self.instance.root(), path),
                        )),
                        Err(error) => {
                            tracing::error!(
                                ?error,
                                collection = collection.name,
                                "json engine: dropping collection capability"
                            );
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move {
            let mut records = Vec::new();
            if let Some(preflight) = &self.config().preflight {
                self.run_preflight(preflight).await?;
            }
            for collection in &self.config().collections {
                self.run_collection(collection, &mut records).await?;
            }
            Ok(records)
        })
    }

    fn write<'a>(
        &'a self,
        _request: data_loader::WriteRequest,
    ) -> ImportFuture<'a, Result<data_loader::WriteReceipt, ImportError>> {
        Box::pin(async move {
            Err(ImportError::UnsupportedEffect {
                effect: Effect::Write,
            })
        })
    }
}

#[cfg(test)]
mod tests;
