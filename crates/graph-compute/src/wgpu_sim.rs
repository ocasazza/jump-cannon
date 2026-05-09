//! Server-side wgpu ForceAtlas2 integrator.
//!
//! Brute-force O(n²) repulsion + linear-scan attraction shader, ported byte-for-byte
//! from `crates/graph-layouts/src/layout/algorithms/force_atlas2.rs` on
//! `worktree-agent-a52adc39`. Owns its own buffers; one dispatch per `step`.
//!
//! `step` blocks (via `pollster`) on a positions readback because the gRPC
//! sim loop drives this from `tokio::task::spawn_blocking` and needs the
//! host-side `Vec<f32>` to broadcast.

use std::borrow::Cow;

use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::sim::CsrGraph;

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
    _pad0: u32,
    _pad1: u32,
}

/// FA2 tunables baked into the params uniform. Defaults mirror
/// `ForceAtlas2Settings::default` from graph-layouts.
#[derive(Clone, Debug)]
pub struct WgpuSimSettings {
    pub gravity: f32,
    pub strong_gravity: bool,
    pub scaling_ratio: f32,
    pub edge_weight_influence: f32,
    pub jitter_tolerance: f32,
    pub lin_log_mode: bool,
    pub prevent_overlap: bool,
    pub time_step: f32,
}

impl Default for WgpuSimSettings {
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
        }
    }
}

pub struct WgpuSim {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,

    positions_buf: wgpu::Buffer,
    _velocities_buf: wgpu::Buffer,
    _edges_buf: wgpu::Buffer,
    _edge_weights_buf: wgpu::Buffer,
    _degrees_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    /// MAP_READ staging buffer for positions readback. Sized to `n_nodes * 16` (vec4<f32>).
    readback_buf: wgpu::Buffer,

    n_nodes: u32,
    cached_n_edges: u32,
    settings: WgpuSimSettings,
    /// `n_nodes * 16` — bytes of the positions storage buffer (vec4 per node).
    positions_byte_len: u64,
    /// Reusable adapter info for diagnostics.
    pub adapter_info: wgpu::AdapterInfo,
}

impl WgpuSim {
    /// Try to bring up a wgpu device and upload the graph + positions. Returns
    /// `Err` if no adapter is available (CI fall-through to `cpu_step`).
    pub fn new(graph: &CsrGraph, initial_positions: &[f32]) -> Result<Self> {
        Self::new_with_settings(graph, initial_positions, WgpuSimSettings::default())
    }

    pub fn new_with_settings(
        graph: &CsrGraph,
        initial_positions: &[f32],
        settings: WgpuSimSettings,
    ) -> Result<Self> {
        let n_nodes = graph.n_nodes;
        let n = n_nodes as usize;
        if initial_positions.len() != 3 * n {
            return Err(anyhow!(
                "initial_positions length {} != 3 * n_nodes {}",
                initial_positions.len(),
                3 * n
            ));
        }

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

        // ---- Build CPU-side buffers --------------------------------------
        // Positions as vec4<f32> (xyz + 0 pad) — the shader reads `positions[i].xyz`.
        let mut positions_vec4: Vec<f32> = Vec::with_capacity(n * 4);
        for i in 0..n {
            positions_vec4.push(initial_positions[3 * i]);
            positions_vec4.push(initial_positions[3 * i + 1]);
            positions_vec4.push(initial_positions[3 * i + 2]);
            positions_vec4.push(0.0);
        }
        if positions_vec4.is_empty() {
            // wgpu refuses zero-sized storage buffers; pad to 1 vec4.
            positions_vec4.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
        }

        let velocities = vec![0.0f32; n.max(1) * 4];

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
        let velocities_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2_velocities"),
            contents: bytemuck::cast_slice(&velocities),
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

        let params_init = build_params_raw(n_nodes, n_edges, &settings);
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

        // ---- Pipeline ----------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fa2_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "shaders/force_atlas2.wgsl"
            ))),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fa2_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fa2_pipeline_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fa2_pipeline"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("fa2_step"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fa2_bind_group"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: positions_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: velocities_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: edges_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: edge_weights_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: degrees_buf.as_entire_binding() },
            ],
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group,
            positions_buf,
            _velocities_buf: velocities_buf,
            _edges_buf: edges_buf,
            _edge_weights_buf: edge_weights_buf,
            _degrees_buf: degrees_buf,
            params_buf,
            readback_buf,
            n_nodes,
            cached_n_edges: n_edges,
            settings,
            positions_byte_len,
            adapter_info,
        })
    }

    /// Encode one fa2_step dispatch, copy positions into the MAP_READ staging
    /// buffer, submit, and block until the host can read them back.
    pub fn step(&mut self) -> Vec<f32> {
        // Refresh params uniform — settings can change between calls; n_edges is
        // fixed for the lifetime of the WgpuSim and lives in `cached_n_edges`.
        let params = build_params_raw(self.n_nodes, self.cached_n_edges, &self.settings);
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fa2_encoder"),
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fa2_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            let workgroups = (self.n_nodes + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            pass.dispatch_workgroups(workgroups.max(1), 1, 1);
        }

        encoder.copy_buffer_to_buffer(
            &self.positions_buf,
            0,
            &self.readback_buf,
            0,
            self.positions_byte_len,
        );

        self.queue.submit(std::iter::once(encoder.finish()));

        // Map + read back.
        let slice = self.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        // Drive the device until the map callback fires.
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("map_async channel closed")
            .expect("buffer map failed");

        let data = slice.get_mapped_range();
        let vec4_floats: &[f32] = bytemuck::cast_slice(&data);
        // Strip the pad lane: take xyz of each vec4.
        let n = self.n_nodes as usize;
        let mut out = Vec::with_capacity(3 * n);
        for i in 0..n {
            out.push(vec4_floats[4 * i]);
            out.push(vec4_floats[4 * i + 1]);
            out.push(vec4_floats[4 * i + 2]);
        }
        drop(data);
        self.readback_buf.unmap();
        out
    }
}

/// Build a params uniform payload. `n_edges` is fixed at `WgpuSim::new` time
/// and cached in `WgpuSim::cached_n_edges`; settings can mutate per-step.
fn build_params_raw(n_nodes: u32, n_edges: u32, s: &WgpuSimSettings) -> Fa2ParamsRaw {
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
        _pad0: 0,
        _pad1: 0,
    }
}
