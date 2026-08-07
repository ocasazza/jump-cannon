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
use graph_compute::proto::{HealthRequest, LoadGraphRequest, SubscribeRequest};
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

fn position_bytes(positions: &[f32]) -> Vec<u8> {
    positions
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn four_node_cycle() -> CsrGraph {
    CsrGraph {
        n_nodes: 4,
        offsets: vec![0, 2, 4, 6, 8],
        neighbors: vec![1, 3, 0, 2, 1, 3, 0, 2],
    }
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
            graph_revision: 0,
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
        .subscribe(SubscribeRequest {
            graph_id: "test".into(),
            graph_revision: 0,
            ..Default::default()
        })
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

/// A same-cardinality topology replacement must still be distinguishable from
/// the graph it replaced. The worker rejects stale subscriptions and stamps
/// every new frame with the replacement revision rather than relying on node
/// count as an identity proxy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_cardinality_reload_rejects_stale_revision_and_stamps_new_frames() {
    let state = SimState::new(CsrGraph::path(4));
    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, 60.0).await });

    let channel = connect_in_process(ComputeService::new(state.clone())).await;
    let mut client = ComputeClient::new(channel);

    let first = client
        .load_graph(LoadGraphRequest {
            csr: CsrGraph::path(4).to_bin(),
            positions: Vec::new(),
            graph_revision: 41,
        })
        .await
        .expect("load first graph")
        .into_inner();
    assert!(first.ok, "first LoadGraph failed: {}", first.error);
    assert_eq!(first.graph_revision, 41);

    let replacement_seed = vec![
        -3.0, 0.0, 0.0, // node 0
        0.0, 3.0, 0.0, // node 1
        3.0, 0.0, 0.0, // node 2
        0.0, -3.0, 0.0, // node 3
    ];
    let replacement = client
        .load_graph(LoadGraphRequest {
            csr: four_node_cycle().to_bin(),
            positions: position_bytes(&replacement_seed),
            graph_revision: 42,
        })
        .await
        .expect("load same-cardinality replacement")
        .into_inner();
    assert!(replacement.ok, "replacement failed: {}", replacement.error);
    assert_eq!(replacement.n_nodes, 4);
    assert_eq!(replacement.graph_revision, 42);

    let stale = client
        .subscribe(SubscribeRequest {
            graph_id: "stale".into(),
            graph_revision: 41,
            layout_id: "cpu-spring".into(),
            ..Default::default()
        })
        .await
        .expect_err("stale graph revision must be rejected");
    assert_eq!(stale.code(), tonic::Code::FailedPrecondition);

    let mut stream = client
        .subscribe(SubscribeRequest {
            graph_id: "replacement".into(),
            graph_revision: 42,
            layout_id: "cpu-spring".into(),
            ..Default::default()
        })
        .await
        .expect("subscribe to replacement")
        .into_inner();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for replacement frame")
        .expect("replacement stream errored")
        .expect("replacement stream ended");
    assert_eq!(frame.graph_revision, 42);
    assert_eq!(frame.n_nodes, 4);
}

/// `LoadGraph.positions` is the remote seed contract. Preserve it exactly so
/// choosing remote execution does not silently replace the caller's initial
/// placement before an engine is selected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_graph_preserves_supplied_positions_and_rejects_bad_arity_atomically() {
    let state = SimState::new(CsrGraph::path(3));
    let channel = connect_in_process(ComputeService::new(state.clone())).await;
    let mut client = ComputeClient::new(channel);

    let supplied = vec![
        -1.0, -2.0, -3.0, // node 0
        4.0, 5.0, 6.0, // node 1
        7.5, 8.5, 9.5, // node 2
    ];
    let loaded = client
        .load_graph(LoadGraphRequest {
            csr: CsrGraph::path(3).to_bin(),
            positions: position_bytes(&supplied),
            graph_revision: 77,
        })
        .await
        .expect("load graph with positions")
        .into_inner();
    assert!(loaded.ok, "LoadGraph failed: {}", loaded.error);
    assert_eq!(loaded.graph_revision, 77);
    assert_eq!(*state.positions.read().await, supplied);

    let before_graph = state.graph.read().await.to_bin();
    let rejected = client
        .load_graph(LoadGraphRequest {
            csr: four_node_cycle().to_bin(),
            positions: position_bytes(&[0.0, 1.0, 2.0]),
            graph_revision: 78,
        })
        .await
        .expect("invalid LoadGraph returns a soft error")
        .into_inner();
    assert!(!rejected.ok, "bad position arity unexpectedly succeeded");
    assert!(rejected.error.contains("positions length"));
    assert_eq!(rejected.graph_revision, 77);
    assert_eq!(
        state
            .graph_revision
            .load(std::sync::atomic::Ordering::Acquire),
        77
    );
    assert_eq!(state.graph.read().await.to_bin(), before_graph);
    assert_eq!(*state.positions.read().await, supplied);
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
    let tmp =
        std::env::temp_dir().join(format!("graph-compute-env-load-{}.bin", std::process::id()));
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
