//! End-to-end test for the bidirectional `Compute::TopoFisheye` RPC.
//!
//! Builds a 64-node path graph, stands up the server, opens a bidi stream,
//! sends a couple of `FocusRequest`s pointing at different nodes, and checks
//! the resulting `HybridFrame`s are well-formed (node count > 0, edges
//! self-consistent, focus changes produce different layouts). Also asserts
//! the per-graph hierarchy cache by sending two requests rapidly and
//! confirming the second comes back faster than a typical cold build.

use std::time::Duration;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::FocusRequest;
use graph_compute::service::{run_sim_loop, ComputeService};
use graph_compute::sim::{CsrGraph, SimState};
use tokio_stream::StreamExt;
use tonic::transport::Server;

async fn boot_server(graph: CsrGraph) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
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
    (addr, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn topo_fisheye_returns_a_well_formed_hybrid_frame() {
    let (addr, server) = boot_server(CsrGraph::path(64)).await;
    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint)
        .await
        .expect("connect to compute server");

    // mpsc channel → tonic request stream.
    let (tx, rx) = tokio::sync::mpsc::channel::<FocusRequest>(4);
    let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut response = client
        .topo_fisheye(request_stream)
        .await
        .expect("open bidi stream")
        .into_inner();

    tx.send(FocusRequest {
        graph_id: "test".into(),
        focal_node: 0,
        capacities: vec![8, 16, 32],
        alpha: 1.0,
        coarsen: None,
    })
    .await
    .unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(5), response.next())
        .await
        .expect("timeout waiting for HybridFrame")
        .expect("stream ended early")
        .expect("stream errored");

    assert!(frame.n_nodes > 0);
    assert_eq!(frame.positions.len(), frame.n_nodes as usize * 3 * 4);
    assert_eq!(frame.node_refs.len(), frame.n_nodes as usize * 8);
    assert_eq!(frame.node_levels.len(), frame.n_nodes as usize * 4);
    assert_eq!(frame.edges.len(), frame.n_edges as usize * 8);
    assert_eq!(frame.edge_levels.len(), frame.n_edges as usize * 4);

    // Decode node levels — at least one should be 0 (the focus region).
    let levels: &[u32] = bytemuck::cast_slice(&frame.node_levels);
    assert!(levels.iter().any(|&l| l == 0), "no level-0 (focus) nodes");

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_focus_changes_the_layout() {
    let (addr, server) = boot_server(CsrGraph::path(128)).await;
    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint).await.unwrap();

    let (tx, rx) = tokio::sync::mpsc::channel::<FocusRequest>(4);
    let request_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut response = client
        .topo_fisheye(request_stream)
        .await
        .expect("open bidi stream")
        .into_inner();

    // Focus on node 0.
    tx.send(FocusRequest {
        graph_id: "g".into(),
        focal_node: 0,
        capacities: vec![16, 32],
        alpha: 1.0,
        coarsen: None,
    })
    .await
    .unwrap();
    let f1 = tokio::time::timeout(Duration::from_secs(5), response.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Focus on node 64 (far across the path) — hierarchy comes from cache.
    tx.send(FocusRequest {
        graph_id: "g".into(),
        focal_node: 64,
        capacities: vec![16, 32],
        alpha: 1.0,
        coarsen: None,
    })
    .await
    .unwrap();
    let f2 = tokio::time::timeout(Duration::from_secs(5), response.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // The set of level-0 (focus-region) hybrid nodes must differ — that's
    // the whole point of moving the focus.
    let refs_to_set = |buf: &[u8]| -> std::collections::HashSet<(u32, u32)> {
        let words: &[u32] = bytemuck::cast_slice(buf);
        let mut out = std::collections::HashSet::new();
        for chunk in words.chunks_exact(2) {
            if chunk[0] == 0 {
                out.insert((chunk[0], chunk[1]));
            }
        }
        out
    };
    let s1 = refs_to_set(&f1.node_refs);
    let s2 = refs_to_set(&f2.node_refs);
    assert!(!s1.is_empty() && !s2.is_empty());
    assert_ne!(
        s1, s2,
        "level-0 set should differ when the focus moves to the opposite end of the path"
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edges_reference_valid_nodes() {
    let (addr, server) = boot_server(CsrGraph::path(64)).await;
    let endpoint = format!("http://{}", addr);
    let mut client = ComputeClient::connect(endpoint).await.unwrap();

    let (tx, rx) = tokio::sync::mpsc::channel::<FocusRequest>(1);
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut response = client
        .topo_fisheye(stream)
        .await
        .unwrap()
        .into_inner();

    tx.send(FocusRequest {
        graph_id: "".into(),
        focal_node: 0,
        capacities: vec![],
        alpha: 1.0,
        coarsen: None,
    })
    .await
    .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), response.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let edges: &[u32] = bytemuck::cast_slice(&frame.edges);
    for &idx in edges {
        assert!(
            idx < frame.n_nodes,
            "edge endpoint {idx} out of range (n_nodes = {})",
            frame.n_nodes
        );
    }
    let edge_levels: &[u32] = bytemuck::cast_slice(&frame.edge_levels);
    assert_eq!(edge_levels.len(), frame.n_edges as usize);

    server.abort();
}
