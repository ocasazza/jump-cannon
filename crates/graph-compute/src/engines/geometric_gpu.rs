//! Geometric GPU engine (`"geometric-gpu"`).
//!
//! Port of [`GeometricEngine`](super::geometric::GeometricEngine) to GPU (WGSL + wgpu).
//! Uses a Barnes-Hut octree for the exclusion/affinity far-field, mirroring
//! the [`Fa2BhEngine`](super::fa2_bh::Fa2BhEngine) architecture.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use wgpu::util::DeviceExt;

use super::geometric::{GeometricEngine, GeometricSettings};
use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "geometric-gpu";

const WORKGROUP_SIZE: u32 = 64;

/// Octree sentinel: empty child slot / end-of-walk / "this node is internal".
const OCT_END: u32 = u32::MAX;
const OCT_BODY_INTERNAL: u32 = u32::MAX;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct GeometricParamsRaw {
    n_nodes: u32,
    n_edges: u32,
    n_octree: u32,
    class_affinity_dim: u32,
    edge_stiffness: f32,
    angle_stiffness: f32,
    exclusion_strength: f32,
    affinity_strength: f32,
    gravity: f32,
    time_step: f32,
    damping: f32,
    max_step: f32,
    theta: f32,
    default_radius: f32,
    cutoff_scale: f32,
}

#[allow(dead_code)]
struct Gpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,

    positions_buf: wgpu::Buffer,
    velocities_buf: wgpu::Buffer,
    edges_buf: wgpu::Buffer,
    target_lens_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    node_class_buf: wgpu::Buffer,
    node_coord_buf: wgpu::Buffer,
    node_mass_buf: wgpu::Buffer,
    oct_nodes_buf: wgpu::Buffer,
    coord_angles_buf: wgpu::Buffer,
    class_radius_buf: wgpu::Buffer,
    class_affinity_buf: wgpu::Buffer,
    csr_buf: wgpu::Buffer,

    readback_buf: wgpu::Buffer,

    n_nodes: u32,
    n_edges: u32,
    oct_capacity: u32,
    positions_byte_len: u64,

    cpu_positions: Vec<f32>,
    cpu_mass: Vec<f32>,
    /// Resolved per-node coordination ids uploaded to `node_coord_buf` — kept for
    /// test introspection (the WGSL angle pass indexes exactly this).
    cpu_coordination: Vec<u32>,
    oct_build: OctreeBuild,
}

pub struct GeometricGpuEngine {
    descriptor: LayoutDescriptor,
    settings: GeometricSettings,
    gpu: Option<Gpu>,
}

impl GeometricGpuEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: LayoutDescriptor {
                id: LAYOUT_ID,
                kind: LayoutKind::Physics,
                display_name: "Geometric (GPU)",
                description: "GPU-accelerated geometric constraint engine. Uses a Barnes-Hut \
                              octree for exclusion/affinity and WGSL kernels for all forces.",
                requirements: LayoutRequirements {
                    needs_edges: true,
                    needs_cpu_positions: true,
                    needs_gpu_positions_buffer: false,
                },
            },
            settings: GeometricSettings::default(),
            gpu: None,
        }
    }
}

impl Default for GeometricGpuEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl GeometricGpuEngine {
    /// The resolved per-node coordination ids this engine uploaded to the GPU at
    /// `init` (the vector the WGSL angle pass indexes). For tests / introspection;
    /// panics if called before a successful `init`.
    pub fn debug_node_coordination(&self) -> Vec<u32> {
        self.gpu
            .as_ref()
            .expect("debug_node_coordination before init")
            .cpu_coordination
            .clone()
    }
}

impl LayoutEngine for GeometricGpuEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: GeometricSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode geometric settings: {e}"))?;
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        ctx: &mut EngineCtx,
        graph_shard: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let gpu_ctx = ctx
            .gpu
            .as_ref()
            .ok_or_else(|| "geometric-gpu requires a wgpu device".to_string())?;
        let device = gpu_ctx.device.clone();
        let queue = gpu_ctx.queue.clone();

        let graph = graph_shard.graph;
        let n_nodes = graph.n_nodes;
        let n = n_nodes as usize;

        let mut positions_vec4 = vec![0.0f32; n * 4];
        for i in 0..n {
            positions_vec4[4 * i] = positions[3 * i];
            positions_vec4[4 * i + 1] = positions[3 * i + 1];
            positions_vec4[4 * i + 2] = positions[3 * i + 2];
        }

        // Resolve class / coordination / mass / edges through the SAME path the
        // CPU engine uses, so structural sources (degree / community / PageRank /
        // injected) all honour the chosen lens identically — not silently default
        // to bucket 0 / unit mass. Errors (e.g. an Injected source with no
        // attribute) propagate exactly as on CPU.
        let resolved = GeometricEngine::resolve(&self.settings, graph, graph_shard.attributes)?;
        let node_class: Vec<u32> = resolved.class;
        let node_coord: Vec<u32> = resolved.coordination;
        let node_mass: Vec<f32> = resolved.mass;

        // Flatten the resolved unique edges (a < b, with target lengths) into the
        // GPU's parallel pair / length buffers.
        let mut edges_pairs: Vec<[u32; 2]> = Vec::with_capacity(resolved.edges.len());
        let mut target_lens: Vec<f32> = Vec::with_capacity(resolved.edges.len());
        for e in &resolved.edges {
            edges_pairs.push([e.a, e.b]);
            target_lens.push(e.target_len);
        }
        let n_edges = edges_pairs.len() as u32;

        // Packed CSR adjacency for the angle neighbour-gather, in ONE buffer to
        // respect the one-free-storage-slot limit. Layout:
        //   csr[0 ..= n]        = offsets, each PRE-SHIFTED by (n+1) so it points
        //                         directly into the neighbours region below.
        //   csr[n+1 + k]        = the k-th neighbour (global CSR neighbour list).
        // Neighbours of node v are therefore csr[csr[v] .. csr[v+1]]. This engine
        // owns the whole graph, so graph.offsets / graph.neighbors are global CSR.
        let header = (n + 1) as u32;
        let mut csr: Vec<u32> = Vec::with_capacity((n + 1) + graph.neighbors.len());
        for v in 0..=n {
            csr.push(graph.offsets[v] + header);
        }
        csr.extend_from_slice(&graph.neighbors);

        // ---- GPU Buffers ----
        let positions_byte_len = (positions_vec4.len() as u64) * 4;
        let positions_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_positions"),
            contents: bytemuck::cast_slice(&positions_vec4),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let velocities_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_velocities"),
            contents: bytemuck::cast_slice(&vec![0.0f32; n * 4]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let edges_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_edges"),
            contents: bytemuck::cast_slice(&edges_pairs),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let target_lens_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_target_lens"),
            contents: bytemuck::cast_slice(&target_lens),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let node_class_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_node_class"),
            contents: bytemuck::cast_slice(&node_class),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let node_coord_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_node_coord"),
            contents: bytemuck::cast_slice(&node_coord),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let node_mass_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_node_mass"),
            contents: bytemuck::cast_slice(&node_mass),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let oct_capacity = (2 * n_nodes + 8).max(1);
        let oct_nodes_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom_oct_nodes"),
            size: oct_capacity as u64 * 48,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let coord_angles_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_coord_angles"),
            contents: if self.settings.coordination_angles.is_empty() {
                bytemuck::cast_slice(&[0.0f32])
            } else {
                bytemuck::cast_slice(&self.settings.coordination_angles)
            },
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let class_radius_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_class_radius"),
            contents: if self.settings.class_radius.is_empty() {
                bytemuck::cast_slice(&[0.0f32])
            } else {
                bytemuck::cast_slice(&self.settings.class_radius)
            },
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let class_affinity_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_class_affinity"),
            contents: if self.settings.class_affinity.is_empty() {
                bytemuck::cast_slice(&[0.0f32])
            } else {
                bytemuck::cast_slice(&self.settings.class_affinity)
            },
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let csr_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_csr"),
            contents: bytemuck::cast_slice(&csr),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let params_init = build_params_raw(n_nodes, n_edges, &self.settings, 0);
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("geom_params"),
            contents: bytemuck::bytes_of(&params_init),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geom_readback"),
            size: positions_byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- Pipeline ----
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("geom_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/geometric_barnes_hut.wgsl"
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
            label: Some("geom_bgl"),
            entries: &[
                storage_rw(0),
                storage_rw(1),
                storage_ro(2),
                storage_ro(3),
                uniform(4),
                storage_ro(5),
                storage_ro(6),
                storage_ro(7),
                storage_ro(8),
                storage_ro(9),
                storage_ro(10),
                storage_ro(11),
                storage_ro(12),
            ],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("geom_pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("geom_layout"),
                    bind_group_layouts: &[&bgl],
                    push_constant_ranges: &[],
                }),
            ),
            module: &shader,
            entry_point: Some("geometric_step"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geom_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: positions_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: velocities_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: edges_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: target_lens_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: node_class_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: node_coord_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: node_mass_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: oct_nodes_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: coord_angles_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: class_radius_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: class_affinity_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: csr_buf.as_entire_binding(),
                },
            ],
        });

        self.gpu = Some(Gpu {
            device,
            queue,
            pipeline,
            bind_group,
            positions_buf,
            velocities_buf,
            edges_buf,
            target_lens_buf,
            params_buf,
            node_class_buf,
            node_coord_buf,
            node_mass_buf,
            oct_nodes_buf,
            coord_angles_buf,
            class_radius_buf,
            class_affinity_buf,
            csr_buf,
            readback_buf,
            n_nodes,
            n_edges,
            oct_capacity,
            positions_byte_len,
            cpu_positions: positions_vec4,
            cpu_mass: node_mass,
            cpu_coordination: node_coord,
            oct_build: OctreeBuild::default(),
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let gpu = self.gpu.as_mut().expect("step before init");

        let used = gpu.oct_build.rebuild(
            &gpu.cpu_positions,
            &gpu.cpu_mass,
            gpu.n_nodes,
            gpu.oct_capacity,
        );
        if used > 0 {
            gpu.queue.write_buffer(
                &gpu.oct_nodes_buf,
                0,
                bytemuck::cast_slice(&gpu.oct_build.nodes),
            );
        }

        let params = build_params_raw(gpu.n_nodes, gpu.n_edges, &settings, used);
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &gpu.bind_group, &[]);
            let workgroups = (gpu.n_nodes + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            pass.dispatch_workgroups(workgroups.max(1), 1, 1);
        }
        encoder.copy_buffer_to_buffer(
            &gpu.positions_buf,
            0,
            &gpu.readback_buf,
            0,
            gpu.positions_byte_len,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let slice = gpu.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let vec4_floats: &[f32] = bytemuck::cast_slice(&data);
        gpu.cpu_positions.copy_from_slice(vec4_floats);

        let mut out = Vec::with_capacity(3 * gpu.n_nodes as usize);
        for i in 0..gpu.n_nodes as usize {
            out.push(vec4_floats[4 * i]);
            out.push(vec4_floats[4 * i + 1]);
            out.push(vec4_floats[4 * i + 2]);
        }
        drop(data);
        gpu.readback_buf.unmap();
        StepOutput::positions_only(out)
    }
}

fn build_params_raw(
    n_nodes: u32,
    n_edges: u32,
    s: &GeometricSettings,
    n_octree: u32,
) -> GeometricParamsRaw {
    GeometricParamsRaw {
        n_nodes,
        n_edges,
        n_octree,
        class_affinity_dim: s.class_affinity_dim,
        edge_stiffness: s.edge_stiffness,
        angle_stiffness: s.angle_stiffness,
        exclusion_strength: s.exclusion_strength,
        affinity_strength: s.affinity_strength,
        gravity: s.gravity,
        time_step: s.time_step,
        damping: s.damping,
        max_step: s.max_step,
        theta: 0.7,
        default_radius: s.default_radius,
        cutoff_scale: s.cutoff_scale,
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct OctNodeRaw {
    pos_size: [f32; 4],
    com_mass: [f32; 4],
    meta: [u32; 4],
}

#[derive(Default)]
struct OctreeBuild {
    nodes: Vec<OctNodeRaw>,
    children: Vec<[u32; 8]>,
}

impl OctreeBuild {
    fn rebuild(&mut self, positions: &[f32], mass: &[f32], n_bodies: u32, capacity: u32) -> u32 {
        if n_bodies == 0 {
            return 0;
        }
        self.nodes.clear();
        self.children.clear();

        let mut min_pos = [f32::INFINITY; 3];
        let mut max_pos = [f32::NEG_INFINITY; 3];
        for i in 0..n_bodies as usize {
            for d in 0..3 {
                let p = positions[4 * i + d];
                if p < min_pos[d] {
                    min_pos[d] = p;
                }
                if p > max_pos[d] {
                    max_pos[d] = p;
                }
            }
        }

        let mut center = [0.0f32; 3];
        let mut max_dim = 1e-3f32;
        for d in 0..3 {
            center[d] = (min_pos[d] + max_pos[d]) * 0.5;
            max_dim = max_dim.max(max_pos[d] - min_pos[d]);
        }
        let half_extent = max_dim * 0.5;

        self.nodes.push(OctNodeRaw {
            pos_size: [center[0], center[1], center[2], half_extent],
            com_mass: [0.0, 0.0, 0.0, 0.0],
            meta: [OCT_BODY_INTERNAL, OCT_END, OCT_END, 0],
        });
        self.children.push([OCT_END; 8]);

        for i in 0..n_bodies {
            self.insert(0, i, positions, mass, capacity);
        }

        self.compute_com_and_rope(0, OCT_END);
        self.nodes.len() as u32
    }

    fn insert(
        &mut self,
        node_idx: usize,
        body_idx: u32,
        positions: &[f32],
        mass: &[f32],
        capacity: u32,
    ) {
        let (pos_i, mass_i) = (
            &positions[4 * body_idx as usize..4 * body_idx as usize + 3],
            mass[body_idx as usize],
        );

        let mut curr = node_idx;
        loop {
            let node = &self.nodes[curr];
            let octant = self.get_octant(node.pos_size, pos_i);
            let child = self.children[curr][octant];

            if child == OCT_END {
                if self.nodes.len() as u32 >= capacity {
                    return;
                }
                let new_idx = self.nodes.len() as u32;
                let c = self.get_child_center(node.pos_size, octant);
                let h = node.pos_size[3] * 0.5;
                self.nodes.push(OctNodeRaw {
                    pos_size: [c[0], c[1], c[2], h],
                    com_mass: [pos_i[0], pos_i[1], pos_i[2], mass_i],
                    meta: [body_idx, OCT_END, OCT_END, 0],
                });
                self.children.push([OCT_END; 8]);
                self.children[curr][octant] = new_idx;
                self.nodes[curr].meta[3] += 1;
                return;
            }

            let child_node = &self.nodes[child as usize];
            if child_node.meta[0] != OCT_BODY_INTERNAL {
                if self.nodes.len() as u32 >= capacity {
                    return;
                }
                let old_body = child_node.meta[0];

                self.nodes[child as usize].meta[0] = OCT_BODY_INTERNAL;
                self.nodes[child as usize].com_mass = [0.0, 0.0, 0.0, 0.0];

                self.insert(child as usize, old_body, positions, mass, capacity);

                // Recalculate octant for current body since child became internal
                let node = &self.nodes[curr];
                let octant = self.get_octant(node.pos_size, pos_i);
                curr = self.children[curr][octant] as usize;
            } else {
                curr = child as usize;
            }
        }
    }

    fn get_octant(&self, pos_size: [f32; 4], pos: &[f32]) -> usize {
        let mut o = 0;
        if pos[0] >= pos_size[0] {
            o |= 1;
        }
        if pos[1] >= pos_size[1] {
            o |= 2;
        }
        if pos[2] >= pos_size[2] {
            o |= 4;
        }
        o
    }

    fn get_child_center(&self, pos_size: [f32; 4], octant: usize) -> [f32; 3] {
        let h = pos_size[3] * 0.5;
        [
            pos_size[0] + if (octant & 1) != 0 { h } else { -h },
            pos_size[1] + if (octant & 2) != 0 { h } else { -h },
            pos_size[2] + if (octant & 4) != 0 { h } else { -h },
        ]
    }

    fn compute_com_and_rope(&mut self, idx: usize, skip: u32) -> u32 {
        let node = &mut self.nodes[idx];
        if node.meta[0] != OCT_BODY_INTERNAL {
            node.meta[1] = OCT_END;
            node.meta[2] = skip;
            return idx as u32;
        }

        let mut total_mass = 0.0f32;
        let mut com = [0.0f32; 3];
        let mut first_child = OCT_END;

        let children_ids = self.children[idx];
        for i in 0..8 {
            let child_idx = children_ids[i];
            if child_idx == OCT_END {
                continue;
            }

            if first_child == OCT_END {
                first_child = child_idx;
            }

            let mut next_skip = skip;
            for j in (i + 1)..8 {
                if children_ids[j] != OCT_END {
                    next_skip = children_ids[j];
                    break;
                }
            }

            self.compute_com_and_rope(child_idx as usize, next_skip);
            let child = &self.nodes[child_idx as usize];
            let m = child.com_mass[3];
            total_mass += m;
            com[0] += child.com_mass[0] * m;
            com[1] += child.com_mass[1] * m;
            com[2] += child.com_mass[2] * m;
        }

        if total_mass > 0.0 {
            com[0] /= total_mass;
            com[1] /= total_mass;
            com[2] /= total_mass;
        }

        let node = &mut self.nodes[idx];
        node.com_mass = [com[0], com[1], com[2], total_mass];
        node.meta[1] = first_child;
        node.meta[2] = skip;
        idx as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::CsrGraph;

    #[test]
    fn smoke_geometric_gpu_init_and_step() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping geometric-gpu smoke test (no GPU)");
            return;
        }

        let mut engine = GeometricGpuEngine::new();
        let graph = CsrGraph::path(10);
        let shard = CsrShard::whole(&graph);
        let mut positions = vec![0.0f32; 30];
        for i in 0..10 {
            positions[3 * i] = i as f32;
        }

        engine
            .init(&mut ctx, &shard, &positions)
            .expect("init failed");
        let out = engine.step(&mut ctx);
        assert_eq!(out.positions.len(), 30);
        // Smoke: positions should have moved (non-zero velocity/forces)
        let mut moved = false;
        for (i, &p) in out.positions.iter().enumerate() {
            if (p - positions[i]).abs() > 1e-6 {
                moved = true;
                break;
            }
        }
        assert!(moved, "nodes should have moved after one step");
    }
}
