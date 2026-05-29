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

pub mod fa2_brute;
pub mod cpu_spring;
pub mod fa2_bh;
pub mod sgd_stress;
pub mod multilevel;

pub use cpu_spring::CpuSpringEngine;
pub use fa2_brute::Fa2BruteEngine;
pub use fa2_bh::Fa2BhEngine;
pub use sgd_stress::SgdStressEngine;
pub use multilevel::MultilevelEngine;

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

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok_or_else(|| anyhow!("no wgpu adapter available"))?;

    let adapter_info = adapter.get_info();

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("graph-compute-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
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
}

impl<'a> CsrShard<'a> {
    /// Wrap a whole graph as an unsharded (single-worker) shard.
    pub fn whole(graph: &'a CsrGraph) -> Self {
        Self { graph, shard: None }
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

    /// The built-in registry: the ported brute-force FA2 (`"fa2-brute"`,
    /// default) plus the CPU spring fallback (`"cpu-spring"`).
    pub fn builtin() -> Self {
        Self::new(
            &[
                (Fa2BruteEngine::ID, || {
                    Box::new(Fa2BruteEngine::new()) as Box<dyn LayoutEngine>
                }),
                (CpuSpringEngine::ID, || {
                    Box::new(CpuSpringEngine::new()) as Box<dyn LayoutEngine>
                }),
                (Fa2BhEngine::ID, || {
                    Box::new(Fa2BhEngine::new()) as Box<dyn LayoutEngine>
                }),
                (SgdStressEngine::ID, || {
                    Box::new(SgdStressEngine::new()) as Box<dyn LayoutEngine>
                }),
                (MultilevelEngine::ID, || {
                    Box::new(MultilevelEngine::new()) as Box<dyn LayoutEngine>
                }),
            ],
            Fa2BruteEngine::ID,
        )
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
