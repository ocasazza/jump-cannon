//! Simulation core.
//!
//! Holds the in-memory CSR graph + position/velocity buffers and advances them
//! one tick at a time. Two backends:
//!
//!   - `WgpuSim` (preferred): server-side wgpu compute pipeline running the
//!     ForceAtlas2 shader from `crates/graph-layouts`. Brought up lazily via
//!     `try_init_wgpu`; failure (no adapter, no Vulkan ICD, etc.) leaves the
//!     slot empty and the loop falls back to `cpu_step`.
//!
//!   - `cpu_step` (fallback): tiny serial spring-only integrator — runs
//!     anywhere, used by tests and on hosts without a GPU.

use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::proto::PositionDelta;
use crate::wgpu_sim::WgpuSim;

/// Compressed-sparse-row graph. Edge `i` connects `src=node` (where
/// `offsets[node] <= i < offsets[node+1]`) to `neighbors[i]`.
#[derive(Clone, Debug)]
pub struct CsrGraph {
    pub n_nodes: u32,
    pub offsets: Vec<u32>,
    pub neighbors: Vec<u32>,
}

impl CsrGraph {
    /// Trivial path graph 0—1—2—…—(n-1). Useful for tests and the default
    /// `--demo` path when no graph file is provided.
    pub fn path(n: u32) -> Self {
        let mut offsets = Vec::with_capacity((n + 1) as usize);
        let mut neighbors = Vec::new();
        for i in 0..n {
            offsets.push(neighbors.len() as u32);
            if i > 0 {
                neighbors.push(i - 1);
            }
            if i + 1 < n {
                neighbors.push(i + 1);
            }
        }
        offsets.push(neighbors.len() as u32);
        Self { n_nodes: n, offsets, neighbors }
    }
}

/// State shared between the simulation tick task and the gRPC service.
pub struct SimState {
    pub graph: CsrGraph,
    /// Interleaved x,y,z f32 positions, length `3 * n_nodes`. Host copy —
    /// the CUDA backend mirrors this on the device and copies back each tick.
    pub positions: RwLock<Vec<f32>>,
    pub frame: RwLock<u64>,
    /// Broadcast channel of per-tick `PositionDelta` snapshots. The gRPC
    /// `Subscribe` handler subscribes to this; the simulation tick task is
    /// the sole producer. Lagging subscribers drop frames (log + continue).
    pub tx: broadcast::Sender<PositionDelta>,
    /// Lazily-initialized wgpu integrator. `None` until `try_init_wgpu` succeeds;
    /// if it fails (no adapter / no Vulkan ICD on the host) the run loop falls
    /// back to `cpu_step` and this stays `None` for the lifetime of the process.
    pub wgpu_sim: Mutex<Option<WgpuSim>>,
}

impl SimState {
    pub fn new(graph: CsrGraph) -> Arc<Self> {
        let n = graph.n_nodes as usize;
        // Deterministic ring seed: same convention as
        // crates/graph-api/src/vault_loader.rs.
        let mut positions = vec![0.0f32; 3 * n];
        for i in 0..n {
            let t = (i as f32) / (n.max(1) as f32) * std::f32::consts::TAU;
            positions[3 * i] = t.cos();
            positions[3 * i + 1] = t.sin();
            positions[3 * i + 2] = 0.0;
        }
        // 32-frame ring buffer; bigger than typical RTT so a brief client
        // hiccup doesn't drop a frame.
        let (tx, _rx) = broadcast::channel(32);
        Arc::new(Self {
            graph,
            positions: RwLock::new(positions),
            frame: RwLock::new(0),
            tx,
            wgpu_sim: Mutex::new(None),
        })
    }

    /// Attempt to bring up the wgpu integrator. Returns `true` on success.
    /// On failure the slot stays `None`, the caller logs the cause, and the
    /// sim loop transparently falls back to `cpu_step`.
    pub async fn try_init_wgpu(self: &Arc<Self>) -> bool {
        let positions = self.positions.read().await.clone();
        let graph = self.graph.clone();
        // The wgpu device construction blocks on adapter request; do it on the
        // blocking pool so we don't stall the runtime.
        let init = tokio::task::spawn_blocking(move || WgpuSim::new(&graph, &positions))
            .await
            .expect("wgpu init task panicked");
        match init {
            Ok(sim) => {
                tracing::info!(
                    backend = ?sim.adapter_info.backend,
                    name = %sim.adapter_info.name,
                    device_type = ?sim.adapter_info.device_type,
                    "wgpu adapter initialized; using GPU FA2 integrator"
                );
                let mut slot = self.wgpu_sim.lock().await;
                *slot = Some(sim);
                true
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "wgpu adapter not found, falling back to cpu_step"
                );
                false
            }
        }
    }
}

/// CPU reference integrator. Spring-only, no repulsion. NOT meant to scale —
/// it's the fallback the grpc test uses on hosts without a CUDA driver, and it
/// shares the wire format with the future CUDA backend so the API broker
/// doesn't care which one produced the frame.
///
/// The caller passes in the current `positions` snapshot (taken under the
/// async RwLock in the sim loop) so this function stays sync and can be run
/// inside `tokio::task::spawn_blocking`.
pub fn cpu_step(graph: &CsrGraph, positions: &[f32], dt: f32) -> Vec<f32> {
    let n = graph.n_nodes as usize;
    let mut new_positions = positions.to_vec();
    let k_spring = 0.05f32;
    let rest_len = 1.0f32;
    for v in 0..n {
        let start = graph.offsets[v] as usize;
        let end = graph.offsets[v + 1] as usize;
        let (vx, vy, vz) = (positions[3 * v], positions[3 * v + 1], positions[3 * v + 2]);
        let mut fx = 0.0f32;
        let mut fy = 0.0f32;
        let mut fz = 0.0f32;
        for e in start..end {
            let u = graph.neighbors[e] as usize;
            let dx = positions[3 * u] - vx;
            let dy = positions[3 * u + 1] - vy;
            let dz = positions[3 * u + 2] - vz;
            let dist = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-4);
            let f = k_spring * (dist - rest_len) / dist;
            fx += f * dx;
            fy += f * dy;
            fz += f * dz;
        }
        new_positions[3 * v] = vx + dt * fx;
        new_positions[3 * v + 1] = vy + dt * fy;
        new_positions[3 * v + 2] = vz + dt * fz;
    }
    new_positions
}

