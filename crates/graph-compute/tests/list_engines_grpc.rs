//! In-process round-trip test for the `Compute::ListEngines` RPC (ADR-002 /
//! FROZEN CONTRACT). Exercises the REAL gRPC path — the `ListEnginesResponse`
//! crosses the tonic codec — WITHOUT binding a TCP port, using the same
//! `tokio::io::duplex` + `service_fn` connector pattern as
//! `exchange_halo_grpc.rs`. No sockets, so it runs under the sandbox.
//!
//! Asserts the response enumerates the worker's builtin EngineRegistry: every
//! known id is present with a non-empty `display_name`, and `default_id` is the
//! registry default ("fa2-brute").

use std::collections::HashMap;
use std::future::ready;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::ListEnginesRequest;
use graph_compute::service::ComputeService;
use graph_compute::sim::{CsrGraph, SimState};
use hyper_util::rt::TokioIo;
use tonic::transport::{Endpoint, Server, Uri};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_engines_enumerates_builtin_registry() {
    // The graph topology is irrelevant to ListEngines — it reads the registry.
    let state = SimState::new(CsrGraph::path(6));
    let svc = ComputeService::new(state);

    // In-memory transport: a duplex pipe. Server gets one end as its single
    // incoming connection; the client connects through the other end (no TCP).
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    // The Uri is a dummy — the connector ignores it and always returns our
    // in-memory pipe.
    let mut client_io = Some(client_io);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex");

    let mut client = ComputeClient::new(channel);
    let resp = client
        .list_engines(ListEnginesRequest {})
        .await
        .expect("ListEngines call")
        .into_inner();

    assert_eq!(
        resp.default_id, "fa2-brute",
        "registry default engine id must round-trip"
    );

    // Index by id so we can assert per-engine fields.
    let by_id: HashMap<&str, &graph_compute::proto::EngineDescriptor> =
        resp.engines.iter().map(|e| (e.id.as_str(), e)).collect();

    for id in [
        "fa2-brute",
        "fa2-bh",
        "sgd-stress",
        "multilevel",
        "cpu-spring",
    ] {
        let d = by_id
            .get(id)
            .unwrap_or_else(|| panic!("ListEngines missing known engine {id:?}"));
        assert!(
            !d.display_name.is_empty(),
            "engine {id:?} must carry a non-empty display_name"
        );
        assert!(
            d.kind == "Physics" || d.kind == "Static",
            "engine {id:?} kind must be Physics|Static, got {:?}",
            d.kind
        );
    }
}
