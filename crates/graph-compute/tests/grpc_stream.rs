//! End-to-end test: stand up the tonic server with a stub graph, connect a
//! gRPC client, and confirm at least one `PositionDelta` is delivered.
//!
//! Uses the CPU integrator so it runs anywhere — CUDA-only paths are gated
//! out for Phase 1.

use std::time::Duration;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::{HealthRequest, SubscribeRequest};
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

/// Stronger end-to-end check: positions actually advance over time. Both the
/// wgpu FA2 path and the CPU spring-only fallback produce non-zero motion on
/// the deterministic ring seed, so this assertion holds either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positions_advance_over_frames() {
    let graph = CsrGraph::path(64);
    let state = SimState::new(graph);
    // Best-effort GPU init — if it fails the CPU fallback still produces motion.
    let _ = state.try_init_wgpu().await;

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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint)
        .await
        .expect("connect to compute server");

    let mut stream = client
        .subscribe(SubscribeRequest { graph_id: "test".into() })
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

    server.abort();
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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint)
        .await
        .expect("connect to compute server");

    // 3. Health check: n_nodes must reflect the on-disk graph.
    let health = client
        .health(HealthRequest {})
        .await
        .expect("health rpc")
        .into_inner();
    assert_eq!(health.n_nodes, 64);
    assert!(health.ok);

    server.abort();
    std::env::remove_var("GRAPH_COMPUTE_GRAPH_PATH");
    let _ = std::fs::remove_file(&tmp);
}
