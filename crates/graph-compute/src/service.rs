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

/// Drive the simulation forward. Prefers the wgpu FA2 integrator when
/// `SimState::wgpu_sim` is `Some`; otherwise falls back to the CPU
/// reference integrator so CI hosts without a GPU still produce frames.
pub async fn run_sim_loop(state: Arc<SimState>, tick_hz: f32) {
    let dt = 1.0 / tick_hz.max(1.0);
    let period = Duration::from_secs_f32(dt);
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        // Prefer the wgpu integrator. We take the WgpuSim out of the Mutex
        // for the duration of the blocking dispatch + readback (so the lock
        // doesn't sit across the join) and put it back afterwards. There's
        // exactly one tick task, so contention here is impossible.
        let taken_sim = state.wgpu_sim.lock().await.take();
        let new_positions = if let Some(mut sim) = taken_sim {
            let (positions, sim) = tokio::task::spawn_blocking(move || {
                let positions = sim.step();
                (positions, sim)
            })
            .await
            .expect("wgpu sim step panicked");
            *state.wgpu_sim.lock().await = Some(sim);
            positions
        } else {
            let snapshot = state.positions.read().await.clone();
            let graph = state.graph.clone();
            tokio::task::spawn_blocking(move || cpu_step(&graph, &snapshot, dt))
                .await
                .expect("cpu sim step panicked")
        };

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
