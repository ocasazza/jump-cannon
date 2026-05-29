//! Broker-level tests for the ADR-002 remote-engine selection plumbing that
//! backs `PUT /compute/layout` + `GET /compute/engines`.
//!
//! These run WITHOUT a live graph-compute worker. `connect_with` only
//! *validates* the URL synchronously and spawns a background reconnect loop
//! that backs off forever against the (unreachable) address — harmless for the
//! purposes of asserting the broker's bookkeeping (stored selection + the
//! degraded `/compute/engines` view). A full end-to-end reselect that proves
//! frames flip to the new engine needs an in-process worker; see the todo in
//! the task summary and the in-process gRPC pattern in
//! `graph-compute/tests/list_engines_grpc.rs`.

use graph_api::compute_broker::{ComputeBroker, RemoteLayout};

/// A disabled broker (never `connect`ed) reports the contract's graceful
/// degraded shape: `connected:false`, empty `active`, no engines.
#[tokio::test]
async fn list_engines_degrades_when_disabled() {
    let broker = ComputeBroker::new();
    let view = broker.list_engines().await;
    assert!(!view.connected, "disabled broker must report connected:false");
    assert_eq!(view.active, "", "disabled broker has no active engine");
    assert!(view.engines.is_empty(), "disabled broker lists no engines");
}

/// `reselect` against a connected broker (URL configured, even with no live
/// worker) updates the stored selection. Subsequent `/graph/layout/stream`
/// subscribers + the next reconnect read this live value; here we assert the
/// bookkeeping half (the network half needs a worker).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reselect_updates_stored_selection() {
    let broker = ComputeBroker::new();
    // Dial a port nothing is listening on. `connect_with` validates the URL
    // and spawns the (forever-backing-off) forwarder; it does not block on a
    // successful dial, so this returns Ok immediately.
    broker
        .connect_with("http://127.0.0.1:1/".to_string(), RemoteLayout::default())
        .await
        .expect("connect_with validates the url and spawns the loop");

    // Initial selection is the default (empty ⇒ worker default engine).
    assert_eq!(broker.selection().await.layout_id, "");

    // Reselect to a concrete engine + params.
    let params = serde_json::json!({ "gravity": 1.5 });
    broker
        .reselect(RemoteLayout {
            layout_id: "fa2-bh".to_string(),
            params: Some(params.clone()),
        })
        .await
        .expect("reselect against a connected broker succeeds");

    let sel = broker.selection().await;
    assert_eq!(sel.layout_id, "fa2-bh", "reselect must store the new layout_id");
    assert_eq!(sel.params, Some(params), "reselect must store the new params");

    // The degraded engines view still reflects the active selection even when
    // the worker is unreachable (connected:false because the dial fails, but
    // active is the broker's stored id). Per the contract, `active` is the
    // broker's currently-selected layout_id.
    let view = broker.list_engines().await;
    assert!(!view.connected, "no worker listening ⇒ connected:false");
}

/// `reselect` on a never-connected broker is a caller error (no URL to dial).
#[tokio::test]
async fn reselect_errors_without_connect() {
    let broker = ComputeBroker::new();
    let err = broker
        .reselect(RemoteLayout {
            layout_id: "fa2-bh".to_string(),
            params: None,
        })
        .await
        .expect_err("reselect with no configured URL must error");
    assert!(
        err.to_string().contains("not connected"),
        "error should explain the broker isn't connected, got: {err}"
    );
}
