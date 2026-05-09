//! graph-compute binary entrypoint.
//!
//! Phase 1 scope: a single-GPU worker exposing the `Compute` gRPC service over
//! `[::1]:50051`. The graph is a synthetic path graph by default — Phase 2
//! wires in a partition map loaded from disk + the `vault-data` ingestion path.

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

    // TODO(phase 2): load a real graph from `--graph <path>` (binary CSR file
    // produced by graph-api at vault-load time). For now: a 1024-node path
    // graph so the CPU integrator does something visible.
    let graph = CsrGraph::path(1024);
    let state = SimState::new(graph);

    let bind: SocketAddr = std::env::var("GRAPH_COMPUTE_ADDR")
        .unwrap_or_else(|_| "[::1]:50051".to_string())
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
