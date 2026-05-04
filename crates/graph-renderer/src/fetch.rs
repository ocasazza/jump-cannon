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

    pub async fn node(&self, id: &str) -> Result<proto::NodeMeta, String> {
        let bytes = http_get_bytes(&self.url(&format!("/node/{id}"))).await?;
        proto::NodeMeta::decode(&*bytes).map_err(|e| format!("decode node: {e}"))
    }

    pub async fn meta_summary(&self) -> Result<proto::MetaSummary, String> {
        let bytes = http_get_bytes(&self.url("/graph/meta_summary")).await?;
        proto::MetaSummary::decode(&*bytes).map_err(|e| format!("decode meta_summary: {e}"))
    }

    pub async fn search(&self, q: &str) -> Result<proto::SearchResults, String> {
        let bytes = http_get_bytes(&self.url(&format!("/search?q={}", urlencode(q)))).await?;
        proto::SearchResults::decode(&*bytes).map_err(|e| format!("decode search: {e}"))
    }
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
