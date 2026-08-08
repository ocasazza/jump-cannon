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
    // An explicit build-time override always wins (e.g. a phone/Tauri build
    // pointed at a remote Mac via `JC_SERVER_URL=…`).
    if let Some(u) = option_env!("JC_SERVER_URL") {
        return u.to_string();
    }
    // Otherwise, when a browser loaded this page from graph-api itself, fetch
    // from that same origin: `localhost` vs `127.0.0.1`, custom ports, LAN IPs
    // and Tailscale names then all "just work" without a per-origin
    // localStorage entry — and a stale stored URL can no longer strand the app
    // on a host the page was never served from. Only http(s) origins qualify;
    // the Tauri shell loads from `tauri://localhost` / `null`, which falls
    // through to the 127.0.0.1 default below.
    if let Some(origin) = page_origin() {
        if origin.starts_with("http://") || origin.starts_with("https://") {
            return origin;
        }
    }
    "http://127.0.0.1:8765".to_string()
}

/// The page's own origin (`http://host:port`), or `None` outside a browser.
/// Reflection-based (like `panels::instances::page_origin`) to avoid pulling in
/// the web-sys `Location` feature.
#[cfg(target_arch = "wasm32")]
fn page_origin() -> Option<String> {
    use wasm_bindgen::JsValue;
    let win = web_sys::window()?;
    let loc = js_sys::Reflect::get(win.as_ref(), &JsValue::from_str("location")).ok()?;
    js_sys::Reflect::get(&loc, &JsValue::from_str("origin"))
        .ok()?
        .as_string()
}

#[cfg(not(target_arch = "wasm32"))]
fn page_origin() -> Option<String> {
    None
}

/// Normalize a user-typed server URL past the two classic webview footguns:
/// a missing scheme (fetch treats `127.0.0.1:8766` as a relative URL) and
/// `localhost` (macOS WebKit resolves it to IPv6 `::1`, but graph-api binds
/// IPv4 `127.0.0.1` — the fetch dies as an opaque "TypeError: Load failed").
fn normalize_url(url: &str) -> String {
    let mut v = url.trim().trim_end_matches('/').to_string();
    if !v.is_empty() && !v.contains("://") {
        v = format!("http://{v}");
    }
    v.replace("://localhost", "://127.0.0.1")
}

pub fn server_url() -> String {
    let v: String = LocalStorage::get(URL_KEY).unwrap_or_else(|_| default_url());
    let v = normalize_url(&v);
    if v.is_empty() {
        default_url()
    } else {
        v
    }
}

pub fn set_server_url(url: &str) {
    let _ = LocalStorage::set(URL_KEY, normalize_url(url));
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

/// GET with the browser HTTP cache bypassed. The graph buffers are live
/// state, not assets: `/generate`, `/compute/soup`, and importer reloads swap
/// them mid-session, and a cached `/graph/edges` from a previous graph
/// poisons every consistency check until it expires (the server once
/// stamped them `immutable, max-age=1y` — WebKit took it at its word).
/// `NoStore` both skips existing entries and never writes new ones.
fn get(path: &str) -> gloo_net::http::RequestBuilder {
    Request::get(&url(path)).cache(web_sys::RequestCache::NoStore)
}

pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> ApiResult<T> {
    get(path)
        .send()
        .await
        .map_err(err)?
        .json()
        .await
        .map_err(err)
}

pub(crate) async fn put_json<I: Serialize, O: serde::de::DeserializeOwned>(
    path: &str,
    body: &I,
) -> ApiResult<O> {
    let resp = Request::put(&url(path))
        .json(body)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    resp.json().await.map_err(err)
}

pub(crate) async fn put_raw_json<O: serde::de::DeserializeOwned>(
    path: &str,
    body: Vec<u8>,
) -> ApiResult<O> {
    let resp = Request::put(&url(path))
        .header("Content-Type", "application/octet-stream")
        .body(body)
        .map_err(err)?
        .send()
        .await
        .map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    resp.json().await.map_err(err)
}

/// A graph-derived response paired with the topology revision advertised by
/// graph-api. Revision `0` means an older server did not provide the header.
pub(crate) struct Revisioned<T> {
    pub(crate) revision: u64,
    pub(crate) value: T,
}

fn graph_revision(resp: &gloo_net::http::Response) -> u64 {
    resp.headers()
        .get("X-Graph-Revision")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

async fn get_revisioned_json<T: serde::de::DeserializeOwned>(
    path: &str,
) -> ApiResult<Revisioned<T>> {
    let resp = get(path).send().await.map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    let revision = graph_revision(&resp);
    let value = resp.json().await.map_err(err)?;
    Ok(Revisioned { revision, value })
}

async fn get_revisioned_bytes(path: &str) -> ApiResult<Revisioned<Vec<u8>>> {
    let resp = get(path).send().await.map_err(err)?;
    if !resp.ok() {
        return Err(format!("{} -> HTTP {}", path, resp.status()));
    }
    let revision = graph_revision(&resp);
    let value = resp.binary().await.map_err(err)?;
    Ok(Revisioned { revision, value })
}

pub(crate) async fn get_bytes(path: &str) -> ApiResult<Vec<u8>> {
    let resp = get(path).send().await.map_err(err)?;
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
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// --- graph data ---------------------------------------------------------------

/// `/graph/init` — node/edge counts, community/wcc counts, color palette.
pub async fn init() -> ApiResult<proto::Init> {
    get_proto("/graph/init").await
}

/// `/graph/ids` — node ids in the same order as the binary buffers.
#[allow(dead_code)] // revision-aware bootstrap uses revisioned_ids
pub async fn ids() -> ApiResult<Vec<String>> {
    get_json("/graph/ids").await
}

pub(crate) async fn revisioned_ids() -> ApiResult<Revisioned<Vec<String>>> {
    get_revisioned_json("/graph/ids").await
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
#[allow(dead_code)] // revision-aware bootstrap uses revisioned_edges
pub async fn edges() -> ApiResult<Vec<u32>> {
    Ok(u32s(&get_bytes("/graph/edges").await?))
}

pub(crate) async fn revisioned_edges() -> ApiResult<Revisioned<Vec<u32>>> {
    let r = get_revisioned_bytes("/graph/edges").await?;
    Ok(Revisioned {
        revision: r.revision,
        value: u32s(&r.value),
    })
}

/// `/graph/metrics/:name` — per-node f32 buffer (degree, pagerank, community, …).
pub async fn metric(name: &str) -> ApiResult<Vec<f32>> {
    Ok(f32s(&get_bytes(&format!("/graph/metrics/{name}")).await?))
}

pub(crate) async fn revisioned_metric(name: &str) -> ApiResult<Revisioned<Vec<f32>>> {
    let r = get_revisioned_bytes(&format!("/graph/metrics/{name}")).await?;
    Ok(Revisioned {
        revision: r.revision,
        value: f32s(&r.value),
    })
}

/// `/node/*id` — full per-node metadata plus any readable source body.
pub async fn node_meta(id: &str) -> ApiResult<proto::NodeMeta> {
    get_proto(&format!("/node/{}", encode_id(id))).await
}

/// `/search?q=…` — importer-schema-driven search over the hosted graph.
#[allow(dead_code)] // the node browser uses search_rich; kept for wire parity with the egui client
pub async fn search(q: &str, limit: u32) -> ApiResult<proto::SearchResults> {
    get_proto(&format!(
        "/search?q={}&limit={limit}",
        urlencoding::encode(q)
    ))
    .await
}

/// One `/search/rich` hit. `snippet` is server-built HTML: the matched body
/// region with `<b>` around hits (Tantivy SnippetGenerator output — source
/// text is escaped server-side, the only markup is the highlight tags).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RichHit {
    pub id: String,
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub snippet: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
pub struct RichResults {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub results: Vec<RichHit>,
}

/// `/search/rich?q=…` — importer-schema-driven search with snippets from
/// fields the importer explicitly marks as snippet-capable.
pub async fn search_rich(q: &str, limit: u32) -> ApiResult<RichResults> {
    let path = format!("/search/rich?q={}&limit={limit}", urlencoding::encode(q));
    let resp = get(&path).send().await.map_err(err)?;
    if !resp.ok() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            format!("{path} -> HTTP {status}")
        } else {
            format!("{path} -> HTTP {status}: {detail}")
        });
    }
    resp.json().await.map_err(err)
}

/// Minimal `/graph/schema` view used by the Nodes panel. Serde deliberately
/// ignores the rest of the importer contract: this UI only needs the source
/// identity and the fields callers may use in a search query.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GraphSchema {
    #[serde(default)]
    pub graph_revision: u64,
    pub source: ImporterSource,
    pub schema: DiscoverySchema,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ImporterSource {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
pub struct DiscoverySchema {
    #[serde(default)]
    pub fields: Vec<DiscoveryField>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DiscoveryField {
    pub key: String,
    #[serde(default)]
    pub searchable: bool,
    #[serde(default)]
    pub facetable: bool,
}

/// `/graph/schema` — the active importer's versioned discovery contract.
pub async fn graph_schema() -> ApiResult<GraphSchema> {
    get_json("/graph/schema").await
}

// --- importer deployment catalog ---------------------------------------------

/// Sanitized, deployment-owned importer catalog exposed by graph-api. The
/// endpoint is deliberately read-only: changing the selected source replaces
/// the process-lifetime importer and watcher, so Helm remains the activation
/// authority and a rollout performs the switch.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImporterCatalog {
    pub activation: String,
    #[serde(default)]
    pub selected: Option<String>,
    pub active: ActiveImporter,
    #[serde(default)]
    pub sources: Vec<ImporterProfile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ActiveImporter {
    #[serde(default)]
    pub kind: Option<String>,
    pub importer: ImporterSource,
}

impl ActiveImporter {
    pub fn kind_label(&self) -> &str {
        self.kind.as_deref().unwrap_or("unknown/custom")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImporterProfile {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub kind: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub filesystem_rescan_interval_seconds: Option<u64>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub source: Option<ImporterFilesystemSource>,
    #[serde(default)]
    pub producer: Option<ImporterProducer>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImporterFilesystemSource {
    pub volume_name: String,
    pub existing_claim: String,
    pub mount_path: String,
    pub path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImporterProducer {
    pub chart: String,
    pub default_claim: String,
    pub repository_root: String,
    pub workflow_input: String,
    pub existing_claim_value_path: String,
    pub existing_claim_value: String,
}

/// `GET /importers` — deployment-selectable source profiles and the importer
/// currently hosted by this graph-api process.
pub async fn importers() -> ApiResult<ImporterCatalog> {
    get_json("/importers").await
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
    Start {
        id: u64,
        group: String,
        label: String,
    },
    SetProgress {
        id: u64,
        progress: f32,
    },
    UpdateLabel {
        id: u64,
        label: String,
    },
    Finish {
        id: u64,
    },
    Fail {
        id: u64,
        reason: String,
    },
    Log {
        level: LogLevel,
        group: String,
        message: String,
    },
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

#[cfg(test)]
mod tests {
    use super::ImporterCatalog;

    #[test]
    fn importer_catalog_accepts_an_omitted_active_kind() {
        let catalog: ImporterCatalog = serde_json::from_value(serde_json::json!({
            "activation": "helm_rollout",
            "selected": null,
            "active": {
                "importer": {
                    "id": "custom",
                    "name": "Custom importer",
                    "version": "1"
                }
            },
            "sources": []
        }))
        .expect("catalog without an active kind must remain readable");

        assert_eq!(catalog.active.kind, None);
        assert_eq!(catalog.active.kind_label(), "unknown/custom");
    }
}
