//! graph-compute binary entrypoint.
//!
//! Phase 1 scope: a single-GPU worker exposing the `Compute` gRPC service.
//! Default bind is dual-stack `[::]:50051` so the service is reachable from
//! external clients (broker, probe, the podman-machine VM on darwin); set
//! `GRAPH_COMPUTE_ADDR` to override (e.g. `[::1]:50051` for in-host loopback
//! only). The graph is a synthetic path graph by default — Phase 2 wires in a
//! partition map loaded from disk + the `vault-data` ingestion path.

use std::net::SocketAddr;

use anyhow::Result;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::service::{run_sim_loop, ComputeService};
use graph_compute::sim::{CsrGraph, SimState};
use tonic::transport::Server;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Phase 2: load a real graph from `GRAPH_COMPUTE_GRAPH_PATH` (binary CSR
    // file produced by graph-api's `/graph/csr.bin` at vault-load time). When
    // unset, fall back to a 1024-node path graph so unit tests / dev runs
    // without a vault still produce visible motion.
    let (graph, source) = match std::env::var("GRAPH_COMPUTE_GRAPH_PATH") {
        Ok(path) if !path.is_empty() => {
            let g = CsrGraph::load_bin(&path)?;
            let n_edges = g.neighbors.len();
            let src = format!(
                "file: {} ({} nodes, {} edges)",
                path, g.n_nodes, n_edges
            );
            (g, src)
        }
        _ => (
            CsrGraph::path(1024),
            "path-graph (1024 nodes)".to_string(),
        ),
    };
    tracing::info!(target: "graph-compute", "graph source: {source}");
    let state = SimState::new(graph);

    // Try to bring up the wgpu FA2 integrator. On hosts without a Vulkan/Metal
    // adapter (most CI runners), this returns false and we transparently fall
    // back to `cpu_step` — `try_init_wgpu` already logs the cause.
    let _ = state.try_init_wgpu().await;

    let bind: SocketAddr = std::env::var("GRAPH_COMPUTE_ADDR")
        .unwrap_or_else(|_| "[::]:50051".to_string())
        .parse()?;

    let tick_hz: f32 = std::env::var("GRAPH_COMPUTE_TICK_HZ")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30.0);

    tracing::info!(%bind, tick_hz, n_nodes = state.graph.n_nodes, "graph-compute starting");

    // Sim loop runs in the background; gRPC server runs in the foreground.
    let sim_state = state.clone();
    tokio::spawn(async move { run_sim_loop(sim_state, tick_hz).await });

    let svc = ComputeService::new(state);
    Server::builder()
        .add_service(ComputeServer::new(svc))
        .serve(bind)
        .await?;
    Ok(())
}
