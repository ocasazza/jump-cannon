//! HTTP API. axum router + route handlers.
//
// Future: when split across machines, this server runs on luna; the renderer
// (graph-renderer) is served from any static host and points its fetch URLs
// at this server via a --backend-url flag (not yet implemented).

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::sync::broadcast::error::RecvError;
use prost::Message as ProstMessage;
use serde::Deserialize;

use crate::{proto, state::AppState};
use vault_data::color::PALETTE;

const PROTOBUF_CT: &str = "application/x-protobuf";
const OCTET_CT: &str = "application/octet-stream";

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/*path", get(asset))
        .route("/graph/init", get(graph_init))
        .route("/graph/ids", get(graph_ids))
        .route("/graph/positions", get(graph_positions))
        .route("/graph/edges", get(graph_edges))
        .route("/graph/metrics/:name", get(graph_metric))
        .route("/graph/meta_summary", get(graph_meta_summary))
        .route("/graph/layout/stream", get(graph_layout_stream))
        .route("/node/:id", get(node_meta))
        .route("/search", get(search))
        .with_state(state)
}

async fn index(State(s): State<AppState>) -> impl IntoResponse {
    asset_response(&s, "index.html")
}

async fn asset(State(s): State<AppState>, Path(path): Path<String>) -> impl IntoResponse {
    asset_response(&s, &path)
}

/// Dev mode (assets_dir set): read from disk every request — refresh browser
/// to see JS/CSS/HTML edits without rebuild.
/// Release mode (assets_dir None): serve from the include_dir!() embedded bundle.
fn asset_response(s: &AppState, path: &str) -> axum::response::Response {
    let mime = mime_for(path);
    if let Some(dir) = &s.inner.assets_dir {
        let full = dir.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
                (StatusCode::OK, headers, bytes).into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, format!("not found: {}", full.display()))
                .into_response(),
        }
    } else {
        match graph_renderer::assets().get_file(path) {
            Some(file) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
                (StatusCode::OK, headers, file.contents().to_vec()).into_response()
            }
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html"  => "text/html; charset=utf-8",
        "js"    => "application/javascript",
        "wasm"  => "application/wasm",
        "css"   => "text/css",
        "json"  => "application/json",
        "proto" => "text/plain; charset=utf-8",
        "png"   => "image/png",
        "svg"   => "image/svg+xml",
        _       => "application/octet-stream",
    }
}

// --- Protobuf endpoints ---

/// JSON list of node ids in the same order as the `/graph/positions`,
/// `/graph/edges`, and `/graph/metrics/*` binary buffers. Lets the renderer
/// hand server-side string ids (vault paths) directly to Cosmograph and back
/// to `/node/:id` without a translation step.
async fn graph_ids(State(s): State<AppState>) -> impl IntoResponse {
    use axum::Json;
    Json(s.inner.idx_to_id.clone())
}

async fn graph_init(State(s): State<AppState>) -> impl IntoResponse {
    let g = &s.inner.graph;
    let palette: Vec<f32> = PALETTE.iter().flat_map(|rgb| rgb.iter().copied()).collect();
    let msg = proto::Init {
        n_nodes: g.node_count() as u32,
        n_edges: g.edge_count() as u32,
        num_communities: g.num_communities as u32,
        num_wcc: g.num_wcc as u32,
        palette,
    };
    proto_response(&msg)
}

/// `/node/:id` lookup.
///
/// Primary path: serve from the in-memory `VaultGraph`. Many ids the renderer
/// renders (especially under `shared/knowledge-base/_ingested/...`) no longer
/// live in the in-memory graph because the ingest pipeline migrated those
/// documents into a Prisma-managed SQLite database. Returning 404 for those
/// caused a noisy stream of console errors and an empty modal.
///
/// Stub fallback: when the id is not in the graph, return a *minimal*
/// `NodeMeta` populated from the id alone (title = last path segment,
/// `doctype = "external"`). The renderer treats `doctype = "external"` as a
/// stub marker so it can render *something* useful instead of an error.
///
/// TODO(prisma): wire up an optional Prisma lookup *before* falling back to
/// the stub. Concrete next steps:
///   - Schema lives at:
///       `~/Repositories/schrodinger/nixstation/projects/ingest/prisma/schema.prisma`
///     (model `ObsidianDocument`, keyed on `vaultPath`).
///   - The dev SQLite file is at:
///       `~/Repositories/schrodinger/nixstation/projects/ingest/prisma/_dev.db`
///     Production likely uses the same `DATABASE_URL` env var convention as
///     the rest of the ingest pipeline.
///   - Add a `--prisma-url <sqlite-url>` flag (and matching `Prisma` field on
///     `AppStateInner`, populated at boot only when the flag is set) — keep
///     this *optional* so the existing in-memory-only path keeps working.
///   - Probably easiest to use `sqlx` (raw SQL against the small set of
///     columns we need: `vault_path`, `title`, `lifecycle`, plus the per-doc
///     join into `obsidian_documents_tags` for tags). `prisma-client-rust`
///     is the "match the schema" option but adds a build-time codegen step.
///   - On hit, populate `NodeMeta` with the real title/folder/tags from the
///     row and leave the metric fields at zero (no PageRank for nodes that
///     aren't in the layout graph).
async fn node_meta(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    if let Some(node) = s.inner.graph.nodes.get(&id) {
        let frontmatter_json = serde_json::to_string(&node.meta.frontmatter)
            .unwrap_or_else(|_| "{}".into());
        let msg = proto::NodeMeta {
            id: id.clone(),
            title: node.meta.title.clone(),
            path: node.meta.path.clone(),
            folder: node.meta.folder.clone(),
            doctype: node.meta.doctype.clone(),
            tags: node.meta.tags.clone(),
            frontmatter_json,
            degree: node.metrics.degree as u32,
            indegree: node.metrics.indegree as u32,
            outdegree: node.metrics.outdegree as u32,
            pagerank: node.metrics.pagerank as f32,
            betweenness: node.metrics.betweenness as f32,
            kcore: node.metrics.kcore as u32,
            community: node.metrics.community as u32,
            wcc: node.metrics.wcc as u32,
        };
        return proto_response(&msg).into_response();
    }

    // Stub fallback. Best-effort title from the last path segment; folder is
    // everything before it. `doctype = "external"` is the stub marker.
    let (folder, title) = match id.rsplit_once('/') {
        Some((f, t)) => (f.to_string(), t.to_string()),
        None => (String::new(), id.clone()),
    };
    let msg = proto::NodeMeta {
        id: id.clone(),
        title,
        path: id.clone(),
        folder,
        doctype: Some("external".into()),
        tags: Vec::new(),
        frontmatter_json: "{}".into(),
        degree: 0,
        indegree: 0,
        outdegree: 0,
        pagerank: 0.0,
        betweenness: 0.0,
        kcore: 0,
        community: 0,
        wcc: 0,
    };
    proto_response(&msg).into_response()
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default)]
    limit: Option<u32>,
}

async fn search(State(s): State<AppState>, Query(p): Query<SearchParams>) -> impl IntoResponse {
    let limit = p.limit.unwrap_or(50);
    // Fast path: proxy to vault-search's /ids endpoint (Tantivy BM25).
    if let Some(vs) = &s.inner.vault_search {
        let url = format!(
            "{}/ids?q={}&limit={}",
            vs.url(),
            urlencoding::encode(&p.q),
            limit
        );
        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, format!("vault-search proxy: {e}"))
                    .into_response()
            }
        };
        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, format!("vault-search decode: {e}"))
                    .into_response()
            }
        };
        // vault-search shape: {"ids": [...], "total": N}
        let ids: Vec<String> = json["ids"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let total = json["total"].as_u64().unwrap_or(ids.len() as u64) as u32;
        let msg = proto::SearchResults { total, ids };
        return proto_response(&msg).into_response();
    }

    // Fallback: naive title-contains scan when vault-search isn't running.
    let q = p.q.to_lowercase();
    let mut ids: Vec<String> = s
        .inner
        .graph
        .nodes
        .iter()
        .filter(|(_, n)| n.meta.title.to_lowercase().contains(&q))
        .map(|(id, _)| id.clone())
        .take(limit as usize)
        .collect();
    ids.sort();
    let msg = proto::SearchResults {
        total: ids.len() as u32,
        ids,
    };
    proto_response(&msg).into_response()
}

fn proto_response<M: ProstMessage>(msg: &M) -> impl IntoResponse {
    let mut buf = Vec::with_capacity(msg.encoded_len());
    msg.encode(&mut buf).expect("encode");
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(PROTOBUF_CT));
    (StatusCode::OK, headers, buf)
}

// --- Binary endpoints (raw little-endian buffers for hot-path bulk data) ---

async fn graph_positions(State(s): State<AppState>) -> impl IntoResponse {
    cached_binary_response(&s, "positions").into_response()
}

async fn graph_edges(State(s): State<AppState>) -> impl IntoResponse {
    cached_binary_response(&s, "edges").into_response()
}

async fn graph_metric(State(s): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    cached_binary_response(&s, &name).into_response()
}

/// Returns a per-field inverted index covering the small handful of
/// fields the renderer-side chip / badge UI cares about. Built once per
/// process and cached as `Arc<[u8]>` in `binary_cache` under the
/// reserved key "meta_summary".
async fn graph_meta_summary(State(s): State<AppState>) -> impl IntoResponse {
    if let Some(buf) = s.inner.binary_cache.get("meta_summary").cloned() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(PROTOBUF_CT));
        return (StatusCode::OK, headers, buf.to_vec()).into_response();
    }
    let bytes = build_meta_summary_bytes(&s.inner.graph);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(PROTOBUF_CT));
    (StatusCode::OK, headers, bytes).into_response()
}

/// Walk the graph once and build a [`proto::MetaSummary`]. The frontend
/// uses this CSR-style payload (sorted node-idx vecs per (field, value)
/// bucket) to compute filter intersections without per-click round-trips.
pub fn build_meta_summary_bytes(graph: &vault_data::VaultGraph) -> Vec<u8> {
    use std::collections::BTreeMap;
    use serde_json::Value;

    // Field name -> value -> sorted Vec<node_idx>.
    let mut idx: BTreeMap<String, BTreeMap<String, Vec<u32>>> = BTreeMap::new();
    fn push(idx: &mut BTreeMap<String, BTreeMap<String, Vec<u32>>>,
            field: &str, value: &str, node: u32) {
        let v = value.trim();
        if v.is_empty() { return; }
        idx.entry(field.to_string())
            .or_default()
            .entry(v.to_string())
            .or_default()
            .push(node);
    }

    for (i, (_id, node)) in graph.nodes.iter().enumerate() {
        let ni = i as u32;
        for t in &node.meta.tags {
            push(&mut idx,"tags", t, ni);
        }
        if let Some(dt) = &node.meta.doctype {
            push(&mut idx,"doctype", dt, ni);
        }
        push(&mut idx,"folder", &node.meta.folder, ni);
        let fm = &node.meta.frontmatter;
        // status — usually a scalar string.
        if let Some(Value::String(v)) = fm.get("status") {
            push(&mut idx,"status", v, ni);
        }
        // authors — comma-split string OR array.
        if let Some(v) = fm.get("authors") {
            for s in extract_strings(v) {
                for part in s.split(',') {
                    push(&mut idx,"authors", part, ni);
                }
            }
        }
        if let Some(v) = fm.get("entities") {
            for s in extract_strings(v) {
                push(&mut idx,"entities", &s, ni);
            }
        }
        if let Some(v) = fm.get("key_topics") {
            for s in extract_strings(v) {
                push(&mut idx,"key_topics", &s, ni);
            }
        }
        // related — wikilinks; strip the [[ ]] wrapper, split on |.
        if let Some(v) = fm.get("related") {
            for s in extract_strings(v) {
                let t = s.trim();
                let inner = t.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")).unwrap_or(t);
                let target = inner.split_once('|').map(|(p, _)| p).unwrap_or(inner).trim();
                push(&mut idx,"related", target, ni);
            }
        }
    }

    let mut fields: Vec<String> = idx.keys().cloned().collect();
    fields.sort();
    let field_to_idx: std::collections::HashMap<&String, u32> = fields
        .iter()
        .enumerate()
        .map(|(i, n)| (n, i as u32))
        .collect();
    let mut buckets: Vec<proto::FieldBucket> = Vec::new();
    for (field, vmap) in &idx {
        let fi = *field_to_idx.get(field).unwrap();
        for (value, mut node_idx) in vmap.iter().map(|(k, v)| (k.clone(), v.clone())) {
            node_idx.sort_unstable();
            node_idx.dedup();
            buckets.push(proto::FieldBucket {
                field_idx: fi,
                value,
                node_idx,
            });
        }
    }
    let msg = proto::MetaSummary { fields, buckets };
    let mut buf = Vec::with_capacity(msg.encoded_len());
    msg.encode(&mut buf).expect("encode meta_summary");
    buf
}

fn extract_strings(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Look up a precomputed buffer in `AppState::binary_cache` and serve it
/// with `Cache-Control: max-age=…, immutable`. Bytes are an `Arc<[u8]>`
/// so the response shares the buffer with the cache — no copy. The
/// graph is immutable for the server's lifetime, so a long max-age is
/// safe; `immutable` tells the browser not to revalidate on refresh.
// --- Layout streaming (Phase 1 of the distributed-compute plan) -------------
//
// Subscribes to the configured graph-compute worker and forwards every
// PositionDelta to the WebSocket as a single binary frame:
//
//   [u64 LE frame number][u32 LE n_nodes][raw f32 LE positions...]
//
// The WASM client reuses the same `positions` byte layout that
// `/graph/positions` already serves, so the renderer's existing buffer-update
// path can ingest these frames once a stream-consumer lands. WebTransport
// upgrade is a Phase 4 deliverable; WebSocket is the contained Phase 1 choice.
async fn graph_layout_stream(
    State(s): State<AppState>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    let Some(rx) = s.inner.compute_broker.subscribe().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "compute worker not connected",
        )
            .into_response();
    };
    ws.on_upgrade(move |socket| layout_stream_loop(socket, rx))
}

async fn layout_stream_loop(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<graph_compute::proto::PositionDelta>,
) {
    loop {
        let frame = match rx.recv().await {
            Ok(f) => f,
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "ws subscriber lagged");
                continue;
            }
            Err(RecvError::Closed) => break,
        };
        let mut buf =
            Vec::with_capacity(8 + 4 + frame.positions.len());
        buf.extend_from_slice(&frame.frame.to_le_bytes());
        buf.extend_from_slice(&frame.n_nodes.to_le_bytes());
        buf.extend_from_slice(&frame.positions);
        if socket.send(Message::Binary(buf)).await.is_err() {
            break;
        }
    }
}

fn cached_binary_response(s: &AppState, key: &str) -> axum::response::Response {
    let Some(buf) = s.inner.binary_cache.get(key).cloned() else {
        return (StatusCode::NOT_FOUND, "unknown buffer").into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(OCTET_CT));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    (StatusCode::OK, headers, buf.to_vec()).into_response()
}
