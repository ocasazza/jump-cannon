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

use crate::engines::GraphAttributes as HostGraphAttributes;
use crate::partition::HaloDelta as HostHaloDelta;
use crate::proto::compute_server::Compute;
use crate::proto::{
    CoarsenSettings, EngineDescriptor, FocusRequest, GraphAttributes as ProtoGraphAttributes,
    HaloDelta, HealthRequest, HealthResponse, HybridFrame, ListEnginesRequest, ListEnginesResponse,
    PositionDelta, SubscribeRequest,
};
use crate::sim::{CsrGraph, SimState};
use crate::topo_fisheye::{
    build_hierarchy, build_hybrid, distort_radial, CoarsenParams, DistortParams, HybridParams,
    MatchWeights, TopoHierarchy,
};

/// Source of this worker's outgoing boundary halo for the distributed BSP
/// exchange (docs/compute-architecture.md §4, step c). The `ExchangeHalo` RPC
/// asks the provider, per inbound frame, "what boundary positions do I owe the
/// peer for this frame?" and streams the answer back.
///
/// This is the seam that keeps the gRPC handler honest without baking a full
/// distributed runtime into the service: a real worker installs a provider
/// backed by its [`Partition`](crate::partition::Partition) + live owned
/// positions; the single-process default installs none (the RPC then reports
/// `unimplemented`). `inbound` is the peer's delta for that frame, so a
/// provider may also use it to drive its own `apply_halo` before replying.
pub trait HaloProvider: Send + Sync {
    /// Boundary deltas this worker owns and the peer needs for `frame`. The
    /// peer's inbound delta for the same frame is supplied for context (e.g.
    /// to fold into the local engine before answering). Returning an empty Vec
    /// is legal (this worker owes the peer nothing this frame).
    fn outgoing_for(&self, frame: u64, inbound: &HostHaloDelta) -> Vec<HostHaloDelta>;
}

pub struct ComputeService {
    pub state: Arc<SimState>,
    /// Cache of multilevel hierarchies keyed by graph_id. Built lazily on
    /// the first `TopoFisheye` request for a graph and reused across
    /// subsequent focus changes (and across reconnecting clients) since
    /// the hierarchy depends only on the input topology + initial layout.
    hierarchies: Arc<Mutex<HashMap<String, Arc<TopoHierarchy>>>>,
    /// Distributed BSP halo source for `ExchangeHalo` (doc §4). `None` for the
    /// single-process default — the RPC then returns `unimplemented`.
    halo: Option<Arc<dyn HaloProvider>>,
}

impl ComputeService {
    pub fn new(state: Arc<SimState>) -> Self {
        Self {
            state,
            hierarchies: Arc::new(Mutex::new(HashMap::new())),
            halo: None,
        }
    }

    /// Install a [`HaloProvider`] so this service answers the distributed
    /// `ExchangeHalo` RPC (doc §4). Without one, `ExchangeHalo` is
    /// `unimplemented`.
    pub fn with_halo_provider(mut self, provider: Arc<dyn HaloProvider>) -> Self {
        self.halo = Some(provider);
        self
    }
}

type SubscribeStream =
    Pin<Box<dyn Stream<Item = Result<PositionDelta, Status>> + Send + 'static>>;
type TopoFisheyeStream =
    Pin<Box<dyn Stream<Item = Result<HybridFrame, Status>> + Send + 'static>>;
type ExchangeHaloStream =
    Pin<Box<dyn Stream<Item = Result<HaloDelta, Status>> + Send + 'static>>;

/// String form of a `LayoutKind` for the `EngineDescriptor.kind` wire field
/// (FROZEN CONTRACT: "Physics" | "Static"). The renderer keys its picker off
/// these exact strings.
fn layout_kind_str(kind: graph_layouts::LayoutKind) -> &'static str {
    match kind {
        graph_layouts::LayoutKind::Physics => "Physics",
        graph_layouts::LayoutKind::Static => "Static",
    }
}

/// Decode a proto `HaloDelta` (frame + owner_id + raw-LE byte blobs) into the
/// host-readable [`HostHaloDelta`]. Bulk numeric fields ride raw LE per the
/// repo wire rule; `decode_bytes` enforces alignment + the positions↔node_ids
/// length invariant.
fn proto_to_host(d: HaloDelta) -> Result<HostHaloDelta, Status> {
    HostHaloDelta::decode_bytes(d.frame, d.owner_id, &d.node_ids, &d.positions, d.attributes)
        .map_err(|e| Status::invalid_argument(format!("malformed HaloDelta: {e}")))
}

/// Encode a host [`HostHaloDelta`] into the proto wire form (raw-LE bytes).
fn host_to_proto(d: &HostHaloDelta) -> HaloDelta {
    d.encode_proto()
}

/// Decode a proto `GraphAttributes` into the host `GraphAttributes`.
fn proto_attrs_to_host(a: ProtoGraphAttributes) -> Result<HostGraphAttributes, Status> {
    let cast_u32 = |name: &str, bytes: &[u8]| -> Result<Option<Vec<u32>>, Status> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() % 4 != 0 {
            return Err(Status::invalid_argument(format!(
                "GraphAttributes.{name} misaligned: len {} is not a multiple of 4",
                bytes.len()
            )));
        }
        Ok(Some(bytemuck::cast_slice::<u8, u32>(bytes).to_vec()))
    };
    let cast_f32 = |name: &str, bytes: &[u8]| -> Result<Option<Vec<f32>>, Status> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() % 4 != 0 {
            return Err(Status::invalid_argument(format!(
                "GraphAttributes.{name} misaligned: len {} is not a multiple of 4",
                bytes.len()
            )));
        }
        Ok(Some(bytemuck::cast_slice::<u8, f32>(bytes).to_vec()))
    };

    Ok(HostGraphAttributes {
        node_class: cast_u32("node_class", &a.node_class)?,
        node_coordination: cast_u32("node_coordination", &a.node_coordination)?,
        node_mass: cast_f32("node_mass", &a.node_mass)?,
        edge_len: cast_f32("edge_len", &a.edge_len)?,
    })
}

#[tonic::async_trait]
impl Compute for ComputeService {
    type SubscribeStream = SubscribeStream;
    type TopoFisheyeStream = TopoFisheyeStream;
    type ExchangeHaloStream = ExchangeHaloStream;

    async fn subscribe(
        &self,
        req: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = req.into_inner();

        // ADR-002: a Subscribe may select + tune the layout engine. An empty
        // `layout_id` with no `params` means "leave whatever the worker is
        // already running" (the Phase-1 startup default). Otherwise we (re)init
        // the active engine from the registry. Single-worker model: there is
        // one active engine per process, so a select swaps it for everyone.
        let want_select = !req.layout_id.is_empty() || req.params.is_some();
        if want_select {
            if !req.layout_id.is_empty() && !self.state.registry.contains(&req.layout_id) {
                return Err(Status::invalid_argument(format!(
                    "unknown layout_id {:?}",
                    req.layout_id
                )));
            }
            // google.protobuf.Struct -> serde_json::Value (Null when absent ⇒
            // the engine uses its built-in defaults).
            let params = req
                .params
                .map(struct_to_json)
                .unwrap_or(serde_json::Value::Null);
            let attributes = if let Some(a) = req.attributes {
                Some(proto_attrs_to_host(a)?)
            } else {
                None
            };
            let running = self
                .state
                .init_engine(&req.layout_id, params, attributes)
                .await
                .map_err(|e| Status::internal(format!("engine init failed: {e}")))?;
            tracing::info!(
                requested = %req.layout_id,
                running,
                "Subscribe selected layout engine"
            );
        }

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

    async fn list_engines(
        &self,
        _req: Request<ListEnginesRequest>,
    ) -> Result<Response<ListEnginesResponse>, Status> {
        let engines = self
            .state
            .registry
            .descriptors()
            .iter()
            .map(|d| EngineDescriptor {
                id: d.id.to_string(),
                display_name: d.display_name.to_string(),
                description: d.description.to_string(),
                kind: layout_kind_str(d.kind).to_string(),
            })
            .collect();
        Ok(Response::new(ListEnginesResponse {
            engines,
            default_id: self.state.registry.default_id().to_string(),
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

    /// Distributed BSP halo exchange (doc §4, step c). For each boundary
    /// `HaloDelta` the peer streams in (the positions IT owns for some frame),
    /// this worker replies with the boundary `HaloDelta`s IT owns and the peer
    /// needs for that same frame, sourced from the installed [`HaloProvider`].
    ///
    /// Real transport, not the in-memory double: the bytes cross the tonic
    /// codec both ways. Without a provider installed the RPC is `unimplemented`
    /// (the single-process default uses [`LocalTransport`](crate::partition::LocalTransport)).
    async fn exchange_halo(
        &self,
        req: Request<tonic::Streaming<HaloDelta>>,
    ) -> Result<Response<Self::ExchangeHaloStream>, Status> {
        let provider = self.halo.clone().ok_or_else(|| {
            Status::unimplemented(
                "ExchangeHalo requires a HaloProvider; this worker runs single-process \
                 (LocalTransport). See docs/compute-architecture.md §4.",
            )
        })?;
        let mut in_stream = req.into_inner();

        let out = async_stream::try_stream! {
            while let Some(msg) = in_stream.next().await {
                let proto = msg
                    .map_err(|e| Status::internal(format!("peer halo stream error: {e}")))?;
                let frame = proto.frame;
                let inbound = proto_to_host(proto)?;
                // Reply with what this worker owes the peer for the SAME frame.
                for owed in provider.outgoing_for(frame, &inbound) {
                    yield host_to_proto(&owed);
                }
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

/// Convert a `google.protobuf.Struct` (top-level engine params object) into a
/// `serde_json::Value` so it can be fed to `LayoutEngine::set_params`. This is
/// the receiving half of the ADR-002 `serde_json::Value ↔ prost_types::Struct`
/// mapping; the sending half (`json_to_struct`) lives alongside it for the
/// broker/renderer client path. The two are exact inverses for the JSON value
/// domain (object/array/number/string/bool/null).
pub fn struct_to_json(s: prost_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(
        s.fields
            .into_iter()
            .map(|(k, v)| (k, prost_value_to_json(v)))
            .collect(),
    )
}

/// Inverse of [`struct_to_json`]. A non-object JSON value can't be a protobuf
/// `Struct` (whose top level is always a map), so anything that isn't a JSON
/// object — including `Null` — maps to an empty `Struct`. Callers that want
/// "no params" should send `None` for the field rather than an empty struct,
/// but an empty struct is treated identically (engine defaults) on receipt.
pub fn json_to_struct(v: serde_json::Value) -> prost_types::Struct {
    match v {
        serde_json::Value::Object(map) => prost_types::Struct {
            fields: map
                .into_iter()
                .map(|(k, v)| (k, json_to_prost_value(v)))
                .collect(),
        },
        _ => prost_types::Struct::default(),
    }
}

fn prost_value_to_json(v: prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match v.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s),
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(b),
        Some(Kind::StructValue(s)) => struct_to_json(s),
        Some(Kind::ListValue(l)) => serde_json::Value::Array(
            l.values.into_iter().map(prost_value_to_json).collect(),
        ),
    }
}

fn json_to_prost_value(v: serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(b),
        // serde_json numbers (incl. integers) collapse to f64 — the only
        // numeric type protobuf's Value carries. Lossless for the f32/u32
        // engine settings we transport (well within f64's exact-int range).
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s),
        serde_json::Value::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.into_iter().map(json_to_prost_value).collect(),
        }),
        serde_json::Value::Object(m) => Kind::StructValue(prost_types::Struct {
            fields: m
                .into_iter()
                .map(|(k, v)| (k, json_to_prost_value(v)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

/// Drive the simulation forward by stepping the active layout engine from the
/// registry (ADR-001). The engine — wgpu FA2 (`"fa2-brute"`) or the CPU spring
/// fallback (`"cpu-spring"`) — is chosen by `SimState::init_engine`; this loop
/// is engine-agnostic. If no engine has been initialized yet (or it self-halts)
/// the loop ticks idle without producing frames.
pub async fn run_sim_loop(state: Arc<SimState>, _tick_hz: f32) {
    let dt = 1.0 / _tick_hz.max(1.0);
    let period = Duration::from_secs_f32(dt);
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        // Take the active engine out of the Mutex for the duration of the
        // blocking step (so the lock doesn't sit across the join) and put it
        // back afterwards. There's exactly one tick task, so taking it here is
        // contention-free. If no engine is installed yet, skip this tick.
        let taken = state.active.lock().await.take();
        let mut active = match taken {
            Some(a) => a,
            None => continue,
        };
        if active.engine.is_halted() {
            // Engine converged: reinstall and idle (still answer Subscribe with
            // the last broadcast frame).
            *state.active.lock().await = Some(active);
            continue;
        }

        let (output, active) = tokio::task::spawn_blocking(move || {
            let out = active.engine.step(&mut active.ctx);
            (out, active)
        })
        .await
        .expect("engine step panicked");
        *state.active.lock().await = Some(active);
        let new_positions = output.positions;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proto_attrs_to_host() {
        // Valid case
        let valid = ProtoGraphAttributes {
            node_class: vec![1, 0, 0, 0], // one u32
            node_coordination: vec![], // empty is fine
            node_mass: vec![0, 0, 128, 63], // 1.0f32
            edge_len: vec![0, 0, 0, 64], // 2.0f32
        };
        let host = proto_attrs_to_host(valid).expect("should succeed");
        assert_eq!(host.node_class, Some(vec![1]));
        assert_eq!(host.node_coordination, None);
        assert_eq!(host.node_mass, Some(vec![1.0]));
        assert_eq!(host.edge_len, Some(vec![2.0]));

        // Misaligned case
        let misaligned = ProtoGraphAttributes {
            node_class: vec![1, 0, 0], // 3 bytes, not multiple of 4
            ..Default::default()
        };
        let err = proto_attrs_to_host(misaligned).unwrap_err();
        assert_eq!(err.code(), Status::invalid_argument("").code());
        assert!(err.message().contains("misaligned"));
    }
}
