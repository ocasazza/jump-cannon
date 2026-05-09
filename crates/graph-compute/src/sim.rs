//! Simulation core.
//!
//! Holds the in-memory CSR graph + position/velocity buffers and advances them
//! one tick at a time. Two backends:
//!
//!   - `Cuda` (feature = "cuda"): allocates GPU buffers via cudarc and is
//!     intended to compile + launch a CUDA C force kernel via nvrtc. Phase 1
//!     ships the allocation + (stub) kernel-source path; the launch site is
//!     `unimplemented!()` so we don't pretend to do real GPU physics until the
//!     Phase 2 port lands.
//!
//!   - `Cpu` (always available): a tiny serial reference integrator —
//!     spring-only, no repulsion. Used by tests and as a fallback so the
//!     end-to-end gRPC stream works on machines without a CUDA driver.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::proto::PositionDelta;

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
        })
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

/// Stub CUDA C kernel source — the spring-only equivalent of the wgsl in
/// `crates/graph-layouts/src/layout/algorithms/shaders/force.wgsl`. Phase 2
/// will port the full force model (repulsion modes, gravity, integration) and
/// move the Barnes-Hut tree build onto the GPU (Karras 2012).
#[cfg(feature = "cuda")]
pub const FORCE_KERNEL_SRC: &str = r#"
extern "C" __global__ void force_step(
    const float* __restrict__ positions,
    float*       __restrict__ new_positions,
    const unsigned int* __restrict__ offsets,
    const unsigned int* __restrict__ neighbors,
    unsigned int n_nodes,
    float dt,
    float k_spring,
    float rest_len)
{
    unsigned int v = blockIdx.x * blockDim.x + threadIdx.x;
    if (v >= n_nodes) return;
    float vx = positions[3*v + 0];
    float vy = positions[3*v + 1];
    float vz = positions[3*v + 2];
    float fx = 0.0f, fy = 0.0f, fz = 0.0f;
    unsigned int start = offsets[v];
    unsigned int end   = offsets[v + 1];
    for (unsigned int e = start; e < end; ++e) {
        unsigned int u = neighbors[e];
        float dx = positions[3*u + 0] - vx;
        float dy = positions[3*u + 1] - vy;
        float dz = positions[3*u + 2] - vz;
        float dist = sqrtf(dx*dx + dy*dy + dz*dz);
        if (dist < 1e-4f) dist = 1e-4f;
        float f = k_spring * (dist - rest_len) / dist;
        fx += f * dx;
        fy += f * dy;
        fz += f * dz;
    }
    new_positions[3*v + 0] = vx + dt * fx;
    new_positions[3*v + 1] = vy + dt * fy;
    new_positions[3*v + 2] = vz + dt * fz;
}
"#;

#[cfg(feature = "cuda")]
pub mod cuda_backend {
    //! cudarc allocation scaffold. The kernel launch site is intentionally
    //! unimplemented — Phase 2 wires up nvrtc compilation + actual launch.
    use super::*;
    use anyhow::Result;

    pub struct CudaSim {
        // Real types live behind the `cuda` feature; we keep the struct empty
        // so the call sites compile without a CUDA toolkit during Phase 1.
        // Phase 2 will replace these with `Arc<CudaDevice>` + `CudaSlice<f32>`.
        pub n_nodes: u32,
    }

    impl CudaSim {
        /// Initialize a CUDA context on device 0 and allocate buffers. Phase 1
        /// stub: returns an error so callers fall back to the CPU integrator.
        pub fn new(_graph: &CsrGraph) -> Result<Self> {
            // Phase 2: cudarc::driver::CudaDevice::new(0)?, alloc positions /
            // velocities / offsets / neighbors / mass on the device, compile
            // FORCE_KERNEL_SRC via cudarc::nvrtc::compile_ptx.
            anyhow::bail!("CUDA backend kernel launch not yet implemented (Phase 2)")
        }

        pub fn step(&mut self, _dt: f32) -> Result<Vec<f32>> {
            // Phase 2: launch force_step kernel, copy positions back via pinned
            // memory; for now we never get here because `new` errors out.
            unimplemented!("graph-compute CUDA kernel launch is a Phase 2 deliverable")
        }
    }
}

