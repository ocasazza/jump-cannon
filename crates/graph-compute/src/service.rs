//! tonic gRPC service implementation.
//!
//! `Compute::Subscribe` returns a server-streaming `PositionDelta` channel
//! backed by `tokio::sync::broadcast`. The simulation tick task is the sole
//! producer; clients subscribing late simply pick up at the next frame.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream;
use tokio::sync::broadcast::error::RecvError;
use tonic::{Request, Response, Status};

use crate::proto::compute_server::Compute;
use crate::proto::{HealthRequest, HealthResponse, PositionDelta, SubscribeRequest};
use crate::sim::{cpu_step, SimState};

pub struct ComputeService {
    pub state: Arc<SimState>,
}

impl ComputeService {
    pub fn new(state: Arc<SimState>) -> Self {
        Self { state }
    }
}

type SubscribeStream =
    Pin<Box<dyn Stream<Item = Result<PositionDelta, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Compute for ComputeService {
    type SubscribeStream = SubscribeStream;

    async fn subscribe(
        &self,
        _req: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        // Subscribe BEFORE returning so we don't miss the next tick.
        let mut rx = self.state.tx.subscribe();
        let stream = async_stream::try_stream! {
            loop {
                match rx.recv().await {
                    Ok(frame) => yield frame,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "subscriber lagged; dropping frames");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    async fn health(
        &self,
        _req: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let frame = *self.state.frame.read().await;
        Ok(Response::new(HealthResponse {
            ok: true,
            n_nodes: self.state.graph.n_nodes,
            frame,
        }))
    }
}

/// Drive the simulation forward. CPU integrator only in Phase 1 — the CUDA
/// backend's `step` is `unimplemented!()` and we don't want the binary to
/// panic on hosts without a GPU. Phase 2 swaps this for the cudarc path.
pub async fn run_sim_loop(state: Arc<SimState>, tick_hz: f32) {
    let dt = 1.0 / tick_hz.max(1.0);
    let period = Duration::from_secs_f32(dt);
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        // Snapshot positions under the async lock, then run the integrator
        // in spawn_blocking so a 1M-node graph doesn't stall the runtime.
        // Phase 2: replace with cudarc launch.
        let snapshot = state.positions.read().await.clone();
        let graph = state.graph.clone();
        let new_positions =
            tokio::task::spawn_blocking(move || cpu_step(&graph, &snapshot, dt))
                .await
                .expect("sim step panicked");

        // Commit + broadcast.
        {
            let mut p = state.positions.write().await;
            *p = new_positions.clone();
        }
        let frame = {
            let mut f = state.frame.write().await;
            *f += 1;
            *f
        };
        let bytes = bytemuck::cast_slice::<f32, u8>(&new_positions).to_vec();
        let delta = PositionDelta {
            frame,
            positions: bytes,
            n_nodes: state.graph.n_nodes,
        };
        // ignore send errors; broadcast returns Err if no receivers — that's fine.
        let _ = state.tx.send(delta);
    }
}
