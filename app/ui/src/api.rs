//! Typed client for graph-api (`crates/graph-api`).
//!
//! Three wire formats, mirroring the server:
//!   - JSON for control-plane endpoints (/graph/ids, /progress, /vault/page)
//!   - protobuf (prost) for structured payloads (/graph/init, /node/:id,
//!     /search) — same `graph.proto` schema the server builds against
//!   - raw little-endian f32/u32 buffers for bulk numeric data
//!     (/graph/positions, /graph/edges, /graph/metrics/:name)
//!
//! The base URL is configurable at runtime and persisted in local storage —
//! localhost in dev, a LAN/Tailscale address from another device.

use gloo_net::http::Request;
use gloo_storage::{LocalStorage, Storage};
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::proto;

const URL_KEY: &str = "jc_server_url";

/// Default API base. 127.0.0.1, not "localhost": on macOS `localhost`
/// resolves to ::1 (IPv6) first, but the dev server binds IPv4. 8765 is the
/// `just dev-up` compose port; set `JC_SERVER_URL` at build time (e.g.
/// `JC_SERVER_URL=http://127.0.0.1:8766 just app-dev`) to point a dev build
/// elsewhere without touching localStorage.
pub fn default_url() -> String {
    option_env!("JC_SERVER_URL").unwrap_or("http://127.0.0.1:8765").to_string()
}

pub fn server_url() -> String {
    let v: String = LocalStorage::get(URL_KEY).unwrap_or_else(|_| default_url());
    if v.trim().is_empty() {
        default_url()
    } else {
        v.trim_end_matches('/').to_string()
    }
}

pub fn set_server_url(url: &str) {
    let _ = LocalStorage::set(URL_KEY, url.trim().trim_end_matches('/'));
}

pub type ApiResult<T> = Result<T, String>;

pub(crate) fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub(crate) fn url(path: &str) -> String {
    format!("{}{}", server_url(), path)
}

/// Percent-encode a node id for the `/node/*id` route — ids are vault paths,
/// so the `/` separators must survive encoding.
fn encode_id(id: &str) -> String {
    id.split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> ApiResult<T> {
    Request::get(&url(path)).send().await.map_err(err)?.json().await.map_err(err)
}

pub(crate) async fn get_bytes(path: &str) -> ApiResult<Vec<u8>> {
    let resp = Request::get(&url(path)).send().await.map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    resp.binary().await.map_err(err)
}

pub(crate) async fn get_proto<T: Message + Default>(path: &str) -> ApiResult<T> {
    let bytes = get_bytes(path).await?;
    T::decode(bytes.as_slice()).map_err(err)
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

// --- graph data ---------------------------------------------------------------

/// `/graph/init` — node/edge counts, community/wcc counts, color palette.
pub async fn init() -> ApiResult<proto::Init> {
    get_proto("/graph/init").await
}

/// `/graph/ids` — node ids in the same order as the binary buffers.
pub async fn ids() -> ApiResult<Vec<String>> {
    get_json("/graph/ids").await
}

/// `/graph/positions` — flat [x0, y0, x1, y1, …] f32 buffer.
///
/// Unused since the wgpu renderer landed: it seeds its own 3D positions
/// (sphere shell + multilevel coarsening warm-up, like the egui app) and
/// the GPU force sim takes over from there. Kept for parity with the
/// server's endpoint surface.
#[allow(dead_code)]
pub async fn positions() -> ApiResult<Vec<f32>> {
    Ok(f32s(&get_bytes("/graph/positions").await?))
}

/// `/graph/edges` — flat [src, tgt, …] u32 buffer of dense node indices.
pub async fn edges() -> ApiResult<Vec<u32>> {
    Ok(u32s(&get_bytes("/graph/edges").await?))
}

/// `/graph/metrics/:name` — per-node f32 buffer (degree, pagerank, community, …).
pub async fn metric(name: &str) -> ApiResult<Vec<f32>> {
    Ok(f32s(&get_bytes(&format!("/graph/metrics/{name}")).await?))
}

/// `/node/*id` — full per-node metadata + markdown body.
pub async fn node_meta(id: &str) -> ApiResult<proto::NodeMeta> {
    get_proto(&format!("/node/{}", encode_id(id))).await
}

/// `/search?q=…` — BM25 full-text search (vault-search) with title fallback.
pub async fn search(q: &str, limit: u32) -> ApiResult<proto::SearchResults> {
    get_proto(&format!("/search?q={}&limit={limit}", urlencoding::encode(q))).await
}

// --- vault writes ---------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
pub struct VaultPagePutResp {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// `PUT /vault/page` — write a note's body markdown (frontmatter on disk is
/// preserved verbatim). `path` follows the vault-links convention: relative,
/// no `.md` extension, matching `NodeMeta.path`.
pub async fn put_page(path: &str, body: &str) -> ApiResult<VaultPagePutResp> {
    Request::put(&url("/vault/page"))
        .json(&serde_json::json!({ "path": path, "body": body }))
        .map_err(err)?
        .send()
        .await
        .map_err(err)?
        .json()
        .await
        .map_err(err)
}

// --- progress -------------------------------------------------------------------

/// Mirrors `graph-api::progress::{ProgressEvent, Stamped, ProgressResponse}`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    Start { id: u64, group: String, label: String },
    SetProgress { id: u64, progress: f32 },
    UpdateLabel { id: u64, label: String },
    Finish { id: u64 },
    Fail { id: u64, reason: String },
    Log { level: LogLevel, group: String, message: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Stamped {
    pub seq: u64,
    pub ts_ms: u64,
    pub event: ProgressEvent,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProgressResponse {
    pub next_seq: u64,
    pub server_ms: u64,
    pub events: Vec<Stamped>,
}

/// `GET /progress?since=<seq>` — tail of the server-side progress event log.
pub async fn progress(since: u64) -> ApiResult<ProgressResponse> {
    get_json(&format!("/progress?since={since}")).await
}

#[allow(dead_code)] // not surfaced in a panel yet — /configs is dev-only on the server
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConfigEntry {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GET /configs` — named AppState presets (dev mode only on the server).
#[allow(dead_code)]
pub async fn configs() -> ApiResult<Vec<ConfigEntry>> {
    get_json("/configs").await
}
