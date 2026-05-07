//! HTTP-level regression tests for the axum router.
//!
//! Each test pins a server-side bug we already paid for once. They drive
//! the `Router` returned by [`graph_api::router`] via tower's `oneshot`,
//! avoiding the need for a real TCP socket / async runtime spin-up.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use prost::Message;
use tower::ServiceExt; // for `oneshot`

use graph_api::proto::NodeMeta;
use graph_api::AppState;
use vault_data::VaultGraph;

/// Build an `AppState` over an empty `VaultGraph`. No vault-search
/// subprocess, no asset dir — enough to exercise the protobuf endpoints.
fn empty_state() -> AppState {
    AppState::new(
        std::path::PathBuf::from("/tmp/jump-cannon-test-empty-vault"),
        VaultGraph::new(),
        None,
        None,
    )
}

/// `/node/<missing-id>` regression: previously returned 404 + a noisy
/// `[graph-renderer]` error log. Now returns a stub `NodeMeta` with
/// `doctype = Some("external")` so the renderer can show *something*.
#[tokio::test]
async fn node_meta_stub_for_missing_id() {
    let state = empty_state();
    let app = graph_api::router(state);

    // The route is `/node/:id`, so the renderer URL-encodes embedded
    // slashes. We do the same here: the *decoded* id is a deep vault
    // path, which is exactly the shape that originally returned 404.
    let decoded_id = "some/deeply/nested/path/Missing.md";
    let encoded_id = decoded_id.replace('/', "%2F");
    let req = Request::builder()
        .uri(format!("/node/{encoded_id}"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.expect("router served");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "missing-id node lookup must not 404 — see KB-404 stub",
    );
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(ct, "application/x-protobuf", "stub must be served as protobuf");

    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    let meta = NodeMeta::decode(bytes.as_ref()).expect("decode NodeMeta");

    assert_eq!(
        meta.doctype.as_deref(),
        Some("external"),
        "stub must mark itself with doctype=\"external\""
    );
    assert!(
        meta.tags.is_empty(),
        "stub must have empty tags; got {:?}",
        meta.tags,
    );
    assert_eq!(meta.pagerank, 0.0, "stub pagerank must be zero");
    assert_eq!(meta.degree, 0, "stub degree must be zero");
    assert_eq!(meta.community, 0, "stub community must be zero");
    assert_eq!(meta.id, decoded_id, "stub id must echo the (decoded) request path");
    // Title = last path segment, folder = everything before. Pin both so
    // a future "smarter" id-splitter doesn't silently drift.
    assert_eq!(meta.title, "Missing.md");
    assert_eq!(meta.folder, "some/deeply/nested/path");
}

/// Belt-and-braces: keep `AppState` constructable from outside the crate.
/// If a future refactor makes `AppState::new` private, this test fails to
/// compile and reminds us to ship a public test-only constructor instead
/// of breaking integration tests silently.
#[allow(dead_code)]
fn _state_constructor_is_public() -> AppState {
    AppState::new(
        std::path::PathBuf::new(),
        VaultGraph::new(),
        None,
        None,
    )
}

// Silence "unused import" if Arc ever becomes unused; keeps the test
// file honest with whatever helpers it actually exercises today.
#[allow(dead_code)]
fn _arc_keepalive() -> Option<Arc<()>> {
    None
}
