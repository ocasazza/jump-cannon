//! End-to-end test: stand up the tonic server with a stub graph, connect a
//! gRPC client, and confirm at least one `PositionDelta` is delivered.
//!
//! Uses the CPU integrator so it runs anywhere — CUDA-only paths are gated
//! out for Phase 1.

use std::time::Duration;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::SubscribeRequest;
use graph_compute::service::{run_sim_loop, ComputeService};
use graph_compute::sim::{CsrGraph, SimState};
use tonic::transport::Server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivers_at_least_one_position_delta() {
    let graph = CsrGraph::path(64);
    let state = SimState::new(graph);

    // Bind to an ephemeral port. Tonic's `Server::serve` doesn't surface the
    // bound port, so we bind a std listener first and pull the port out.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });

    let svc = ComputeService::new(state);
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve(addr)
            .await
            .unwrap();
    });

    // Give the server a beat to bind.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint)
        .await
        .expect("connect to compute server");

    let mut stream = client
        .subscribe(SubscribeRequest {
            graph_id: "test".into(),
        })
        .await
        .expect("subscribe")
        .into_inner();

    let frame = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for frame")
        .expect("stream errored")
        .expect("stream ended without a frame");

    assert_eq!(frame.n_nodes, 64);
    assert_eq!(frame.positions.len(), 64 * 3 * 4); // n * xyz * f32
    assert!(frame.frame >= 1);

    server.abort();
}
