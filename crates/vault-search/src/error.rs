use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

/// Errors surfaced from HTTP handlers. Anything in here is an "expected"
/// failure mode that gets turned into a JSON error body with an appropriate
/// status code; unexpected panics still bubble through axum's default
/// handling.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,

    #[error("indexer not ready")]
    NotReady,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::NotReady => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        let body = Json(json!({ "error": msg }));
        (status, body).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<tantivy::TantivyError> for ApiError {
    fn from(e: tantivy::TantivyError) -> Self {
        ApiError::Internal(format!("tantivy: {e}"))
    }
}
