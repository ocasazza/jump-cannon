//! HTTP/JSON connector — the single component in this crate that touches a
//! network or filesystem.
//!
//! Everything downstream (the [`crate::JsonDecoder`] and the
//! [`crate::mapper::ManifestMapper`]) operates on bytes and decoded values, so
//! tests drive the whole importer through a fixture [`JsonTransport`]
//! implementation without ever reaching a real server.
//!
//! The seam is intentionally narrow: [`JsonTransport::get`] is the only
//! effectful method on the connector's surface. [`ReqwestTransport`] is the
//! production implementation; tests substitute their own. Errors are
//! [`data_loader::ImportError`] so the connector fits the same pipeline as
//! every other [`data_loader::SourceConnector`] in the repo.

use std::collections::BTreeMap;
use std::fmt;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, SourceConnector, SourceRecord, Transport,
};

use crate::manifest::{Collection, Preflight, ValidatedPackage, HARD_LIMITS, SOURCE_KIND};
use crate::{InstanceConfig, RECORD_COLLECTION_KEY, RECORD_PAGE_KEY};

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
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
    max_response_bytes: usize,
    timeout_seconds: u64,
}

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

impl ReqwestTransport {
    pub fn new(instance: &InstanceConfig, limits: crate::manifest::Limits) -> Result<Self, ImportError> {
        let max_response_bytes = limits.max_response_bytes;
        if max_response_bytes == 0 || max_response_bytes > HARD_LIMITS.max_response_bytes {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "http-json: max_response_bytes must be between 1 and {}, got {max_response_bytes}",
                    HARD_LIMITS.max_response_bytes
                ),
            });
        }
        let timeout_seconds = limits.request_timeout_seconds;
        if timeout_seconds == 0 || timeout_seconds > HARD_LIMITS.request_timeout_seconds {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "http-json: request_timeout_seconds must be between 1 and {}, got {timeout_seconds}",
                    HARD_LIMITS.request_timeout_seconds
                ),
            });
        }
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| ImportError::InvalidDescriptor {
                message: format!("http-json: failed to build HTTP client: {error}"),
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
        Ok(Self {
            package,
            instance,
            variables,
            transport,
        })
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
                    message: format!("http-json: {field} has an unterminated {{ placeholder"),
                });
            };
            let name = &after[..end];
            let value = self.variables.get(name).ok_or_else(|| ImportError::InvalidDescriptor {
                message: format!(
                    "http-json: {field} references unresolved variable {name:?}; bound variables are [{}]",
                    self.variables.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            })?;
            out.push_str(value);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
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
                "http-json: preflight variable {:?} is not bound; resolved variables are [{}]",
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
        let limits = self.package.limits();
        let page_size = limits.page_size;
        let max_records = limits.max_records;
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
            let query = match collection.paginate {
                crate::manifest::Pagination::None => static_query.clone(),
                crate::manifest::Pagination::LimitOffset => {
                    Self::pagination_query(&static_query, page_size, offset)
                }
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
            match collection.paginate {
                crate::manifest::Pagination::None => break,
                crate::manifest::Pagination::LimitOffset => {
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
                let mut out = Vec::with_capacity(
                    self.package.collections().len() + self.package.preflight().is_some() as usize,
                );
                if let Some(preflight) = self.package.preflight() {
                    match self.preflight_url(preflight) {
                        Ok(url) => out.push(Capability::new(effect, Transport::Http, url)),
                        Err(error) => {
                            tracing::error!(?error, "http-json: dropping preflight capability");
                            // Continue with collections; `read()` will fail
                            // loudly when the preflight URL is built.
                        }
                    }
                }
                for collection in self.package.collections() {
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
                                "http-json: dropping collection capability"
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
            if let Some(preflight) = self.package.preflight() {
                self.run_preflight(preflight).await?;
            }
            for collection in self.package.collections() {
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
