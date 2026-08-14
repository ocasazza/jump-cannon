//! The conditional codeload fetch — the only network code in this crate.
//!
//! Everything downstream (bounded extraction, Obsidian parsing, identity
//! remapping) operates on bytes and directories, so tests drive the whole
//! importer through a fixture [`TarballSource`] without touching the network.

use std::fmt;

use data_loader::{ImportError, ImportFuture};

use crate::USER_AGENT;

/// Outcome of one conditional tarball fetch.
pub enum FetchOutcome {
    /// The server answered 304 to our `If-None-Match`; the cached extraction
    /// is still current.
    NotModified,
    /// Fresh tarball bytes plus the validator to send on the next poll
    /// (`None` when the server stops sending `ETag` — the importer falls back
    /// to content-keyed caching rather than adding a GitHub API round-trip).
    Fetched { etag: Option<String>, bytes: Vec<u8> },
}

/// Effectful tarball boundary. Implementations perform the conditional GET;
/// the importer owns cache layout, extraction, and parsing.
pub trait TarballSource: Send + Sync {
    /// Fetch the tarball, revalidating against `etag` when one is cached.
    fn fetch<'a>(&'a self, etag: Option<&'a str>)
        -> ImportFuture<'a, Result<FetchOutcome, ImportError>>;

    /// Downcasting support for fixture sources in tests.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Production [`TarballSource`]: one conditional GET against codeload.
pub struct HttpTarballSource {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
    max_bytes: u64,
}

impl fmt::Debug for HttpTarballSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTarballSource")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl HttpTarballSource {
    pub fn new(url: String, token: Option<String>, max_bytes: u64) -> Result<Self, ImportError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| ImportError::InvalidDescriptor {
                message: format!("failed to build GitHub HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            url,
            token,
            max_bytes,
        })
    }
}

impl TarballSource for HttpTarballSource {
    fn fetch<'a>(
        &'a self,
        etag: Option<&'a str>,
    ) -> ImportFuture<'a, Result<FetchOutcome, ImportError>> {
        Box::pin(async move {
            let mut request = self.client.get(&self.url);
            if let Some(etag) = etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            // reqwest error displays never include request headers, so the
            // bearer token cannot leak through this message.
            let response = request.send().await.map_err(|error| ImportError::SourceRead {
                origin: self.url.clone(),
                message: format!("tarball request failed: {error}"),
            })?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_MODIFIED {
                tracing::debug!(url = %self.url, "GitHub tarball unchanged (304)");
                return Ok(FetchOutcome::NotModified);
            }
            if status != reqwest::StatusCode::OK {
                return Err(ImportError::SourceRead {
                    origin: self.url.clone(),
                    message: format!("tarball request failed with HTTP {status}"),
                });
            }
            if let Some(length) = response.content_length() {
                if length > self.max_bytes {
                    return Err(ImportError::SourceRead {
                        origin: self.url.clone(),
                        message: format!(
                            "tarball Content-Length {length} exceeds the {} byte bound",
                            self.max_bytes
                        ),
                    });
                }
            }
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let mut response = response;
            let mut bytes = Vec::new();
            while let Some(chunk) =
                response
                    .chunk()
                    .await
                    .map_err(|error| ImportError::SourceRead {
                        origin: self.url.clone(),
                        message: format!("tarball stream failed: {error}"),
                    })?
            {
                if bytes.len() as u64 + chunk.len() as u64 > self.max_bytes {
                    return Err(ImportError::SourceRead {
                        origin: self.url.clone(),
                        message: format!(
                            "tarball stream exceeds the {} byte bound",
                            self.max_bytes
                        ),
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            tracing::info!(url = %self.url, bytes = bytes.len(), "fetched GitHub tarball");
            Ok(FetchOutcome::Fetched { etag, bytes })
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
