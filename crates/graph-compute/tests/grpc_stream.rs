//! End-to-end test: stand up the tonic server with a stub graph, connect a
//! gRPC client, and confirm at least one `PositionDelta` is delivered.
//!
//! Uses the CPU integrator so it runs anywhere — CUDA-only paths are gated
//! out for Phase 1.
//!
//! Transport: an in-process `tokio::io::duplex` pipe (the same pattern proven
//! in `tests/exchange_halo_grpc.rs`) — the server end is fed to
//! `Server::serve_with_incoming` and the client connects through the other end
//! via `Endpoint::connect_with_connector` + a `tower::service_fn` connector
//! (the duplex wrapped in `hyper_util::rt::TokioIo`). No TCP port is bound, so
//! these run under the sandbox.

use std::future::ready;
use std::time::Duration;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::{HealthRequest, SubscribeRequest};
use graph_compute::service::{run_sim_loop, ComputeService};
use graph_compute::sim::{CsrGraph, SimState};
use hyper_util::rt::TokioIo;
use tonic::transport::{Channel, Endpoint, Server, Uri};

/// Serve `svc` over a single in-memory duplex connection (no TCP bind) and
/// return a `Channel` connected through the other end of the same pipe. The
/// `Uri` handed to the connector is a dummy — the connector ignores it and
/// always hands back our in-memory client end.
async fn connect_in_process(svc: ComputeService) -> Channel {
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
    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivers_at_least_one_position_delta() {
    let graph = CsrGraph::path(64);
    let state = SimState::new(graph);
    // Init the layout engine (registry default `"fa2-brute"`, falling back to
    // the CPU spring engine on GPU-less hosts) so the sim loop produces frames.
    let _ = state.init_engine("", serde_json::Value::Null, None).await;

    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });

    let svc = ComputeService::new(state);
    let channel = connect_in_process(svc).await;
    let mut client = ComputeClient::new(channel);

    let mut stream = client
        .subscribe(SubscribeRequest {
            graph_id: "test".into(),
            ..Default::default()
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
}

/// Stronger end-to-end check: positions actually advance over time. Both the
/// wgpu FA2 path and the CPU spring-only fallback produce non-zero motion on
/// the deterministic ring seed, so this assertion holds either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positions_advance_over_frames() {
    let graph = CsrGraph::path(64);
    let state = SimState::new(graph);
    // Init the layout engine — the wgpu FA2 engine if a GPU is present, else
    // the CPU spring fallback. Both produce non-zero motion on the ring seed.
    let _ = state.init_engine("", serde_json::Value::Null, None).await;

    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });

    let svc = ComputeService::new(state);
    let channel = connect_in_process(svc).await;
    let mut client = ComputeClient::new(channel);

    let mut stream = client
        .subscribe(SubscribeRequest { graph_id: "test".into(), ..Default::default() })
        .await
        .expect("subscribe")
        .into_inner();

    // Pull frames; capture frame 1 and frame 30 (or whatever shows up
    // ~500ms later at 60Hz, with safety margin).
    let mut first: Option<Vec<f32>> = None;
    let mut later: Option<Vec<f32>> = None;
    let mut count: u32 = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let msg = tokio::time::timeout(Duration::from_secs(2), stream.message())
            .await
            .expect("timed out waiting for frame")
            .expect("stream errored")
            .expect("stream ended without a frame");
        let positions: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&msg.positions).to_vec();
        count += 1;
        if first.is_none() {
            first = Some(positions);
        } else if count >= 30 {
            later = Some(positions);
            break;
        }
    }

    let first = first.expect("no first frame");
    let later = later.expect("did not reach frame 30");
    assert_eq!(first.len(), later.len());

    let l2_sq: f32 = first
        .iter()
        .zip(later.iter())
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    let l2 = l2_sq.sqrt();
    assert!(
        l2 > 0.0,
        "positions did not advance over 30 frames (L2 = {})",
        l2
    );
}

/// Phase 2: validate that a CSR file written via `write_bin` and re-loaded
/// via `load_bin` (the same path the binary takes when `GRAPH_COMPUTE_GRAPH_PATH`
/// is set) drives the gRPC server's reported `n_nodes` correctly.
///
/// We don't shell out to the `graph-compute` binary here — that would couple
/// this test to cargo's bin-output path. Instead we exercise the same code
/// the binary's main runs: read env -> `CsrGraph::load_bin` -> SimState ->
/// `Compute::Health`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_graph_from_file_via_env() {
    // 1. Write a 64-node path graph to a tempfile.
    let original = CsrGraph::path(64);
    let tmp = std::env::temp_dir().join(format!(
        "graph-compute-env-load-{}.bin",
        std::process::id()
    ));
    original.write_bin(&tmp).expect("write_bin");

    // 2. Set the env var the binary reads, then mirror the binary's load path.
    std::env::set_var("GRAPH_COMPUTE_GRAPH_PATH", &tmp);
    let path = std::env::var("GRAPH_COMPUTE_GRAPH_PATH").unwrap();
    let graph = CsrGraph::load_bin(&path).expect("load_bin from env");
    assert_eq!(graph.n_nodes, 64);

    let state = SimState::new(graph);

    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });

    let svc = ComputeService::new(state);
    let channel = connect_in_process(svc).await;
    let mut client = ComputeClient::new(channel);

    // 3. Health check: n_nodes must reflect the on-disk graph.
    let health = client
        .health(HealthRequest {})
        .await
        .expect("health rpc")
        .into_inner();
    assert_eq!(health.n_nodes, 64);
    assert!(health.ok);

    std::env::remove_var("GRAPH_COMPUTE_GRAPH_PATH");
    let _ = std::fs::remove_file(&tmp);
}
