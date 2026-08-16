//! The Hindsight HTTP boundary — the only network code in this crate.
//!
//! The importer speaks the Hindsight REST API (`/v1/{tenant}/banks/...`).
//! Everything downstream (bank selection, pagination, mapping into the vault
//! graph) operates on parsed JSON, so tests drive the whole importer through
//! a fixture [`HindsightApi`] without touching the network.

use std::fmt;
use std::time::Duration;

use data_loader::{ImportError, ImportFuture};

/// User-Agent sent with every Hindsight request.
pub const USER_AGENT: &str = "jump-cannon-hindsight-importer";

/// Hard bound on one HTTP response body. A bank listing or page of memory
/// units is plain JSON; anything larger is an error (or an attack), not a
/// valid source.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// Per-request timeout. `GET .../graph` recomputes a bank's semantic and
/// temporal link set on demand, which grows superlinearly with unit count, so
/// this is sized for the slowest endpoint rather than the typical list call.
/// A poll that exceeds it fails the import and leaves the prior snapshot live.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Effectful Hindsight API boundary: one bounded GET returning parsed JSON.
pub trait HindsightApi: Send + Sync {
    /// GET `path` (already query-encoded, rooted at the API base) and parse
    /// the response body as JSON.
    fn get_json<'a>(
        &'a self,
        path: &'a str,
    ) -> ImportFuture<'a, Result<serde_json::Value, ImportError>>;
}

/// Production [`HindsightApi`]: one reqwest client against the configured
/// base URL, optionally authenticated with a bearer token.
pub struct HttpHindsightApi {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl fmt::Debug for HttpHindsightApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpHindsightApi")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl HttpHindsightApi {
    pub fn new(
        base_url: impl Into<String>,
        token: Option<String>,
    ) -> Result<Self, ImportError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| ImportError::SourceRead {
                origin: "hindsight client".into(),
                message: format!("build HTTP client: {error}"),
            })?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
        })
    }

    fn origin(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

impl HindsightApi for HttpHindsightApi {
    fn get_json<'a>(
        &'a self,
        path: &'a str,
    ) -> ImportFuture<'a, Result<serde_json::Value, ImportError>> {
        let origin = self.origin(path);
        Box::pin(async move {
            let mut request = self
                .client
                .get(&origin)
                .header(reqwest::header::ACCEPT, "application/json");
            // The token is attached here and nowhere else: it never appears
            // in logs, capability scopes, or error messages.
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(|error| ImportError::SourceRead {
                origin: origin.clone(),
                message: format!("request failed: {error}"),
            })?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let body = body.trim().chars().take(512).collect::<String>();
                return Err(ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!("HTTP {status}{body_semicolon}{body}", body_semicolon = if body.is_empty() { "" } else { ": " }),
                });
            }
            let body = response
                .bytes()
                .await
                .map_err(|error| ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!("read response body: {error}"),
                })?;
            if body.len() as u64 > MAX_RESPONSE_BYTES {
                return Err(ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!(
                        "response body {} bytes exceeds the {} byte bound",
                        body.len(),
                        MAX_RESPONSE_BYTES
                    ),
                });
            }
            serde_json::from_slice(&body).map_err(|error| ImportError::Decode {
                origin: origin.clone(),
                message: format!("invalid JSON response: {error}"),
            })
        })
    }
}
