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

use graph_api::compute_broker::{
    CapabilityDimensionView, ComputeBroker, EngineManifestView, ManifestUnavailable, RemoteLayout,
};

/// A disabled broker (never `connect`ed) reports the contract's graceful
/// degraded shape: `connected:false`, empty `active`, no engines.
#[tokio::test]
async fn list_engines_degrades_when_disabled() {
    let broker = ComputeBroker::new();
    let view = broker.list_engines().await;
    assert!(
        !view.connected,
        "disabled broker must report connected:false"
    );
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
    assert_eq!(broker.selection_state().await.generation, 1);

    // Reselect to a concrete engine + params.
    let params = serde_json::json!({ "gravity": 1.5 });
    let update = broker
        .reselect(
            RemoteLayout {
                layout_id: "fa2-bh".to_string(),
                params: Some(params.clone()),
                ..Default::default()
            },
            Some(1),
        )
        .await
        .expect("reselect against a connected broker succeeds");
    assert_eq!(update.generation, 2);
    assert!(update.changed);

    let sel = broker.selection().await;
    assert_eq!(
        sel.layout_id, "fa2-bh",
        "reselect must store the new layout_id"
    );
    assert_eq!(
        sel.params,
        Some(params.clone()),
        "reselect must store the new params"
    );

    // The degraded engines view still reflects the active selection even when
    // the worker is unreachable (connected:false because the dial fails, but
    // active is the broker's stored id). Per the contract, `active` is the
    // broker's currently-selected layout_id.
    let view = broker.list_engines().await;
    assert!(!view.connected, "no worker listening ⇒ connected:false");
    assert_eq!(view.selection_generation, 2);

    // Retrying the identical desired state is idempotent even when the caller
    // only has the pre-mutation generation (for example, a lost response).
    let retry = broker
        .reselect(
            RemoteLayout {
                layout_id: "fa2-bh".to_string(),
                params: Some(params),
                ..Default::default()
            },
            Some(1),
        )
        .await
        .expect("identical stale retry is idempotent");
    assert_eq!(retry.generation, 2);
    assert!(!retry.changed);

    let stale = broker
        .reselect(
            RemoteLayout {
                layout_id: "cpu-spring".to_string(),
                ..Default::default()
            },
            Some(1),
        )
        .await
        .expect_err("different selection with stale generation must fail");
    assert!(stale.to_string().contains("stale selection generation"));
}

/// `reselect` on a never-connected broker is a caller error (no URL to dial).
#[tokio::test]
async fn reselect_errors_without_connect() {
    let broker = ComputeBroker::new();
    let err = broker
        .reselect(
            RemoteLayout {
                layout_id: "fa2-bh".to_string(),
                params: None,
                ..Default::default()
            },
            None,
        )
        .await
        .expect_err("reselect with no configured URL must error");
    assert!(
        err.to_string().contains("not connected"),
        "error should explain the broker isn't connected, got: {err}"
    );
}

/// A disabled broker cannot answer a manifest request at all — that is a
/// deployment state (503), not "this engine has no manifest" (404).
#[tokio::test]
async fn engine_manifest_reports_a_disabled_broker() {
    let broker = ComputeBroker::new();
    let error = broker
        .engine_manifest("fa2-bh")
        .await
        .expect_err("a disabled broker has no worker to ask");
    assert_eq!(error, ManifestUnavailable::BrokerDisabled);
    assert!(error.is_unavailable(), "maps to 503, not 404");
    assert!(
        error.reason().contains("compute broker disabled"),
        "reason names the deployment cause: {}",
        error.reason()
    );
}

/// The two definite-"no manifest" cases map to 404 and must be
/// distinguishable in the body: an engine that declares nothing is a
/// different fact from an engine that does not exist.
#[test]
fn undeclared_and_unknown_engines_are_distinguishable_404s() {
    let undeclared = ManifestUnavailable::Undeclared("geometric".to_string());
    let unknown = ManifestUnavailable::UnknownEngine("nope".to_string());
    assert!(!undeclared.is_unavailable() && !unknown.is_unavailable());
    assert!(
        undeclared.reason().contains("serves no capability manifest"),
        "{}",
        undeclared.reason()
    );
    assert!(
        unknown.reason().contains("no engine"),
        "{}",
        unknown.reason()
    );
    assert_ne!(undeclared.reason(), unknown.reason());
}

/// FROZEN CONTRACT: the HTTP body is snake_case, with `owned_by` and
/// `min_nodes` null-when-absent so a client can tell "no floor" from
/// "floor 0" and "not data-owned" from an empty owner string.
#[test]
fn manifest_view_serializes_to_the_frozen_shape() {
    let view = EngineManifestView {
        engine: "fa2-bh".to_string(),
        schema_version: 1,
        execution: "live".to_string(),
        dimensions: vec![
            CapabilityDimensionView {
                id: "scaling_ratio".to_string(),
                label: "Scaling ratio".to_string(),
                control: "absolute".to_string(),
                owned_by: None,
                note: String::new(),
                min_nodes: None,
            },
            CapabilityDimensionView {
                id: "edge_rest_len".to_string(),
                label: "Edge rest len".to_string(),
                control: "internal".to_string(),
                owned_by: Some("uff bond table".to_string()),
                note: "taken from typed data".to_string(),
                min_nodes: Some(500),
            },
        ],
    };
    let json = serde_json::to_value(&view).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "engine": "fa2-bh",
            "schema_version": 1,
            "execution": "live",
            "dimensions": [
                {
                    "id": "scaling_ratio",
                    "label": "Scaling ratio",
                    "control": "absolute",
                    "owned_by": null,
                    "note": "",
                    "min_nodes": null
                },
                {
                    "id": "edge_rest_len",
                    "label": "Edge rest len",
                    "control": "internal",
                    "owned_by": "uff bond table",
                    "note": "taken from typed data",
                    "min_nodes": 500
                }
            ]
        })
    );
}
