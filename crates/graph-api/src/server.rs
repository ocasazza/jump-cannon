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
    routing::{get, put},
    Json, Router,
};
use tokio::sync::broadcast::error::RecvError;
use prost::Message as ProstMessage;
use serde::{Deserialize, Serialize};

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
        .route("/graph/csr.bin", get(graph_csr_bin))
        .route("/graph/metrics/:name", get(graph_metric))
        .route("/graph/meta_summary", get(graph_meta_summary))
        .route("/graph/layout/stream", get(graph_layout_stream))
        .route("/node/:id", get(node_meta))
        .route("/search", get(search))
        .route("/compute/health", get(compute_health))
        .route("/vault/page", put(vault_page_put))
        .with_state(state)
}

// --- Vault write endpoint ---
//
// PUT /vault/page  Body: { "path": "...", "body": "..." }
//
// `path` follows the vault-links convention (relative, no `.md` extension,
// matching `meta.path` in NodeMeta). `body` is the *body-only* markdown —
// the on-disk YAML frontmatter block (if any) is preserved verbatim. This
// keeps the editor's source surface focused on prose while frontmatter
// editing continues to flow through the chip strip surface. See the
// `page_viewer` module on the renderer side for the matching client.
//
// SECURITY: no authentication. The graph-api server is a local-dev tool
// (the README only documents `127.0.0.1` binding and no auth on /search,
// /node/:id, etc.). Adding auth here without doing it everywhere would be
// security theatre. The path-traversal guard below is the only line of
// defence; if this server ever binds to a non-loopback address, the
// auth story must be revisited *for every endpoint*, not just this one.

/// Cap on accepted write body. 5 MiB is well above any sane Obsidian
/// note (~50 KiB typical) but bounded enough that a runaway client
/// can't OOM the server.
const MAX_PAGE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
struct VaultPagePutReq {
    /// vault-links id convention: relative path without `.md` extension.
    path: String,
    /// New body content (frontmatter is NOT part of this — the on-disk
    /// frontmatter block is preserved verbatim).
    body: String,
}

#[derive(Serialize)]
struct VaultPagePutResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn put_err(status: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    let msg = msg.into();
    tracing::warn!(error = %msg, "vault page put rejected");
    (
        status,
        Json(VaultPagePutResp { ok: false, error: Some(msg) }),
    )
        .into_response()
}

/// Validate the relative `path` before joining with the vault root.
///
/// Rejects:
///   - empty / whitespace-only
///   - absolute paths (`/foo`)
///   - any component that is `..` or contains a NUL byte
///   - windows drive letters (`C:\…`)
///
/// On success returns the absolute file path (`<vault_root>/<path>.md`).
/// Canonicalization is deferred to the caller — we canonicalize the
/// parent directory (which must exist) and verify the result still
/// starts with the canonicalized vault root.
fn resolve_vault_path(
    vault_root: &std::path::Path,
    rel: &str,
) -> Result<std::path::PathBuf, String> {
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return Err("path is empty".into());
    }
    if trimmed.contains('\0') {
        return Err("path contains NUL byte".into());
    }
    let p = std::path::Path::new(trimmed);
    if p.is_absolute() {
        return Err("path must be relative".into());
    }
    for comp in p.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => return Err("path contains `..`".into()),
            Component::Prefix(_) | Component::RootDir => {
                return Err("path must be relative".into());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(vault_root.join(format!("{trimmed}.md")))
}

async fn vault_page_put(
    State(s): State<AppState>,
    Json(req): Json<VaultPagePutReq>,
) -> axum::response::Response {
    if req.body.len() > MAX_PAGE_BYTES {
        return put_err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body exceeds {MAX_PAGE_BYTES} bytes"),
        );
    }
    let vault_root = s.inner.vault_root.clone();
    let abs = match resolve_vault_path(&vault_root, &req.path) {
        Ok(p) => p,
        Err(e) => return put_err(StatusCode::BAD_REQUEST, e),
    };

    // Canonicalize the parent directory (it must exist for the path to
    // be a real vault page) and re-verify the prefix. Canonicalizing
    // the file itself would fail when writing to a brand-new path, but
    // for this endpoint the file must already exist (we don't create
    // new pages — there's no graph-side affordance for that yet).
    let canonical_root = match tokio::fs::canonicalize(&vault_root).await {
        Ok(p) => p,
        Err(e) => return put_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("canonicalize vault_root: {e}"),
        ),
    };
    let parent = match abs.parent() {
        Some(p) => p.to_path_buf(),
        None => return put_err(StatusCode::BAD_REQUEST, "path has no parent"),
    };
    let canonical_parent = match tokio::fs::canonicalize(&parent).await {
        Ok(p) => p,
        Err(e) => return put_err(
            StatusCode::BAD_REQUEST,
            format!("parent dir not found: {e}"),
        ),
    };
    if !canonical_parent.starts_with(&canonical_root) {
        return put_err(
            StatusCode::BAD_REQUEST,
            "resolved path escapes vault root",
        );
    }
    let final_path = canonical_parent.join(
        abs.file_name().ok_or_else(|| "missing filename").unwrap_or_default(),
    );

    // Read the existing file to preserve its YAML frontmatter block.
    // The wire format is *body only*; the on-disk file keeps whatever
    // frontmatter was already there. If the file is missing, we refuse
    // — page creation is not supported here.
    let existing = match tokio::fs::read_to_string(&final_path).await {
        Ok(s) => s,
        Err(e) => return put_err(
            StatusCode::NOT_FOUND,
            format!("read {}: {e}", final_path.display()),
        ),
    };
    let frontmatter_block = extract_frontmatter_block(&existing);

    let mut new_contents = String::with_capacity(
        frontmatter_block.len() + req.body.len() + 1,
    );
    new_contents.push_str(&frontmatter_block);
    // Ensure a single newline between frontmatter and body when a
    // frontmatter block exists and the new body doesn't already start
    // with one. When there's no frontmatter, just write the body
    // verbatim.
    if !frontmatter_block.is_empty() && !req.body.starts_with('\n') {
        new_contents.push('\n');
    }
    new_contents.push_str(&req.body);

    // Atomic write: write to `<file>.tmp` (same dir, so rename is atomic
    // on the same filesystem), then rename over the destination. If the
    // tmp write fails halfway through, the original file is untouched.
    let tmp_path = {
        let mut t = final_path.clone();
        let name = final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("page");
        t.set_file_name(format!(".{name}.tmp"));
        t
    };
    if let Err(e) = tokio::fs::write(&tmp_path, new_contents.as_bytes()).await {
        return put_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("write tmp {}: {e}", tmp_path.display()),
        );
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        // Best-effort cleanup of the tmp file so we don't leave detritus.
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return put_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("rename tmp -> {}: {e}", final_path.display()),
        );
    }
    tracing::info!(
        path = %final_path.display(),
        bytes = new_contents.len(),
        "vault page saved",
    );
    (
        StatusCode::OK,
        Json(VaultPagePutResp { ok: true, error: None }),
    )
        .into_response()
}

/// Return the raw YAML frontmatter block (`---\n…---\n`) from `text`, or
/// the empty string when there is no leading frontmatter. The trailing
/// newline after the closing `---` IS included so callers can splice the
/// body directly after.
fn extract_frontmatter_block(text: &str) -> String {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(_) => text,
        None => return String::new(),
    };
    let after_open = rest.strip_prefix("---\n")
        .or_else(|| rest.strip_prefix("---\r\n"))
        .unwrap_or(rest);
    let consumed = rest.len() - after_open.len();
    if let Some(end) = find_fm_close(after_open) {
        // `end` is the byte offset (inside after_open) of the closing
        // `---` line. Include the closing marker + its trailing newline.
        let mut block_end = consumed + end + 3; // `---`
        // Account for the newline (or CRLF) after the closing fence.
        let tail = &rest[block_end..];
        if tail.starts_with("\r\n") {
            block_end += 2;
        } else if tail.starts_with('\n') {
            block_end += 1;
        }
        rest[..block_end].to_string()
    } else {
        String::new()
    }
}

/// Live status of the gRPC link to the `graph-compute` worker. The
/// renderer polls this to surface back-half liveness in the footer log
/// (the renderer's WS to *this* server stays connected even when the
/// downstream gRPC stream is dead, so without this signal a stalled
/// canvas reads as a frontend bug).
async fn compute_health(State(s): State<AppState>) -> impl IntoResponse {
    let status = s.inner.compute_broker.status().await;
    axum::Json(status)
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
        // Read the full markdown body from disk. We don't pre-load
        // every body into the in-memory graph (a 10k-node vault is
        // ~50MB of text), so this is a lazy per-request file read.
        // Strip the YAML frontmatter so the renderer doesn't render it
        // twice — it's already in `frontmatter_json`.
        let body = read_body(&s.inner.vault_root, &node.meta.path);
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
            body,
        };
        return proto_response(&msg).into_response();
    }

    // Filesystem fallback: ids in the search index / Prisma DB but not in
    // the in-memory `VaultGraph` may still map to a real markdown file
    // under `vault_root`. Treat the leading `vault/` as a namespace prefix
    // and look for `<stripped>.md` on disk. Emits a real vault page
    // (doctype = None) rather than the `external` stub so the renderer's
    // editor opens normally.
    let stripped = id.strip_prefix("vault/").unwrap_or(&id);
    let candidate = s.inner.vault_root.join(format!("{stripped}.md"));
    if candidate.is_file() {
        // Path-safety: canonicalize both sides and ensure the resolved
        // file stays inside the vault root. Defeats `..` escapes.
        let safe = match (candidate.canonicalize(), s.inner.vault_root.canonicalize()) {
            (Ok(c), Ok(root)) => c.starts_with(&root),
            _ => false,
        };
        if safe {
            let body = read_body(&s.inner.vault_root, stripped);
            let (folder, title) = match stripped.rsplit_once('/') {
                Some((f, t)) => (f.to_string(), t.to_string()),
                None => (String::new(), stripped.to_string()),
            };
            let msg = proto::NodeMeta {
                id: id.clone(),
                title,
                path: format!("{stripped}.md"),
                folder,
                // Not "external" — this is a real vault page that just
                // isn't in the layout graph.
                doctype: None,
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
                body,
            };
            return proto_response(&msg).into_response();
        }
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
        // External nodes have no corresponding markdown file in the
        // vault, so no body is available. The renderer falls back to
        // displaying just metadata.
        body: String::new(),
    };
    proto_response(&msg).into_response()
}

/// Lazily read a note's markdown body from disk. `path_id` is the
/// vault-links convention (relative path *without* `.md` extension).
/// Frontmatter is stripped — the renderer already has it via
/// `frontmatter_json`. Returns the empty string on any failure (missing
/// file, IO error) rather than propagating an error: the caller treats
/// "no body" as a soft fallback.
fn read_body(vault_root: &std::path::Path, path_id: &str) -> String {
    let abs = vault_root.join(format!("{path_id}.md"));
    let raw = match std::fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(path = %abs.display(), error = %e, "body read failed");
            return String::new();
        }
    };
    // Strip leading `---\n…\n---` YAML frontmatter. Same shape as the
    // splitter in vault-links/src/parser.rs but inline here so we don't
    // pull in that crate just for the strip.
    let rest = match raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return raw,
    };
    if let Some(end) = find_fm_close(rest) {
        let after = &rest[end..];
        let after = after
            .strip_prefix("---\n")
            .or_else(|| after.strip_prefix("---\r\n"))
            .or_else(|| after.strip_prefix("---"))
            .unwrap_or(after);
        return after.trim_start_matches('\n').to_string();
    }
    raw
}

fn find_fm_close(s: &str) -> Option<usize> {
    let mut start = 0;
    while start < s.len() {
        let line_end = s[start..].find('\n').map(|i| start + i).unwrap_or(s.len());
        let line = s[start..line_end].trim_end_matches('\r');
        if line == "---" {
            return Some(start);
        }
        start = line_end + 1;
    }
    None
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

/// Symmetrized CSR adjacency export consumed by `graph-compute` (and SkyPilot
/// `file_mounts` pre-launch). Format is little-endian:
///
/// ```text
/// [u32 n_nodes][u32 n_edges][u32 × (n_nodes+1) offsets][u32 × n_edges neighbors]
/// ```
///
/// This is the on-disk format `graph_compute::sim::CsrGraph::load_bin` parses.
/// Built fresh per request for now; if hot, fold into `binary_cache` like
/// `/graph/edges`. Returns 503 if the in-memory graph hasn't loaded yet
/// (mirrors `/graph/layout/stream`'s 503 pattern).
async fn graph_csr_bin(State(s): State<AppState>) -> axum::response::Response {
    let graph = &s.inner.graph;
    if graph.nodes.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "graph not loaded",
        )
            .into_response();
    }
    let bytes = build_csr_bin(&s);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(OCTET_CT));
    (StatusCode::OK, headers, bytes).into_response()
}

/// Build the CSR byte buffer for `/graph/csr.bin`. Symmetrizes the edge list
/// (force-sim neighbor lookup is undirected) using the same dense indexing
/// `id_to_idx` already provides for the other binary endpoints.
fn build_csr_bin(s: &AppState) -> Vec<u8> {
    let n_nodes = s.inner.graph.nodes.len() as u32;
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n_nodes as usize];
    for edge in &s.inner.graph.edges {
        let (Some(&src), Some(&tgt)) = (
            s.inner.id_to_idx.get(&edge.source),
            s.inner.id_to_idx.get(&edge.target),
        ) else {
            continue;
        };
        if src == tgt {
            continue;
        }
        adj[src as usize].push(tgt);
        adj[tgt as usize].push(src);
    }
    let mut offsets: Vec<u32> = Vec::with_capacity(n_nodes as usize + 1);
    let mut neighbors: Vec<u32> = Vec::new();
    for bucket in &adj {
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(bucket);
    }
    offsets.push(neighbors.len() as u32);
    let n_edges = neighbors.len() as u32;
    let mut out = Vec::with_capacity(8 + 4 * offsets.len() + 4 * neighbors.len());
    out.extend_from_slice(&n_nodes.to_le_bytes());
    out.extend_from_slice(&n_edges.to_le_bytes());
    for v in &offsets {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &neighbors {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
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
