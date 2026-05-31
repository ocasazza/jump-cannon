//! Server-side layout-engine registry (ADR-001).
//!
//! `graph-compute` used to *be* one hardcoded ForceAtlas2 sim. This module
//! lifts the algorithm behind a `LayoutEngine` trait so the worker holds a
//! **map of selectable engines** and the sim loop drives whichever one a
//! `Subscribe` chose. The design and the rejected alternatives are recorded in
//! `docs/compute-architecture.md` (ADR-001: a `graph-compute`-owned trait that
//! *reuses* the shared `LayoutDescriptor`/`LayoutId` vocabulary from
//! `graph-layouts::layout::layout_trait` rather than forking it).
//!
//! ## Scale-out-first by construction
//!
//! The trait is shaped for the distributed model from day one (doc §4):
//!
//!   - `init` takes a [`CsrShard`], not a bare graph. `shard == None` ⇒ this
//!     worker owns the *whole* graph (the only case implemented today).
//!     `shard == Some` ⇒ it owns a CSR partition plus a ghost-node table.
//!   - `step` returns a [`StepOutput`] carrying the OWNED nodes' host-readable
//!     positions for broadcast (and, eventually, the boundary slice to ship to
//!     peers).
//!   - `apply_halo` is the distributed hook — a no-op for the single-worker
//!     case; the BSP superstep phase fills it in. `HaloUpdate` is a minimal
//!     placeholder for now.
//!
//! Both CPU and GPU engines share one trait because [`EngineCtx`] carries an
//! *optional* wgpu `Device`/`Queue`: a GPU engine asserts it on `init`, a CPU
//! engine ignores it.

use std::collections::HashMap;

use graph_layouts::{LayoutDescriptor, LayoutId};

use crate::sim::CsrGraph;

pub mod cpu_spring;
pub mod fa2_bh;
pub mod fa2_brute;
pub mod geometric;
pub mod geometric_gpu;
pub mod multilevel;
pub mod sgd_stress;
pub mod sgd_stress_gpu;

pub use cpu_spring::CpuSpringEngine;
pub use fa2_bh::Fa2BhEngine;
pub use fa2_brute::Fa2BruteEngine;
pub use geometric::{EnergyBreakdown, GeometricEngine, GeometricObservables, GeometricSettings};
pub use geometric_gpu::GeometricGpuEngine;
pub use multilevel::{MultilevelEngine, MultilevelSettings, SweepSchedule};
pub use sgd_stress::SgdStressEngine;
pub use sgd_stress_gpu::SgdStressGpuEngine;

/// Per-worker execution context shared by all engines.
///
/// Carries the *optional* wgpu device + queue so a single trait covers both
/// CPU engines (which ignore it) and GPU engines (which require it). The sim
/// loop builds one `EngineCtx` at startup — attempting wgpu bring-up once — and
/// hands it to `init`/`step` by `&mut`.
pub struct EngineCtx {
    /// `Some` once a wgpu adapter+device were acquired; `None` on hosts without
    /// a usable adapter (CI, headless). GPU engines should fail `init` cleanly
    /// when this is `None` so the caller can fall back to a CPU engine.
    pub gpu: Option<GpuCtx>,
}

/// The wgpu device + queue + adapter info, present only when GPU bring-up
/// succeeded. The device/queue are `Arc`-wrapped (wgpu 23's `Device`/`Queue`
/// are not themselves `Clone`) so an engine can cheaply stash its own handle at
/// `init` time and the context can be cloned/shared.
#[derive(Clone)]
pub struct GpuCtx {
    pub device: std::sync::Arc<wgpu::Device>,
    pub queue: std::sync::Arc<wgpu::Queue>,
    pub adapter_info: wgpu::AdapterInfo,
}

impl EngineCtx {
    /// CPU-only context (no GPU). Used by tests and headless hosts.
    pub fn cpu_only() -> Self {
        Self { gpu: None }
    }

    /// Attempt to bring up a wgpu device. On success returns a context with a
    /// populated `gpu` field; on failure (no adapter / no ICD) returns a
    /// CPU-only context. This is the single GPU-init point for the worker — the
    /// per-engine `WgpuSim`-style adapter request used to live in `wgpu_sim.rs`.
    pub fn try_new_gpu() -> Self {
        match build_gpu_ctx() {
            Ok(gpu) => {
                tracing::info!(
                    backend = ?gpu.adapter_info.backend,
                    name = %gpu.adapter_info.name,
                    device_type = ?gpu.adapter_info.device_type,
                    "wgpu adapter initialized; GPU layout engines available"
                );
                Self { gpu: Some(gpu) }
            }
            Err(e) => {
                tracing::warn!(error = %e, "wgpu adapter not found; CPU engines only");
                Self { gpu: None }
            }
        }
    }
}

fn build_gpu_ctx() -> anyhow::Result<GpuCtx> {
    use anyhow::{anyhow, Context};

    // Vendor-agnostic by construction: wgpu picks the backend per platform, so
    // the same code drives Apple GPUs (Metal), and NVIDIA/AMD/Intel via Vulkan
    // (Linux), DX12 (Windows), or the Metal driver (Intel Macs). We default to
    // `all()` (PRIMARY + the GL secondary fallback for older NVIDIA/AMD without
    // Vulkan) and let `WGPU_BACKEND` override it. `HighPerformance` prefers the
    // discrete GPU when an integrated one is also present (e.g. NVIDIA/AMD over
    // an iGPU); `WGPU_POWER_PREF` can override.
    let backends =
        wgpu::util::backend_bits_from_env().unwrap_or_else(wgpu::Backends::all);
    let power_preference = wgpu::util::power_preference_from_env()
        .unwrap_or(wgpu::PowerPreference::HighPerformance);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok_or_else(|| anyhow!("no wgpu adapter available (backends: {backends:?})"))?;

    let adapter_info = adapter.get_info();

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("graph-compute-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_storage_buffers_per_shader_stage: 12,
                ..wgpu::Limits::default()
            },
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .context("failed to request wgpu device")?;

    Ok(GpuCtx {
        device: std::sync::Arc::new(device),
        queue: std::sync::Arc::new(queue),
        adapter_info,
    })
}

/// A graph partition assigned to one worker.
///
/// `graph` is always the CSR this worker integrates. The optional metadata
/// describes how it relates to the global graph in a sharded deployment:
///
///   - `shard == None`  ⇒ single worker owns the whole graph (today's case).
///   - `shard == Some`  ⇒ this is partition `p`; `owned_node_ids` are the nodes
///     this worker integrates, and `ghost_node_ids` are read-only copies of
///     neighboring partitions' boundary nodes whose positions arrive via
///     [`LayoutEngine::apply_halo`].
///
/// The distributed phase (doc §4/§6) is what populates `ShardMeta`; engines
/// written today can ignore it and treat `graph` as the full graph.
pub struct CsrShard<'a> {
    pub graph: &'a CsrGraph,
    pub shard: Option<ShardMeta>,
    /// Optional per-node / per-edge attribute vectors injected from upstream
    /// (the metadata-rich frontend), parallel to `graph`. CSR itself is pure
    /// topology — tags, types, weights, and precomputed community/centrality
    /// never reach this worker through the CSR buffer — so any engine that wants
    /// to drive geometry from *semantic* attributes (rather than topology alone)
    /// reads them here. `None` ⇒ no injected attributes; engines fall back to
    /// structural sources they can derive from `graph` itself (degree,
    /// label-propagation community, PageRank). See [`GraphAttributes`] and
    /// [`geometric::GeometricEngine`].
    pub attributes: Option<&'a GraphAttributes>,
}

impl<'a> CsrShard<'a> {
    /// Wrap a whole graph as an unsharded (single-worker) shard with no injected
    /// attributes.
    pub fn whole(graph: &'a CsrGraph) -> Self {
        Self {
            graph,
            shard: None,
            attributes: None,
        }
    }

    /// Wrap a whole graph together with injected attribute vectors (the wire
    /// path for semantic, frontend-resolved attributes — see [`GraphAttributes`]).
    pub fn whole_with_attributes(graph: &'a CsrGraph, attributes: &'a GraphAttributes) -> Self {
        Self {
            graph,
            shard: None,
            attributes: Some(attributes),
        }
    }
}

/// Injected per-node / per-edge attribute vectors that travel *alongside* the
/// CSR topology (doc §3, the wire extension). Each field is independently
/// optional: an engine resolves the attributes it needs either from here (when
/// present) or from a structural source it derives from the graph.
///
/// Why this exists: the CSR buffer is pure topology (`offsets` + `neighbors`).
/// Semantic attributes — a node's tag/type/community, an edge's weight/type —
/// live only in the frontend's rich graph model. The frontend resolves a user's
/// chosen mapping (e.g. "community = the `folder` frontmatter field",
/// "edge length = `weight`") into these compact numeric vectors and ships them
/// raw (little-endian, per the repo wire rule) for the backend solver to consume
/// without ever knowing what "folder" meant. A molecular force field is the same
/// mechanism with `node_class = element` and `edge_len = bond length`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphAttributes {
    /// Per-node class id, length `n_nodes`. Indexes the geometric class table
    /// (exclusion radius + inter-class affinity). Frontend-resolved from
    /// community / tag / type.
    pub node_class: Option<Vec<u32>>,
    /// Per-node coordination-geometry id, length `n_nodes`. Indexes the
    /// preferred-angle table. Frontend-resolved (usually from degree, but any
    /// attribute can drive it).
    pub node_coordination: Option<Vec<u32>>,
    /// Per-node mass, length `n_nodes`. Scales gravity pull + integration
    /// inertia. Frontend-resolved from centrality (PageRank/degree/…).
    pub node_mass: Option<Vec<f32>>,
    /// Per-edge target length, **parallel to `graph.neighbors`** (length
    /// `neighbors.len()`): `edge_len[e]` is the rest length of the CSR entry at
    /// neighbor index `e`. Using the canonical `neighbors` order (rather than a
    /// synthesized unique-edge order) keeps the mapping unambiguous across the
    /// wire. Frontend-resolved from edge weight / type.
    pub edge_len: Option<Vec<f32>>,
}

impl GraphAttributes {
    /// Validate that every present vector has the length its kind requires for
    /// `graph`. Returns `Err` with a human-readable mismatch on the first bad
    /// field. An all-`None` `GraphAttributes` is always valid.
    pub fn validate(&self, graph: &CsrGraph) -> Result<(), String> {
        let n = graph.n_nodes as usize;
        let m = graph.neighbors.len();
        let check_node = |name: &str, len: usize| -> Result<(), String> {
            if len != n {
                Err(format!(
                    "GraphAttributes.{name} length {len} != n_nodes {n}"
                ))
            } else {
                Ok(())
            }
        };
        if let Some(v) = &self.node_class {
            check_node("node_class", v.len())?;
        }
        if let Some(v) = &self.node_coordination {
            check_node("node_coordination", v.len())?;
        }
        if let Some(v) = &self.node_mass {
            check_node("node_mass", v.len())?;
        }
        if let Some(v) = &self.edge_len {
            if v.len() != m {
                return Err(format!(
                    "GraphAttributes.edge_len length {} != neighbors.len() {m}",
                    v.len()
                ));
            }
        }
        Ok(())
    }
}

/// Partition / ghost metadata for a sharded deployment. Placeholder for the
/// distributed phase — present so the trait signature is stable now.
#[derive(Clone, Debug, Default)]
pub struct ShardMeta {
    /// This worker's partition index in `[0, n_partitions)`.
    pub partition_id: u32,
    pub n_partitions: u32,
    /// Global node ids this worker owns (integrates + broadcasts).
    pub owned_node_ids: Vec<u32>,
    /// Global node ids this worker holds read-only copies of (peer boundaries).
    pub ghost_node_ids: Vec<u32>,
}

/// One tick's result: the OWNED nodes' positions, host-readable, ready for the
/// broadcast `PositionDelta`. In the unsharded case this is every node; when
/// sharded it is just this partition's owned set (in `owned_node_ids` order),
/// and `boundary` carries the slice peers need.
pub struct StepOutput {
    /// Interleaved `x,y,z` f32 for the owned nodes — what the sim loop packs
    /// into a `PositionDelta`.
    pub positions: Vec<f32>,
    /// Boundary positions to ship to peers, if this engine produced any.
    /// Always `None` in the single-worker case. The distributed phase turns
    /// this into a `HaloDelta` on the wire (doc §4).
    pub boundary: Option<HaloUpdate>,
}

impl StepOutput {
    /// Convenience for the unsharded path: just owned positions, no halo.
    pub fn positions_only(positions: Vec<f32>) -> Self {
        Self {
            positions,
            boundary: None,
        }
    }
}

/// Boundary-position exchange payload (doc §4). Minimal placeholder for the
/// distributed phase: a generalization of `PositionDelta` to a labeled subset
/// of nodes. Single-worker engines neither produce nor consume these.
#[derive(Clone, Debug, Default)]
pub struct HaloUpdate {
    pub frame: u64,
    /// Partition id of the worker that owns `node_ids`.
    pub owner_id: u32,
    /// Global ids of the nodes whose positions follow.
    pub node_ids: Vec<u32>,
    /// Interleaved `x,y,z` f32, parallel to `node_ids`.
    pub positions: Vec<f32>,
    /// Optional attributes for the halo nodes (e.g. class/mass needed for
    /// repulsion).
    pub attributes: Option<GraphAttributes>,
}

/// A server-side layout solver. Scale-out-first: see the module docs and
/// `docs/compute-architecture.md` ADR-001.
///
/// Lifecycle: `set_params` (optional) → `init` (once, with the shard + seed
/// positions) → `step` (per tick) ⟲, with `apply_halo` interleaved in the
/// distributed case.
pub trait LayoutEngine: Send + Sync {
    /// Stable identity + UI metadata. Reuses `graph-layouts`' shared
    /// `LayoutDescriptor` so the renderer's layout picker is engine-location
    /// agnostic (in-process vs. remote).
    fn descriptor(&self) -> &LayoutDescriptor;

    /// Apply dynamic settings (the `google.protobuf.Struct` from a
    /// `SubscribeRequest`, decoded to `serde_json::Value`). Engines deserialize
    /// into their typed settings struct and `Err` on malformed input — the
    /// boundary where a bad request is rejected (ADR-002). Default: accept and
    /// ignore (engines with no tunables).
    fn set_params(&mut self, _params: &serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    /// Bind the engine to a shard + seed positions and build any GPU/CPU state.
    /// `positions` is interleaved `x,y,z`, length `3 * graph.n_nodes`. GPU
    /// engines should `Err` cleanly when `ctx.gpu` is `None`.
    fn init(
        &mut self,
        ctx: &mut EngineCtx,
        graph: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String>;

    /// Re-bind an *already-initialized* engine to a different graph + seed
    /// positions, reusing the live instance instead of constructing a fresh one.
    ///
    /// This is the hook the multilevel wrapper uses to drive a SINGLE inner
    /// engine across the cascade's levels (coarsest → finest): each level has a
    /// different graph and a different node count, but logically it is the same
    /// solver continuing the descent. Reconstructing a fresh engine per level
    /// throws away any reusable state and (for GPU engines) forces a full buffer
    /// rebuild every level.
    ///
    /// The default implementation simply forwards to [`init`], so existing
    /// engines need no change and observe identical behavior to a fresh
    /// construct-then-`init`. GPU engines may later override this to resize /
    /// reuse their buffers in place rather than tearing them down (doc §4).
    fn reinit(
        &mut self,
        ctx: &mut EngineCtx,
        graph: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        self.init(ctx, graph, positions)
    }

    /// Advance one tick and return the owned nodes' host-readable positions for
    /// broadcast (plus the boundary slice when sharded).
    fn step(&mut self, ctx: &mut EngineCtx) -> StepOutput;

    /// Distributed hook: fold in peer boundary positions. No-op for the
    /// single-worker case.
    fn apply_halo(&mut self, _halo: &HaloUpdate) {}

    /// Whether the layout has converged and the sim loop may idle. Default
    /// `false` (continuous physics never self-halts).
    fn is_halted(&self) -> bool {
        false
    }
}

/// Constructor for a registered engine. Called once per worker to mint a fresh,
/// uninitialized engine instance (before `set_params`/`init`).
pub type EngineConstructor = fn() -> Box<dyn LayoutEngine>;

/// The stable id of every NON-multilevel ("leaf") engine this worker ships, in
/// registration order. This is the SINGLE source of truth for "which engines can
/// be wrapped / registered as a leaf" — both [`EngineRegistry::builtin`] and the
/// `multilevel` wrapper derive their tables from [`construct_leaf`], so there is
/// no hand-maintained second copy to drift out of sync.
pub const LEAF_ENGINE_IDS: &[LayoutId] = &[
    Fa2BruteEngine::ID,
    CpuSpringEngine::ID,
    Fa2BhEngine::ID,
    SgdStressEngine::ID,
    SgdStressGpuEngine::ID,
    GeometricEngine::ID,
    GeometricGpuEngine::ID,
];

/// Construct a fresh, uninitialized **leaf** (non-multilevel) engine by id.
///
/// This is the ONE place that maps a leaf engine id to its constructor.
/// `EngineRegistry::builtin` builds its leaf entries from here, and the
/// `multilevel` wrapper calls this to mint its inner solver — neither keeps its
/// own copy of the table, so adding a new leaf engine here registers it
/// everywhere at once.
///
/// `multilevel` itself is intentionally NOT constructible here: it is a wrapper,
/// not a leaf, and excluding it both prevents recursive self-wrapping and avoids
/// any registry↔multilevel construction cycle. Returns `None` for `"multilevel"`
/// and for unknown ids.
pub fn construct_leaf(id: &str) -> Option<Box<dyn LayoutEngine>> {
    match id {
        Fa2BruteEngine::ID => Some(Box::new(Fa2BruteEngine::new())),
        CpuSpringEngine::ID => Some(Box::new(CpuSpringEngine::new())),
        Fa2BhEngine::ID => Some(Box::new(Fa2BhEngine::new())),
        SgdStressEngine::ID => Some(Box::new(SgdStressEngine::new())),
        SgdStressGpuEngine::ID => Some(Box::new(SgdStressGpuEngine::new())),
        GeometricEngine::ID => Some(Box::new(GeometricEngine::new())),
        GeometricGpuEngine::ID => Some(Box::new(GeometricGpuEngine::new())),
        _ => None,
    }
}

/// Map a leaf engine id to a non-capturing [`EngineConstructor`] fn pointer for
/// the registry.
///
/// `EngineConstructor` is a bare `fn()` (no captures), so the registry can't
/// store a closure over `id`. To keep [`construct_leaf`] the SOLE place that
/// actually calls each engine's `::new`, each arm here just forwards to
/// `construct_leaf` with its own id and unwraps (the id is statically known to
/// be a leaf). No `::new` calls are duplicated; only the id↔fn pairing lives
/// here, and it is exhaustively checked against [`LEAF_ENGINE_IDS`] in tests.
/// Panics on an unknown id (callers pass ids from `LEAF_ENGINE_IDS`).
fn leaf_ctor_for(id: &str) -> EngineConstructor {
    match id {
        Fa2BruteEngine::ID => || construct_leaf(Fa2BruteEngine::ID).unwrap(),
        CpuSpringEngine::ID => || construct_leaf(CpuSpringEngine::ID).unwrap(),
        Fa2BhEngine::ID => || construct_leaf(Fa2BhEngine::ID).unwrap(),
        SgdStressEngine::ID => || construct_leaf(SgdStressEngine::ID).unwrap(),
        SgdStressGpuEngine::ID => || construct_leaf(SgdStressGpuEngine::ID).unwrap(),
        GeometricEngine::ID => || construct_leaf(GeometricEngine::ID).unwrap(),
        GeometricGpuEngine::ID => || construct_leaf(GeometricGpuEngine::ID).unwrap(),
        _ => panic!("leaf_ctor_for: unknown leaf engine id {id:?}"),
    }
}

/// The worker's engine registry: a `LayoutId -> constructor` map built once at
/// startup. `Subscribe` selects an engine by `layout_id`; the sim loop calls
/// the constructor, then `set_params`/`init`/`step`.
pub struct EngineRegistry {
    constructors: HashMap<LayoutId, EngineConstructor>,
    /// Cached descriptors (built by invoking each constructor once) so callers
    /// can enumerate available engines without holding live instances.
    descriptors: Vec<LayoutDescriptor>,
    /// Registry key returned when a `Subscribe` omits `layout_id`.
    default_id: LayoutId,
}

impl EngineRegistry {
    /// Build the registry with every engine this worker ships. `default_id`
    /// must be one of the registered ids — used when a request omits a
    /// `layout_id`.
    pub fn new(entries: &[(LayoutId, EngineConstructor)], default_id: LayoutId) -> Self {
        let mut constructors = HashMap::new();
        let mut descriptors = Vec::new();
        for &(id, ctor) in entries {
            let probe = ctor();
            descriptors.push(probe.descriptor().clone());
            constructors.insert(id, ctor);
        }
        debug_assert!(
            constructors.contains_key(default_id),
            "default engine id {default_id:?} not registered"
        );
        Self {
            constructors,
            descriptors,
            default_id,
        }
    }

    /// The built-in registry: every leaf engine (the ported brute-force FA2
    /// `"fa2-brute"` default, the CPU spring fallback `"cpu-spring"`, BH FA2, and
    /// SGD stress) plus the `multilevel` wrapper.
    ///
    /// The leaf entries are NOT spelled out here — they are derived from
    /// [`LEAF_ENGINE_IDS`] / [`construct_leaf`] so the registry and the
    /// `multilevel` wrapper share one constructor table and cannot drift apart.
    /// Only the `multilevel` wrapper (which `construct_leaf` deliberately refuses
    /// to build) is registered explicitly.
    pub fn builtin() -> Self {
        let mut constructors: HashMap<LayoutId, EngineConstructor> = HashMap::new();
        let mut descriptors = Vec::new();

        // Leaf engines: single source of truth is `construct_leaf`. We probe each
        // id once for its descriptor; the stored constructor re-routes through
        // `construct_leaf` (every id here is guaranteed constructible).
        for &id in LEAF_ENGINE_IDS {
            let probe = construct_leaf(id)
                .unwrap_or_else(|| panic!("LEAF_ENGINE_IDS lists unconstructible id {id:?}"));
            descriptors.push(probe.descriptor().clone());
            let ctor: EngineConstructor = leaf_ctor_for(id);
            constructors.insert(id, ctor);
        }

        // The multilevel wrapper is not a leaf and not built by `construct_leaf`.
        let ml = Box::new(MultilevelEngine::new()) as Box<dyn LayoutEngine>;
        descriptors.push(ml.descriptor().clone());
        constructors.insert(MultilevelEngine::ID, || {
            Box::new(MultilevelEngine::new()) as Box<dyn LayoutEngine>
        });

        let default_id = Fa2BruteEngine::ID;
        debug_assert!(constructors.contains_key(default_id));
        Self {
            constructors,
            descriptors,
            default_id,
        }
    }

    pub fn default_id(&self) -> LayoutId {
        self.default_id
    }

    /// Descriptors for every registered engine (for an enumeration RPC / UI).
    pub fn descriptors(&self) -> &[LayoutDescriptor] {
        &self.descriptors
    }

    /// Whether an id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.constructors.keys().any(|&k| k == id)
    }

    /// Construct a fresh engine for `layout_id`. An empty id selects the
    /// default. Returns `None` if the id is unknown.
    pub fn construct(&self, layout_id: &str) -> Option<Box<dyn LayoutEngine>> {
        let id = if layout_id.is_empty() {
            self.default_id
        } else {
            *self.constructors.keys().find(|&&k| k == layout_id)?
        };
        self.constructors.get(id).map(|ctor| ctor())
    }
}

impl Default for EngineRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `construct_leaf` is the single source of truth for leaf engines: every id
    /// in `LEAF_ENGINE_IDS` must be constructible, and the constructed engine's
    /// descriptor id must round-trip back to the same id.
    #[test]
    fn construct_leaf_covers_all_leaf_ids() {
        for &id in LEAF_ENGINE_IDS {
            let engine = construct_leaf(id)
                .unwrap_or_else(|| panic!("construct_leaf returned None for listed id {id:?}"));
            assert_eq!(
                engine.descriptor().id,
                id,
                "leaf engine descriptor id must match its registry id"
            );
        }
    }

    /// `multilevel` is a wrapper, not a leaf: `construct_leaf` must refuse it
    /// (this both prevents recursive self-wrapping and avoids a registry↔
    /// multilevel construction cycle).
    #[test]
    fn construct_leaf_rejects_multilevel_and_unknown() {
        assert!(construct_leaf(MultilevelEngine::ID).is_none());
        assert!(construct_leaf("no-such-engine").is_none());
    }

    /// The builtin registry derives its leaf entries from the same source, so it
    /// registers exactly the leaf ids plus the multilevel wrapper — no more, no
    /// less. Guards against the registry table drifting from `construct_leaf`.
    #[test]
    fn builtin_registers_leaves_plus_multilevel() {
        let reg = EngineRegistry::builtin();
        for &id in LEAF_ENGINE_IDS {
            assert!(reg.contains(id), "builtin registry missing leaf {id:?}");
        }
        assert!(reg.contains(MultilevelEngine::ID));
        // Exactly the leaves + the wrapper.
        assert_eq!(reg.descriptors().len(), LEAF_ENGINE_IDS.len() + 1);
    }

    /// `leaf_ctor_for` (the fn-pointer table behind the registry) must agree with
    /// `construct_leaf` for every leaf id.
    #[test]
    fn leaf_ctor_for_matches_construct_leaf() {
        for &id in LEAF_ENGINE_IDS {
            let via_ctor = leaf_ctor_for(id)();
            let via_construct = construct_leaf(id).unwrap();
            assert_eq!(via_ctor.descriptor().id, via_construct.descriptor().id);
        }
    }
}
