use axum::{
    extract::{Path as AxPath, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tantivy::schema::Value as TantivyValue;
use tantivy::TantivyDocument;
use tokio::task;
use tower_http::cors::{Any, CorsLayer};

use crate::error::ApiError;
use crate::index::{
    full_index, incremental_index, lookup_node, read_existing_mtimes, refresh_paths, run_search,
    IndexState, IndexStatus,
};

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 5000;

pub fn router(state: IndexState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health))
        .route("/search", get(search))
        .route("/ids", get(ids))
        .route("/node/:id", get(node))
        .route("/reindex", post(reindex))
        .route("/refresh", post(refresh))
        .layer(cors)
        .with_state(Arc::new(state))
}

async fn health(State(state): State<Arc<IndexState>>) -> impl IntoResponse {
    let status = state.status.read();
    let body = json!({
        "status": status.as_str(),
        "indexed": state.indexed.load(Ordering::SeqCst),
        "total": state.total.load(Ordering::SeqCst),
        "vault": state.vault.display().to_string(),
    });
    (StatusCode::OK, Json(body))
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

async fn search(
    State(state): State<Arc<IndexState>>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    ensure_ready(&state)?;
    if p.q.trim().is_empty() {
        return Err(ApiError::BadRequest("missing q".into()));
    }
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = p.offset.unwrap_or(0);
    let st = state.clone();
    let q = p.q.clone();
    let (total, results) = task::spawn_blocking(move || run_search(&st, &q, limit, offset, true))
        .await
        .map_err(|e| ApiError::Internal(format!("join: {e}")))??;

    let payload: Vec<Value> = results
        .into_iter()
        .map(|(id, score, snip)| {
            json!({
                "id": id,
                "score": score,
                "snippet": snip.unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(json!({ "total": total, "results": payload })))
}

#[derive(Deserialize)]
struct IdsParams {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn ids(
    State(state): State<Arc<IndexState>>,
    Query(p): Query<IdsParams>,
) -> Result<Json<Value>, ApiError> {
    ensure_ready(&state)?;
    if p.q.trim().is_empty() {
        return Err(ApiError::BadRequest("missing q".into()));
    }
    let limit = p.limit.unwrap_or(MAX_LIMIT).min(MAX_LIMIT);
    let st = state.clone();
    let q = p.q.clone();
    let (total, results) = task::spawn_blocking(move || run_search(&st, &q, limit, 0, false))
        .await
        .map_err(|e| ApiError::Internal(format!("join: {e}")))??;
    let ids: Vec<String> = results.into_iter().map(|(id, _, _)| id).collect();
    Ok(Json(json!({ "ids": ids, "total": total })))
}

async fn node(
    State(state): State<Arc<IndexState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<Value>, ApiError> {
    ensure_ready(&state)?;
    let id = urlencoding::decode(&id)
        .map_err(|e| ApiError::BadRequest(format!("bad id encoding: {e}")))?
        .into_owned();
    let st = state.clone();
    let doc_opt = task::spawn_blocking(move || lookup_node(&st, &id))
        .await
        .map_err(|e| ApiError::Internal(format!("join: {e}")))??;
    let Some(doc) = doc_opt else {
        return Err(ApiError::NotFound);
    };
    Ok(Json(doc_to_json(&state, &doc)))
}

#[derive(Deserialize)]
struct RefreshBody {
    paths: Vec<String>,
}

/// Incrementally re-index the given vault-relative paths. For each path:
///   * file exists → re-parse + upsert (delete-by-id-term, add).
///   * file missing → delete-by-id-term only.
///
/// Returns `{ ok: true, updated, deleted, skipped }`. `updated` counts
/// any path that resulted in an `add_document` (added or replaced);
/// `deleted` counts paths whose docs were removed because the file
/// vanished; `skipped` counts unparseable entries.
async fn refresh(
    State(state): State<Arc<IndexState>>,
    Json(body): Json<RefreshBody>,
) -> Result<Json<Value>, ApiError> {
    // Don't gate on Ready — refreshes are safe to interleave with the
    // initial index because they serialize on the writer lock. But the
    // /search and /node endpoints will still surface NotReady if a
    // request lands mid-rebuild.
    let st = state.clone();
    let (updated, deleted, skipped) = task::spawn_blocking(move || {
        let mut writer = st.writer.write();
        let res = refresh_paths(&mut writer, &st.fields, &st.vault, &body.paths);
        if res.is_ok() {
            let _ = writer.commit();
        }
        res
    })
    .await
    .map_err(|e| ApiError::Internal(format!("join: {e}")))?
    .map_err(|e| ApiError::Internal(format!("refresh: {e}")))?;

    if let Err(e) = state.reader.reload() {
        tracing::warn!(error = %e, "reader reload after refresh failed");
    }
    tracing::info!(updated, deleted, skipped, "refresh complete");

    Ok(Json(json!({
        "ok": true,
        "updated": updated,
        "deleted": deleted,
        "skipped": skipped,
    })))
}

async fn reindex(State(state): State<Arc<IndexState>>) -> impl IntoResponse {
    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = task::spawn_blocking(move || trigger_full_reindex(&st)).await {
            tracing::error!(error = %e, "reindex join failed");
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "status": "reindexing" })))
}

fn ensure_ready(state: &IndexState) -> Result<(), ApiError> {
    let status = state.status.read();
    if *status == IndexStatus::Ready {
        Ok(())
    } else {
        Err(ApiError::NotReady)
    }
}

fn doc_to_json(state: &IndexState, doc: &TantivyDocument) -> Value {
    let id = doc
        .get_first(state.fields.id)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = doc
        .get_first(state.fields.title)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mtime = doc
        .get_first(state.fields.mtime)
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tags: Vec<String> = doc
        .get_all(state.fields.tags)
        .filter_map(|v| v.as_str())
        .map(|s| s.to_string())
        .collect();
    json!({
        "id": id,
        "title": title,
        "tags": tags,
        "mtime": mtime,
    })
}

/// Synchronous full reindex driver — called from POST /reindex via spawn_blocking.
pub fn trigger_full_reindex(state: &IndexState) -> anyhow::Result<()> {
    *state.status.write() = IndexStatus::Indexing;
    state.indexed.store(0, Ordering::SeqCst);
    let mut writer = state.writer.write();
    let count = full_index(
        &mut writer,
        &state.fields,
        &state.vault,
        state.include_hippo,
        &state.indexed,
        &state.total,
    )?;
    writer.commit()?;
    drop(writer);
    state.reader.reload()?;
    state.indexed.store(count, Ordering::SeqCst);
    *state.status.write() = IndexStatus::Ready;
    tracing::info!(count, "full reindex complete");
    Ok(())
}

/// Sync wrapper around the incremental path used at startup.
pub fn run_startup_index(state: &IndexState, rebuild: bool) -> anyhow::Result<()> {
    *state.status.write() = IndexStatus::Indexing;
    state.indexed.store(0, Ordering::SeqCst);

    if rebuild {
        let mut writer = state.writer.write();
        let count = full_index(
            &mut writer,
            &state.fields,
            &state.vault,
            state.include_hippo,
            &state.indexed,
            &state.total,
        )?;
        writer.commit()?;
        drop(writer);
        state.reader.reload()?;
        state.indexed.store(count, Ordering::SeqCst);
        *state.status.write() = IndexStatus::Ready;
        tracing::info!(count, "rebuild complete");
        return Ok(());
    }

    let existing = read_existing_mtimes(&state.index, &state.fields)?;
    if existing.is_empty() {
        // Fresh index — same as rebuild.
        let mut writer = state.writer.write();
        let count = full_index(
            &mut writer,
            &state.fields,
            &state.vault,
            state.include_hippo,
            &state.indexed,
            &state.total,
        )?;
        writer.commit()?;
        drop(writer);
        state.reader.reload()?;
        state.indexed.store(count, Ordering::SeqCst);
        *state.status.write() = IndexStatus::Ready;
        tracing::info!(count, "initial index complete");
        return Ok(());
    }

    let mut writer = state.writer.write();
    let (added, updated, removed) = incremental_index(
        &mut writer,
        &state.fields,
        &existing,
        &state.vault,
        state.include_hippo,
        &state.indexed,
        &state.total,
    )?;
    writer.commit()?;
    drop(writer);
    state.reader.reload()?;
    let total_after = state.total.load(Ordering::SeqCst);
    state.indexed.store(total_after, Ordering::SeqCst);
    *state.status.write() = IndexStatus::Ready;
    tracing::info!(added, updated, removed, total = total_after, "incremental reindex complete");
    Ok(())
}
