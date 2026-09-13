//! In-process round-trip test for the `Compute::EngineManifest` RPC
//! (docs/layout-ux-spec.md §3 / FROZEN CONTRACT). Exercises the REAL gRPC
//! path — the `EngineManifestResponse` crosses the tonic codec — WITHOUT
//! binding a TCP port, using the same `tokio::io::duplex` + `service_fn`
//! connector pattern as `list_engines_grpc.rs`. No sockets, so it runs under
//! the sandbox.
//!
//! The contract this pins: a declaring engine answers `supported: true` with
//! one dimension per settings field it honours, `execution` matching its
//! descriptor kind, and every dimension carrying an id + label + control
//! kind; an unknown id answers `supported: false` rather than erroring, which
//! is what lets a client fall back to a generic form instead of showing an
//! error for an engine that simply declares nothing (M5).

use std::future::ready;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::EngineManifestRequest;
use graph_compute::service::ComputeService;
use graph_compute::sim::{CsrGraph, SimState};
use hyper_util::rt::TokioIo;
use tonic::transport::{Endpoint, Server, Uri};

async fn client() -> ComputeClient<tonic::transport::Channel> {
    let state = SimState::new(CsrGraph::path(6));
    let svc = ComputeService::new(state);
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    let mut client_io = Some(client_io);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex");
    ComputeClient::new(channel)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_manifest_declares_the_engines_own_settings_fields() {
    let mut client = client().await;
    let resp = client
        .engine_manifest(EngineManifestRequest {
            engine_id: "fa2-bh".to_string(),
        })
        .await
        .expect("EngineManifest call")
        .into_inner();

    assert!(resp.supported, "fa2-bh declares a manifest");
    assert_eq!(resp.engine, "fa2-bh");
    assert_eq!(resp.schema_version, 1);
    assert_eq!(resp.execution, "live", "fa2-bh is a continuous physics engine");

    let ids: Vec<&str> = resp.dimensions.iter().map(|d| d.id.as_str()).collect();
    // The dimensions are projected from `Fa2BhSettings`, so the engine's real
    // knobs are present and named exactly as `set_params` accepts them.
    for expected in ["gravity", "scaling_ratio", "strong_gravity", "theta"] {
        assert!(
            ids.contains(&expected),
            "manifest must declare {expected:?}: {ids:?}"
        );
    }
    for dimension in &resp.dimensions {
        assert!(!dimension.id.is_empty(), "dimension needs an id");
        assert!(
            !dimension.label.is_empty(),
            "dimension {:?} needs a display label",
            dimension.id
        );
        assert!(
            ["multiplier", "absolute", "toggle", "enum", "internal"]
                .contains(&dimension.control.as_str()),
            "dimension {:?} has an out-of-vocabulary control {:?}",
            dimension.id,
            dimension.control
        );
    }
    let by_id = |id: &str| {
        resp.dimensions
            .iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    assert_eq!(by_id("strong_gravity").control, "toggle", "bool → toggle");
    assert_eq!(by_id("gravity").control, "absolute", "number → absolute");
    assert!(
        by_id("theta").note.contains("Barnes-Hut"),
        "annotated note crosses the wire: {:?}",
        by_id("theta").note
    );
}

/// An unknown engine id is not an error: the worker says "nothing declared"
/// and the client renders the engine's settings generically (M5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_engine_answers_unsupported_without_erroring() {
    let mut client = client().await;
    let resp = client
        .engine_manifest(EngineManifestRequest {
            engine_id: "no-such-engine".to_string(),
        })
        .await
        .expect("EngineManifest call must not fail for an unknown id")
        .into_inner();
    assert!(!resp.supported);
    assert!(resp.dimensions.is_empty());
    assert_eq!(resp.schema_version, 0, "unset when unsupported");
}
