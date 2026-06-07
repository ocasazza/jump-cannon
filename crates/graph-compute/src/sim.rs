//! Simulation core.
//!
//! Holds the in-memory CSR graph + position buffers and advances them one tick
//! at a time by driving the **selected layout engine** from the
//! [`crate::engines`] registry (ADR-001). `SimState::init_engine` constructs the
//! requested engine (default `"fa2-brute"`), tries wgpu bring-up once via
//! [`EngineCtx`], and falls back to the `"cpu-spring"` engine when no adapter is
//! available. The hardcoded `WgpuSim`/`cpu_step` dichotomy is gone — both are
//! now engines behind one trait.
//!
//! `cpu_step` itself remains here as the reference integrator the
//! `CpuSpringEngine` wraps (and that tests call directly).

use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::engines::{CsrShard, EngineCtx, EngineRegistry, GraphAttributes, LayoutEngine};
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
        Self {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// Load a CSR graph from disk. Wire format (all little-endian):
    ///
    /// ```text
    /// [u32 n_nodes][u32 n_edges][u32 × (n_nodes+1) offsets][u32 × n_edges neighbors]
    /// ```
    ///
    /// Matches the `/graph/csr.bin` exporter in `graph-api`. Same on-disk
    /// format SkyPilot mounts via `file_mounts` in `infra/sky/graph-compute.yaml`.
    pub fn load_bin(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("failed to read CSR file {}: {}", path.display(), e))?;
        Self::from_bin_bytes(&bytes)
            .map_err(|e| anyhow::anyhow!("CSR file {}: {}", path.display(), e))
    }

    /// Parse the same binary CSR format `load_bin` reads, from an in-memory
    /// byte slice — the wire form of the `LoadGraph` gRPC and graph-api's
    /// `/graph/csr.bin`. Alignment-safe (the gRPC buffer is `u8`-aligned), so it
    /// reads u32s via `from_le_bytes` rather than a cast.
    pub fn from_bin_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() < 8 {
            anyhow::bail!("CSR truncated: {} bytes < 8-byte header", bytes.len());
        }
        let rd =
            |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let n_nodes = rd(0);
        let n_edges = rd(4);
        let expected = 8 + 4 * (n_nodes as usize + 1) + 4 * (n_edges as usize);
        if bytes.len() != expected {
            anyhow::bail!(
                "CSR length mismatch: got {} bytes, expected {} (n_nodes={}, n_edges={})",
                bytes.len(),
                expected,
                n_nodes,
                n_edges
            );
        }
        let read_u32s = |start: usize, count: usize| -> Vec<u32> {
            bytes[start..start + 4 * count]
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        };
        let offsets_start = 8usize;
        let offsets = read_u32s(offsets_start, n_nodes as usize + 1);
        let neighbors = read_u32s(offsets_start + 4 * (n_nodes as usize + 1), n_edges as usize);
        Ok(Self {
            n_nodes,
            offsets,
            neighbors,
        })
    }

    /// Serialize this CSR graph to disk in the format `load_bin` consumes.
    /// Used by the `/graph/csr.bin` exporter in graph-api and by tests.
    pub fn write_bin(&self, path: impl AsRef<std::path::Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let bytes = self.to_bin();
        std::fs::write(path, bytes)
            .map_err(|e| anyhow::anyhow!("failed to write CSR file {}: {}", path.display(), e))?;
        Ok(())
    }

    /// In-memory equivalent of `write_bin` — produces the same LE byte buffer.
    /// Lets graph-api's `/graph/csr.bin` handler emit the format without
    /// touching the disk.
    pub fn to_bin(&self) -> Vec<u8> {
        let n_edges = self.neighbors.len() as u32;
        let mut out = Vec::with_capacity(8 + 4 * (self.offsets.len()) + 4 * (self.neighbors.len()));
        out.extend_from_slice(&self.n_nodes.to_le_bytes());
        out.extend_from_slice(&n_edges.to_le_bytes());
        out.extend_from_slice(bytemuck::cast_slice(&self.offsets));
        out.extend_from_slice(bytemuck::cast_slice(&self.neighbors));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_bin_roundtrip() {
        let original = CsrGraph::path(8);
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "graph-compute-csr-roundtrip-{}.bin",
            std::process::id()
        ));
        original.write_bin(&path).expect("write_bin");
        let loaded = CsrGraph::load_bin(&path).expect("load_bin");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.n_nodes, original.n_nodes);
        assert_eq!(loaded.offsets, original.offsets);
        assert_eq!(loaded.neighbors, original.neighbors);
    }

    #[test]
    fn csr_bin_rejects_truncated() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "graph-compute-csr-trunc-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, [0u8; 4]).unwrap();
        let err = CsrGraph::load_bin(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(format!("{err}").contains("truncated"));
    }

    #[test]
    fn csr_bin_rejects_length_mismatch() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "graph-compute-csr-mismatch-{}.bin",
            std::process::id()
        ));
        // header claims 4 nodes + 8 edges but no payload follows
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let err = CsrGraph::load_bin(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(format!("{err}").contains("length mismatch"));
    }

    #[tokio::test]
    async fn init_engine_with_attributes() {
        let n = 4;
        let graph = CsrGraph::path(n);
        let state = SimState::new(graph);

        // Inject node_class vector
        let attrs = GraphAttributes {
            node_class: Some(vec![1, 2, 1, 2]),
            ..Default::default()
        };

        // Use a test-friendly engine like geometric
        let params = serde_json::json!({
            "class_source": {"kind": "injected"},
        });

        let id = state
            .init_engine("geometric", params, Some(attrs))
            .await
            .expect("init_engine should succeed");

        assert_eq!(id, "geometric");
        let mut active = state.active.lock().await;
        let mut active = active.take().unwrap();
        // Just run one step to prove it didn't panic and accepted the attributes
        let out = active.engine.step(&mut active.ctx);
        assert_eq!(out.positions.len(), (n * 3) as usize);
    }

    #[tokio::test]
    async fn load_graph_swaps_and_engine_reinits_on_new_graph() {
        // Start on a tiny 4-node path, then hot-swap to a 64-node particle soup
        // (0 edges) — the self-assembly LoadGraph path.
        let state = SimState::new(CsrGraph::path(4));
        state.init_engine("geometric", serde_json::Value::Null, None)
            .await
            .expect("init on the original graph");
        assert!(state.active.lock().await.is_some());

        let soup = CsrGraph {
            n_nodes: 64,
            offsets: vec![0u32; 65], // every row empty → no edges
            neighbors: Vec::new(),
        };
        // Deterministic scattered seed positions in a small ball.
        let pos: Vec<f32> = (0..64u64 * 3)
            .map(|i| ((i.wrapping_mul(2654435761) % 1000) as f32) / 100.0 - 5.0)
            .collect();

        let n = state
            .load_graph(soup, Some(pos.clone()))
            .await
            .expect("load_graph");
        assert_eq!(n, 64);
        // Swap is observable: graph + positions replaced, frame reset, active cleared.
        assert_eq!(state.graph.read().await.n_nodes, 64);
        assert_eq!(state.positions.read().await.len(), 64 * 3);
        assert_eq!(*state.frame.read().await, 0);
        assert!(
            state.active.lock().await.is_none(),
            "LoadGraph must clear the active engine so Subscribe re-inits"
        );

        // Re-init on the NEW graph and step: the engine must run on the 64-node
        // soup (not the stale 4-node graph) — the key correctness property.
        state
            .init_engine("geometric", serde_json::Value::Null, None)
            .await
            .expect("re-init on the soup");
        let mut active = state.active.lock().await;
        let mut active = active.take().unwrap();
        let out = active.engine.step(&mut active.ctx);
        assert_eq!(out.positions.len(), 64 * 3, "engine ran on the swapped graph");
        assert!(out.positions.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn from_bin_bytes_roundtrips_to_bin() {
        let g = CsrGraph::path(16);
        let parsed = CsrGraph::from_bin_bytes(&g.to_bin()).expect("from_bin_bytes");
        assert_eq!(parsed.n_nodes, g.n_nodes);
        assert_eq!(parsed.offsets, g.offsets);
        assert_eq!(parsed.neighbors, g.neighbors);
        // A soup (0 edges) also round-trips.
        let soup = CsrGraph {
            n_nodes: 100,
            offsets: vec![0u32; 101],
            neighbors: Vec::new(),
        };
        let parsed = CsrGraph::from_bin_bytes(&soup.to_bin()).expect("soup round-trip");
        assert_eq!(parsed.n_nodes, 100);
        assert!(parsed.neighbors.is_empty());
    }
}

/// The live, initialized layout engine plus the execution context it runs on.
/// Owned by the sim loop (via `SimState`) and taken out of the `Mutex` for the
/// duration of each blocking `step`.
pub struct ActiveEngine {
    pub engine: Box<dyn LayoutEngine>,
    pub ctx: EngineCtx,
}

/// Deterministic unit-ring seed positions for `n` nodes (same convention as
/// crates/graph-api/src/vault_loader.rs). The engine seeds from these at init.
fn ring_seed(n: usize) -> Vec<f32> {
    let mut positions = vec![0.0f32; 3 * n];
    for i in 0..n {
        let t = (i as f32) / (n.max(1) as f32) * std::f32::consts::TAU;
        positions[3 * i] = t.cos();
        positions[3 * i + 1] = t.sin();
        positions[3 * i + 2] = 0.0;
    }
    positions
}

/// State shared between the simulation tick task and the gRPC service.
pub struct SimState {
    /// The active graph. Behind an `RwLock<Arc<…>>` so `LoadGraph` can hot-swap
    /// it: `init_engine` clones the `Arc` (cheap) into the engine context, so a
    /// swap is only observed at the next Subscribe re-init — never mid-step.
    pub graph: RwLock<Arc<CsrGraph>>,
    /// Interleaved x,y,z f32 positions, length `3 * n_nodes`. Host copy that
    /// the active engine seeds from at `init` and that each tick overwrites
    /// with the engine's `StepOutput`.
    pub positions: RwLock<Vec<f32>>,
    pub frame: RwLock<u64>,
    /// Broadcast channel of per-tick `PositionDelta` snapshots. The gRPC
    /// `Subscribe` handler subscribes to this; the simulation tick task is
    /// the sole producer. Lagging subscribers drop frames (log + continue).
    pub tx: broadcast::Sender<PositionDelta>,
    /// The engine registry — built once at startup. `Subscribe` selects an
    /// engine by `layout_id`; today the worker initializes one engine for the
    /// whole process (Phase 1).
    pub registry: EngineRegistry,
    /// The active, initialized engine + its context. `None` until
    /// `init_engine` runs. Taken out of the `Mutex` across each blocking step.
    pub active: Mutex<Option<ActiveEngine>>,
}

impl SimState {
    pub fn new(graph: CsrGraph) -> Arc<Self> {
        let positions = ring_seed(graph.n_nodes as usize);
        // 32-frame ring buffer; bigger than typical RTT so a brief client
        // hiccup doesn't drop a frame.
        let (tx, _rx) = broadcast::channel(32);
        Arc::new(Self {
            graph: RwLock::new(Arc::new(graph)),
            positions: RwLock::new(positions),
            frame: RwLock::new(0),
            tx,
            registry: EngineRegistry::builtin(),
            active: Mutex::new(None),
        })
    }

    /// Hot-swap the active graph (the `LoadGraph` RPC). Replaces the graph +
    /// positions, resets the frame counter, and clears the active engine so the
    /// next Subscribe re-inits it on the new graph. `positions` must be length
    /// `3 * n_nodes` if provided; `None` ⇒ a deterministic ring seed. Returns the
    /// new node count.
    pub async fn load_graph(
        &self,
        graph: CsrGraph,
        positions: Option<Vec<f32>>,
    ) -> Result<u32, String> {
        let n = graph.n_nodes as usize;
        let pos = match positions {
            Some(p) if p.len() == 3 * n => p,
            Some(p) => {
                return Err(format!(
                    "positions length {} != 3 * n_nodes ({})",
                    p.len(),
                    3 * n
                ))
            }
            None => ring_seed(n),
        };
        *self.graph.write().await = Arc::new(graph);
        *self.positions.write().await = pos;
        *self.frame.write().await = 0;
        *self.active.lock().await = None; // next Subscribe re-inits on the new graph
        Ok(n as u32)
    }

    /// Construct, parameterize, and initialize the engine selected by
    /// `layout_id` (empty ⇒ registry default), then install it as the active
    /// engine. Tries wgpu bring-up once; if the requested engine fails `init`
    /// for lack of a GPU, transparently falls back to the `"cpu-spring"`
    /// engine. Returns the `LayoutId` actually running.
    ///
    /// All wgpu device construction + engine `init` run on the blocking pool so
    /// the async runtime isn't stalled.
    pub async fn init_engine(
        self: &Arc<Self>,
        layout_id: &str,
        params: serde_json::Value,
        attributes: Option<GraphAttributes>,
    ) -> Result<&'static str, String> {
        let positions = self.positions.read().await.clone();
        let graph = self.graph.read().await.clone(); // Arc clone — cheap
        // Construct the engine on the async thread (cheap), then move it +
        // context to the blocking pool for init.
        let mut engine = self
            .registry
            .construct(layout_id)
            .ok_or_else(|| format!("unknown layout_id {layout_id:?}"))?;
        engine.set_params(&params)?;
        let chosen_id = engine.descriptor().id;
        let fallback_id = crate::engines::CpuSpringEngine::ID;
        let registry_has_fallback = self.registry.contains(fallback_id);

        let result = tokio::task::spawn_blocking(move || {
            let mut ctx = EngineCtx::try_new_gpu();
            let shard = if let Some(attrs) = &attributes {
                CsrShard::whole_with_attributes(&graph, attrs)
            } else {
                CsrShard::whole(&graph)
            };
            match engine.init(&mut ctx, &shard, &positions) {
                Ok(()) => Ok((ActiveEngine { engine, ctx }, attributes)),
                Err(e) => Err((e, ctx, graph, positions, attributes)),
            }
        })
        .await
        .map_err(|e| format!("engine init task panicked: {e}"))?;

        match result {
            Ok((active, _attrs)) => {
                *self.active.lock().await = Some(active);
                Ok(chosen_id)
            }
            Err((init_err, mut ctx, graph, positions, attributes)) => {
                if chosen_id != fallback_id && registry_has_fallback {
                    tracing::warn!(
                        engine = chosen_id,
                        error = %init_err,
                        "engine init failed; falling back to {fallback_id}"
                    );
                    let mut fallback = self
                        .registry
                        .construct(fallback_id)
                        .expect("fallback engine registered");
                    let shard = if let Some(attrs) = &attributes {
                        CsrShard::whole_with_attributes(&graph, attrs)
                    } else {
                        CsrShard::whole(&graph)
                    };
                    fallback
                        .init(&mut ctx, &shard, &positions)
                        .map_err(|e| format!("fallback engine init failed: {e}"))?;
                    *self.active.lock().await = Some(ActiveEngine {
                        engine: fallback,
                        ctx,
                    });
                    Ok(fallback_id)
                } else {
                    Err(format!("engine {chosen_id} init failed: {init_err}"))
                }
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
