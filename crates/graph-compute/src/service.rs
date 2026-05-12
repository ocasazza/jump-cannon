//! tonic gRPC service implementation.
//!
//! `Compute::Subscribe` returns a server-streaming `PositionDelta` channel
//! backed by `tokio::sync::broadcast`. The simulation tick task is the sole
//! producer; clients subscribing late simply pick up at the next frame.
//!
//! `Compute::TopoFisheye` is a bidirectional stream: each client message
//! requests a fresh topological-fisheye view focused at a given node, and
//! the server replies with one `HybridFrame` per message. The expensive
//! multilevel hierarchy is built once per `graph_id` and cached on the
//! service struct, so subsequent focus changes only pay for the hybrid
//! graph construction + distortion (paper §5–6), which is `O(m log m)`
//! in the hybrid-graph size.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::proto::compute_server::Compute;
use crate::proto::{
    CoarsenSettings, FocusRequest, HealthRequest, HealthResponse, HybridFrame, PositionDelta,
    SubscribeRequest,
};
use crate::sim::{cpu_step, CsrGraph, SimState};
use crate::topo_fisheye::{
    build_hierarchy, build_hybrid, distort_radial, CoarsenParams, DistortParams, HybridParams,
    MatchWeights, TopoHierarchy,
};

pub struct ComputeService {
    pub state: Arc<SimState>,
    /// Cache of multilevel hierarchies keyed by graph_id. Built lazily on
    /// the first `TopoFisheye` request for a graph and reused across
    /// subsequent focus changes (and across reconnecting clients) since
    /// the hierarchy depends only on the input topology + initial layout.
    hierarchies: Arc<Mutex<HashMap<String, Arc<TopoHierarchy>>>>,
}

impl ComputeService {
    pub fn new(state: Arc<SimState>) -> Self {
        Self {
            state,
            hierarchies: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

type SubscribeStream =
    Pin<Box<dyn Stream<Item = Result<PositionDelta, Status>> + Send + 'static>>;
type TopoFisheyeStream =
    Pin<Box<dyn Stream<Item = Result<HybridFrame, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Compute for ComputeService {
    type SubscribeStream = SubscribeStream;
    type TopoFisheyeStream = TopoFisheyeStream;

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

    async fn topo_fisheye(
        &self,
        req: Request<tonic::Streaming<FocusRequest>>,
    ) -> Result<Response<Self::TopoFisheyeStream>, Status> {
        let state = self.state.clone();
        let hierarchies = self.hierarchies.clone();
        let mut in_stream = req.into_inner();

        let out = async_stream::try_stream! {
            while let Some(focus) = in_stream.next().await {
                let focus = focus.map_err(|e| Status::internal(format!("client stream error: {e}")))?;

                // Look up or build the hierarchy for this graph_id.
                let h = ensure_hierarchy(&state, &hierarchies, &focus).await
                    .map_err(|e| Status::internal(e))?;

                let frame = build_frame(&h, &focus);
                yield frame;
            }
        };
        Ok(Response::new(Box::pin(out)))
    }
}

/// Lazily build (or fetch from cache) the multilevel hierarchy for the
/// graph named in `focus.graph_id`. Empty graph_id is treated as the
/// implicit "the single graph this server is hosting" key.
async fn ensure_hierarchy(
    state: &Arc<SimState>,
    cache: &Arc<Mutex<HashMap<String, Arc<TopoHierarchy>>>>,
    focus: &FocusRequest,
) -> Result<Arc<TopoHierarchy>, String> {
    let key = focus.graph_id.clone();
    {
        let guard = cache.lock().await;
        if let Some(h) = guard.get(&key) {
            return Ok(h.clone());
        }
    }
    // Snapshot the current positions + graph, then build off-runtime.
    let positions = state.positions.read().await.clone();
    let graph = state.graph.clone();
    let coarsen = coarsen_params_from_proto(&focus.coarsen);
    let h = tokio::task::spawn_blocking(move || {
        let edges = csr_to_edge_pairs(&graph);
        build_hierarchy(graph.n_nodes as usize, &edges, &positions, &coarsen)
    })
    .await
    .map_err(|e| format!("hierarchy build task panicked: {e}"))?;
    let arc = Arc::new(h);
    cache.lock().await.insert(key, arc.clone());
    Ok(arc)
}

/// Run §5 + §6 and pack the result into a `HybridFrame`. Pure CPU; cheap
/// enough to run inline on the tokio runtime for typical interactive
/// graph sizes (paper notes §6 is `O(m log m)` in the hybrid-graph size).
fn build_frame(h: &TopoHierarchy, focus: &FocusRequest) -> HybridFrame {
    let mut hybrid = build_hybrid(
        h,
        &HybridParams {
            focal_node: focus.focal_node,
            capacities: focus.capacities.clone(),
        },
    );

    // Focus XY = position of the focal node at level 0 (the paper's centre).
    let l0 = &h.levels[0];
    let f = focus.focal_node as usize;
    let focus_xy = if f < l0.n_nodes {
        [l0.positions[3 * f], l0.positions[3 * f + 1]]
    } else {
        [0.0, 0.0]
    };
    distort_radial(
        &mut hybrid,
        &DistortParams {
            alpha: if focus.alpha > 0.0 { focus.alpha } else { 1.0 },
            focus_xy,
            smoothing_window: 20,
        },
    );

    let n_nodes = hybrid.nodes.len() as u32;
    let n_edges = (hybrid.edges.len() / 2) as u32;

    // Pack (level, idx) pairs as u32×2 LE.
    let mut node_refs = Vec::with_capacity(8 * hybrid.nodes.len());
    for &(lvl, idx) in &hybrid.nodes {
        node_refs.extend_from_slice(&lvl.to_le_bytes());
        node_refs.extend_from_slice(&idx.to_le_bytes());
    }

    HybridFrame {
        n_nodes,
        node_refs,
        node_levels: bytemuck::cast_slice::<u32, u8>(&hybrid.node_levels).to_vec(),
        positions: bytemuck::cast_slice::<f32, u8>(&hybrid.positions).to_vec(),
        edges: bytemuck::cast_slice::<u32, u8>(&hybrid.edges).to_vec(),
        edge_levels: bytemuck::cast_slice::<u32, u8>(&hybrid.edge_levels).to_vec(),
        n_edges,
    }
}

fn coarsen_params_from_proto(p: &Option<CoarsenSettings>) -> CoarsenParams {
    let mut out = CoarsenParams::default();
    let p = match p {
        Some(p) => p,
        None => return out,
    };
    if p.max_levels > 0 {
        out.max_levels = p.max_levels as usize;
    }
    if p.target_size > 0 {
        out.target_size = p.target_size as usize;
    }
    if p.gt_max_hops > 0 {
        out.gt_max_hops = p.gt_max_hops;
    }
    let w = &mut out.weights;
    let any = p.w_proximity != 0.0
        || p.w_size != 0.0
        || p.w_connection != 0.0
        || p.w_neighborhood != 0.0
        || p.w_degree != 0.0;
    if any {
        *w = MatchWeights {
            w_proximity: p.w_proximity,
            w_size: p.w_size,
            w_connection: p.w_connection,
            w_neighborhood: p.w_neighborhood,
            w_degree: p.w_degree,
        };
    }
    out
}

/// Convert a CSR adjacency into the flat undirected edge-pair list the
/// topo-fisheye coarsener expects. We emit each undirected edge exactly
/// once (`src < dst`) to avoid double-counting in candidate generation.
fn csr_to_edge_pairs(g: &CsrGraph) -> Vec<u32> {
    let mut out = Vec::with_capacity(g.neighbors.len());
    for src in 0..g.n_nodes {
        let lo = g.offsets[src as usize] as usize;
        let hi = g.offsets[src as usize + 1] as usize;
        for &dst in &g.neighbors[lo..hi] {
            if src < dst {
                out.push(src);
                out.push(dst);
            }
        }
    }
    out
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
