//! HTTP API. axum router + route handlers.
//
// Future: when split across machines, this server runs on luna; the renderer
// (graph-renderer) is served from any static host and points its fetch URLs
// at this server via a --backend-url flag (not yet implemented).

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use prost::Message;
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

async fn node_meta(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let Some(node) = s.inner.graph.nodes.get(&id) else {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    };
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

fn proto_response<M: Message>(msg: &M) -> impl IntoResponse {
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

/// Look up a precomputed buffer in `AppState::binary_cache` and serve it
/// with `Cache-Control: max-age=…, immutable`. Bytes are an `Arc<[u8]>`
/// so the response shares the buffer with the cache — no copy. The
/// graph is immutable for the server's lifetime, so a long max-age is
/// safe; `immutable` tells the browser not to revalidate on refresh.
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
