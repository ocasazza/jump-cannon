//! Brute-force ForceAtlas2 layout engine (`"fa2-brute"`).
//!
//! The first engine in the registry (ADR-001), originally lifted from
//! `wgpu_sim::WgpuSim`: O(n²) repulsion + linear-scan attraction + gravity on
//! wgpu (`shaders/force_atlas2.wgsl`), host-readback each tick.
//!
//! Integration is the paper's ADAPTIVE GLOBAL SPEED (Jacomy et al. 2014,
//! PLOS ONE; ported from Gephi's reference ForceAtlas2.java — see
//! [`super::fa2_speed`]): each step runs a `fa2_force` dispatch that also emits
//! per-node swing/traction stats, the host reduces them (fixed order ⇒
//! deterministic) into the global speed `s(G)`, then a `fa2_apply` dispatch
//! displaces every node by `F · s(G)/(1+√(s(G)·swing))`, capped at
//! `max_displacement` (the paper's k_smax). The original Euler
//! `vel = (vel+force)·0.5` integrator diverged exponentially at vault scale
//! (~10k nodes: positions ×37/step, NaN by step 23 from a √n·5-radius ball).
//!
//! The O(n²) repulsion caps useful graph size at ~10–50k nodes; the Barnes-Hut
//! octree that fixes it (`graph-layouts/.../shaders/octree.wgsl`) lands as a
//! separate engine in a later phase (`docs/compute-architecture.md` §2, Phase 2).
//!
//! Unlike the old `WgpuSim` — which requested its own adapter+device — this
//! engine takes the shared device/queue from [`EngineCtx::gpu`] at `init`, so a
//! worker brings up wgpu exactly once for all GPU engines.

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "fa2-brute";

const WORKGROUP_SIZE: u32 = 64;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct Fa2ParamsRaw {
    n_nodes: u32,
    n_edges: u32,
    gravity: f32,
    scaling_ratio: f32,
    edge_weight_influence: f32,
    jitter_tolerance: f32,
    time_step: f32,
    strong_gravity: u32,
    lin_log_mode: u32,
    prevent_overlap: u32,
    /// Global adaptive speed s(G); host-recomputed between the force and apply
    /// passes each step. Replaces the old _pad0.
    speed: f32,
    /// Per-step displacement cap (paper k_smax; 0 disables). Replaces _pad1.
    max_displacement: f32,
}

/// FA2 tunables. Serde-roundtrippable so they ride on the wire as
/// `google.protobuf.Struct` (ADR-002). Defaults mirror
/// `ForceAtlas2Settings::default` from graph-layouts (and the old
/// `WgpuSimSettings::default`), so behavior is identical to the pre-refactor sim.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Fa2Settings {
    pub gravity: f32,
    pub strong_gravity: bool,
    pub scaling_ratio: f32,
    pub edge_weight_influence: f32,
    pub jitter_tolerance: f32,
    pub lin_log_mode: bool,
    pub prevent_overlap: bool,
    pub time_step: f32,
    /// Per-step displacement cap — the paper's `k_smax` (10 in Gephi/the
    /// paper): node speed is limited to `k_smax/|F|` so one step never moves a
    /// node more than this many units. `0` disables the cap.
    pub max_displacement: f32,
}

impl Default for Fa2Settings {
    fn default() -> Self {
        Self {
            gravity: 1.0,
            strong_gravity: false,
            scaling_ratio: 2.0,
            edge_weight_influence: 1.0,
            jitter_tolerance: 1.0,
            lin_log_mode: false,
            prevent_overlap: false,
            time_step: 1.0,
            max_displacement: 10.0,
        }
    }
}

/// GPU state, built once at `init`. Mirrors the old `WgpuSim`'s buffer set.
struct Gpu {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    /// `fa2_force`: forces + swing/traction stats.
    force_pipeline: wgpu::ComputePipeline,
    /// `fa2_apply`: adaptive-speed displacement (after the host reduction).
    apply_pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    positions_buf: wgpu::Buffer,
    _old_force_buf: wgpu::Buffer,
    _force_buf: wgpu::Buffer,
    _edges_buf: wgpu::Buffer,
    _edge_weights_buf: wgpu::Buffer,
    _degrees_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    stats_buf: wgpu::Buffer,
    stats_readback_buf: wgpu::Buffer,
    readback_buf: wgpu::Buffer,
    n_nodes: u32,
    cached_n_edges: u32,
    /// `n_nodes * 16` — bytes of the positions storage buffer (vec4 per node).
    positions_byte_len: u64,
    /// `n_nodes * 8` — bytes of the per-node (swing, traction) stats buffer.
    stats_byte_len: u64,
    /// Adaptive global-speed controller (Gephi's speed/speedEfficiency).
    adaptive: super::fa2_speed::AdaptiveSpeed,
}

/// Brute-force FA2 engine. Uninitialized until [`LayoutEngine::init`].
pub struct Fa2BruteEngine {
    descriptor: LayoutDescriptor,
    settings: Fa2Settings,
    gpu: Option<Gpu>,
}

impl Fa2BruteEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: Fa2Settings::default(),
            gpu: None,
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "ForceAtlas2 (brute force)",
            description: "O(n²) repulsion + linear-scan attraction ForceAtlas2 on wgpu. \
                          Caps out around 10-50k nodes; Barnes-Hut is the scalable variant.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }
}

impl Default for Fa2BruteEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for Fa2BruteEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: Fa2Settings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode fa2-brute settings: {e}"))?;
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        ctx: &mut EngineCtx,
        graph: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let gpu_ctx = ctx
            .gpu
            .as_ref()
            .ok_or_else(|| "fa2-brute requires a wgpu device but none is available".to_string())?;
        let device = gpu_ctx.device.clone();
        let queue = gpu_ctx.queue.clone();

        let graph = graph.graph;
        let n_nodes = graph.n_nodes;
        let n = n_nodes as usize;
        if positions.len() != 3 * n {
            return Err(format!(
                "initial positions length {} != 3 * n_nodes {}",
                positions.len(),
                3 * n
            ));
        }

        // ---- Build CPU-side buffers --------------------------------------
        // Positions as vec4<f32> (xyz + 0 pad) — the shader reads `positions[i].xyz`.
        let mut positions_vec4: Vec<f32> = Vec::with_capacity(n * 4);
        for i in 0..n {
            positions_vec4.push(positions[3 * i]);
            positions_vec4.push(positions[3 * i + 1]);
            positions_vec4.push(positions[3 * i + 2]);
            positions_vec4.push(0.0);
        }
        if positions_vec4.is_empty() {
            // wgpu refuses zero-sized storage buffers; pad to 1 vec4.
            positions_vec4.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
        }

        // Previous-step force F_{t-1}, zero at start (Gephi: old_dx/old_dy = 0
        // on the first pass, so step-1 swing = |F| and traction = |F|/2).
        let old_force = vec![0.0f32; n.max(1) * 4];

        // Synthesize edges from CSR. Emit each undirected pair once (src < tgt)
        // because the shader's attraction loop matches either endpoint.
        let mut edges_pairs: Vec<[u32; 2]> = Vec::new();
        let mut weights: Vec<f32> = Vec::new();
        let mut degrees: Vec<u32> = vec![0u32; n.max(1)];
        for v in 0..n {
            let start = graph.offsets[v] as usize;
            let end = graph.offsets[v + 1] as usize;
            for e in start..end {
                let u = graph.neighbors[e];
                let v_u = v as u32;
                if v_u == u {
                    continue;
                }
                degrees[v] += 1;
                if v_u < u {
                    edges_pairs.push([v_u, u]);
                    weights.push(1.0);
                }
            }
        }
        let n_edges = edges_pairs.len() as u32;
        if edges_pairs.is_empty() {
            edges_pairs.push([0, 0]);
            weights.push(1.0);
        }

        // ---- GPU buffers -------------------------------------------------
        let positions_byte_len = (positions_vec4.len() as u64) * 4;

        let positions_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_positions"),
            contents: bytemuck::cast_slice(&positions_vec4),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let old_force_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_old_force"),
            contents: bytemuck::cast_slice(&old_force),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let force_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_force"),
            contents: bytemuck::cast_slice(&old_force),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let edges_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_edges"),
            contents: bytemuck::cast_slice(&edges_pairs),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let edge_weights_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_edge_weights"),
            contents: bytemuck::cast_slice(&weights),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let degrees_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_degrees"),
            contents: bytemuck::cast_slice(&degrees),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let params_init = build_params_raw(n_nodes, n_edges, &self.settings, 1.0);
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_params"),
            contents: bytemuck::bytes_of(&params_init),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fa2_positions_readback"),
            size: positions_byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Per-node (mass·swing, mass·traction) pairs for the host's global
        // adaptive-speed reduction; read back BETWEEN the force and apply
        // passes each step.
        let stats_byte_len = (n.max(1) as u64) * 8;
        let stats_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fa2_stats"),
            size: stats_byte_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let stats_readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fa2_stats_readback"),
            size: stats_byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- Pipeline ----------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fa2_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/force_atlas2.wgsl"
            ))),
        });

        let storage_rw = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let storage_ro = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fa2_bgl"),
            entries: &[
                storage_rw(0), // positions (force pass reads; apply writes own node)
                storage_rw(1), // old_force (F_{t-1})
                storage_ro(2), // edges
                storage_ro(3), // edge_weights
                uniform(4),    // params
                storage_ro(5), // degrees
                storage_rw(6), // force (F_t)
                storage_rw(7), // stats (mass·swing, mass·traction)
            ],
        });

        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fa2_pipeline_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let force_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fa2_force_pipeline"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("fa2_force"),
            compilation_options: Default::default(),
            cache: None,
        });
        let apply_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fa2_apply_pipeline"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("fa2_apply"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fa2_bind_group"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: positions_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: old_force_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: edges_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: edge_weights_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: degrees_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: force_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: stats_buf.as_entire_binding(),
                },
            ],
        });

        self.gpu = Some(Gpu {
            device,
            queue,
            force_pipeline,
            apply_pipeline,
            bind_group,
            positions_buf,
            _old_force_buf: old_force_buf,
            _force_buf: force_buf,
            _edges_buf: edges_buf,
            _edge_weights_buf: edge_weights_buf,
            _degrees_buf: degrees_buf,
            params_buf,
            stats_buf,
            stats_readback_buf,
            readback_buf,
            n_nodes,
            cached_n_edges: n_edges,
            positions_byte_len,
            stats_byte_len,
            adaptive: super::fa2_speed::AdaptiveSpeed::default(),
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let gpu = self
            .gpu
            .as_mut()
            .expect("fa2-brute step called before successful init");
        let workgroups = ((gpu.n_nodes + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE).max(1);

        // ---- Pass 1: forces + swing/traction stats -------------------------
        // Params are refreshed with the CURRENT speed (the force pass ignores
        // it, but keeps the uniform coherent); n_edges is fixed at init.
        let params = build_params_raw(gpu.n_nodes, gpu.cached_n_edges, &settings, gpu.adaptive.speed);
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fa2_force_encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fa2_force_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.force_pipeline);
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &gpu.stats_buf,
            0,
            &gpu.stats_readback_buf,
            0,
            gpu.stats_byte_len,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // ---- Host reduction: global adaptive speed (Gephi state machine) ---
        {
            let slice = gpu.stats_readback_buf.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |res| {
                let _ = tx.send(res);
            });
            gpu.device.poll(wgpu::Maintain::Wait);
            rx.recv()
                .expect("map_async channel closed")
                .expect("stats buffer map failed");
            let data = slice.get_mapped_range();
            let stats: &[f32] = bytemuck::cast_slice(&data);
            gpu.adaptive
                .update(gpu.n_nodes, settings.jitter_tolerance, stats);
            drop(data);
            gpu.stats_readback_buf.unmap();
        }

        // ---- Pass 2: adaptive-speed displacement ---------------------------
        let params = build_params_raw(gpu.n_nodes, gpu.cached_n_edges, &settings, gpu.adaptive.speed);
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fa2_apply_encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fa2_apply_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.apply_pipeline);
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &gpu.positions_buf,
            0,
            &gpu.readback_buf,
            0,
            gpu.positions_byte_len,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // Map + read back positions.
        let slice = gpu.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        // Drive the device until the map callback fires.
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("map_async channel closed")
            .expect("buffer map failed");

        let data = slice.get_mapped_range();
        let vec4_floats: &[f32] = bytemuck::cast_slice(&data);
        // Strip the pad lane: take xyz of each vec4.
        let n = gpu.n_nodes as usize;
        let mut out = Vec::with_capacity(3 * n);
        for i in 0..n {
            out.push(vec4_floats[4 * i]);
            out.push(vec4_floats[4 * i + 1]);
            out.push(vec4_floats[4 * i + 2]);
        }
        drop(data);
        gpu.readback_buf.unmap();
        StepOutput::positions_only(out)
    }
}

/// Build a params uniform payload. `n_edges` is fixed at `init` time and cached;
/// settings can mutate per-step via `set_params`; `speed` is the host-side
/// adaptive global speed for the apply pass.
fn build_params_raw(n_nodes: u32, n_edges: u32, s: &Fa2Settings, speed: f32) -> Fa2ParamsRaw {
    Fa2ParamsRaw {
        n_nodes,
        n_edges,
        gravity: s.gravity,
        scaling_ratio: s.scaling_ratio,
        edge_weight_influence: s.edge_weight_influence,
        jitter_tolerance: s.jitter_tolerance,
        time_step: s.time_step,
        strong_gravity: s.strong_gravity as u32,
        lin_log_mode: s.lin_log_mode as u32,
        prevent_overlap: s.prevent_overlap as u32,
        speed,
        max_displacement: s.max_displacement,
    }
}
