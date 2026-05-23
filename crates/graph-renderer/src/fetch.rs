//! Thin async API client for graph-api. Single trait surface, two impls
//! (gloo-net on wasm32, reqwest elsewhere).
//!
//! Endpoints:
//!   GET /graph/init              -> protobuf Init
//!   GET /graph/ids               -> JSON [string]
//!   GET /graph/positions         -> raw little-endian f32 [x,y,x,y,...] (2D)
//!   GET /graph/edges             -> raw little-endian u32 [s,t,s,t,...]
//!   GET /graph/metrics/:name     -> raw little-endian f32 [v,v,...]
//!   GET /node/:id                -> protobuf NodeMeta
//!   GET /search?q=…              -> protobuf SearchResults
//
// TODO(phase 1 stream-consumer): graph-api now exposes
//   GET /graph/layout/stream  (WebSocket; binary frames of
//                              [u64 LE frame][u32 LE n_nodes][f32 LE positions...])
// When the user opts into the distributed backend, open a WS to that route
// (web-sys::WebSocket on wasm32, tokio-tungstenite native), push positions
// straight into `positions_buffer`, and skip the local `compute_step()` call
// in `graph_pipelines.rs`. Left as a stub here so the WASM build keeps working
// while the broker + worker scaffold lands; see plan
// `/home/casazza/.claude/plans/federated-swinging-rainbow.md` Phase 1.

use crate::proto;
use prost::Message;

#[derive(Clone)]
pub struct ApiClient {
    pub base: String,
}

impl ApiClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    fn url(&self, path: &str) -> String {
        let base = self.base.trim_end_matches('/');
        format!("{}{}", base, path)
    }

    pub async fn init(&self) -> Result<proto::Init, String> {
        let bytes = http_get_bytes(&self.url("/graph/init")).await?;
        proto::Init::decode(&*bytes).map_err(|e| format!("decode init: {e}"))
    }

    pub async fn ids(&self) -> Result<Vec<String>, String> {
        let bytes = http_get_bytes(&self.url("/graph/ids")).await?;
        serde_json::from_slice::<Vec<String>>(&bytes).map_err(|e| format!("decode ids: {e}"))
    }

    pub async fn positions(&self) -> Result<Vec<f32>, String> {
        let bytes = http_get_bytes(&self.url("/graph/positions")).await?;
        bytes_to_f32(&bytes)
    }

    pub async fn edges(&self) -> Result<Vec<u32>, String> {
        let bytes = http_get_bytes(&self.url("/graph/edges")).await?;
        bytes_to_u32(&bytes)
    }

    pub async fn metric(&self, name: &str) -> Result<Vec<f32>, String> {
        let bytes = http_get_bytes(&self.url(&format!("/graph/metrics/{name}"))).await?;
        bytes_to_f32(&bytes)
    }

    /// `/node/:id` — `Ok(Some)` on a hit, `Ok(None)` when the server returns
    /// 404 (the id isn't in the in-memory graph and no Prisma fallback is
    /// configured). Treating 404 as a soft outcome keeps the browser console
    /// quiet for ids that legitimately moved out of the layout graph.
    pub async fn node(&self, id: &str) -> Result<Option<proto::NodeMeta>, String> {
        match http_get_bytes_opt(&self.url(&format!("/node/{id}"))).await? {
            Some(bytes) => proto::NodeMeta::decode(&*bytes)
                .map(Some)
                .map_err(|e| format!("decode node: {e}")),
            None => Ok(None),
        }
    }

    pub async fn meta_summary(&self) -> Result<proto::MetaSummary, String> {
        let bytes = http_get_bytes(&self.url("/graph/meta_summary")).await?;
        proto::MetaSummary::decode(&*bytes).map_err(|e| format!("decode meta_summary: {e}"))
    }

    pub async fn search(&self, q: &str) -> Result<proto::SearchResults, String> {
        let bytes = http_get_bytes(&self.url(&format!("/search?q={}", urlencode(q)))).await?;
        proto::SearchResults::decode(&*bytes).map_err(|e| format!("decode search: {e}"))
    }

    /// `PUT /vault/page` — save the body of an obsidian-page node to disk.
    ///
    /// `path` follows the vault-links convention (the same string the
    /// renderer received in `NodeMeta.path` — relative, no `.md`
    /// extension). `body` is the body-only markdown; the server
    /// preserves the on-disk YAML frontmatter block verbatim.
    ///
    /// Returns `Ok(())` on a 200 response with `{ "ok": true }`, and
    /// `Err(...)` on any non-2xx status or `{ "ok": false }` payload.
    pub async fn save_page(&self, path: &str, body: &str) -> Result<(), String> {
        #[derive(serde::Serialize)]
        struct Req<'a> { path: &'a str, body: &'a str }
        #[derive(serde::Deserialize)]
        struct Resp { ok: bool, error: Option<String> }
        let url = self.url("/vault/page");
        let req = Req { path, body };
        let bytes = http_put_json(&url, &serde_json::to_vec(&req).map_err(|e| e.to_string())?)
            .await?;
        let resp: Resp =
            serde_json::from_slice(&bytes).map_err(|e| format!("decode save: {e}"))?;
        if resp.ok {
            Ok(())
        } else {
            Err(resp.error.unwrap_or_else(|| "save failed".into()))
        }
    }

    /// `GET /progress?since=<seq>` — tail of the server-side progress
    /// event log. Returns the response untyped (just bytes); decoding
    /// is the caller's job (see `app::kick_off_progress_poll`).
    pub async fn progress(&self, since: u64) -> Result<Vec<u8>, String> {
        http_get_bytes(&self.url(&format!("/progress?since={since}"))).await
    }

    /// `GET /compute/health` — `{ connected: bool, url: String }`.
    /// Used by the renderer to surface compute-broker liveness in the
    /// footer log. Returns `connected=false` when graph-api is up but
    /// the downstream gRPC dial to graph-compute is failing.
    pub async fn compute_health(&self) -> Result<ComputeHealth, String> {
        let bytes = http_get_bytes(&self.url("/compute/health")).await?;
        serde_json::from_slice::<ComputeHealth>(&bytes)
            .map_err(|e| format!("decode compute_health: {e}"))
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ComputeHealth {
    pub connected: bool,
    pub url: String,
}

fn bytes_to_f32(b: &[u8]) -> Result<Vec<f32>, String> {
    if b.len() % 4 != 0 {
        return Err(format!("not f32-aligned: {} bytes", b.len()));
    }
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        out.push(f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]));
    }
    Ok(out)
}

fn bytes_to_u32(b: &[u8]) -> Result<Vec<u32>, String> {
    if b.len() % 4 != 0 {
        return Err(format!("not u32-aligned: {} bytes", b.len()));
    }
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        out.push(u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]));
    }
    Ok(out)
}

fn urlencode(s: &str) -> String {
    // Minimal application/x-www-form-urlencoded. The query string we send is
    // whatever the user typed into a search box. Keep this simple — no
    // urlencoding crate dep on the WASM side.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push('+'),
            _ => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes();
                for &b in bytes {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
async fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    use gloo_net::http::Request;
    let resp = Request::get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.ok() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    resp.binary().await.map_err(|e| format!("body {url}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("body {url}: {e}"))?;
    Ok(bytes.to_vec())
}

/// Like `http_get_bytes`, but maps HTTP 404 to `Ok(None)` instead of
/// `Err`. Used by endpoints (currently `/node/:id`) where a missing
/// resource is an expected, non-error outcome.
#[cfg(target_arch = "wasm32")]
async fn http_put_json(url: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    use gloo_net::http::Request;
    let resp = Request::put(url)
        .header("content-type", "application/json")
        .body(body.to_vec())
        .map_err(|e| format!("build PUT {url}: {e}"))?
        .send()
        .await
        .map_err(|e| format!("PUT {url}: {e}"))?;
    if !resp.ok() {
        return Err(format!("PUT {url}: HTTP {}", resp.status()));
    }
    resp.binary().await.map_err(|e| format!("body {url}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn http_put_json(url: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .put(url)
        .header("content-type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| format!("PUT {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("PUT {url}: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("body {url}: {e}"))?;
    Ok(bytes.to_vec())
}

#[cfg(target_arch = "wasm32")]
async fn http_get_bytes_opt(url: &str) -> Result<Option<Vec<u8>>, String> {
    use gloo_net::http::Request;
    let resp = Request::get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if resp.status() == 404 {
        return Ok(None);
    }
    if !resp.ok() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    resp.binary()
        .await
        .map(Some)
        .map_err(|e| format!("body {url}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn http_get_bytes_opt(url: &str) -> Result<Option<Vec<u8>>, String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("body {url}: {e}"))?;
    Ok(Some(bytes.to_vec()))
}
