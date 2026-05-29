//! Barnes-Hut ForceAtlas2 layout engine (`"fa2-bh"`).
//!
//! Same ForceAtlas2 as [`Fa2BruteEngine`](super::Fa2BruteEngine) — identical
//! attraction (linear edge scan), gravity, and Euler integration — but the
//! O(n²) all-pairs repulsion is replaced by an O(n log n) Barnes-Hut octree
//! walk (θ-criterion). The converged layout is the same; this is a *pure
//! speedup* (docs/layout-algorithms.md §1: "Visual: identical to FA2"), lifting
//! the useful graph size from ~10–50k nodes (brute force) toward ~1M.
//!
//! Tree construction follows Burtscher & Pingali's tree-based Barnes-Hut n-body
//! pipeline (build → center-of-mass aggregate → force), and the WGSL buffer +
//! stackless "rope" traversal layout matches GraphWaGu (harp-lab, IEEE
//! PacificVis 2022), the only direct wgpu+WGSL graph-layout precedent for our
//! stack. The per-node `OctNode` byte layout, the next/skip rope, and the
//! acceptance criterion `(s/d)² < θ²` mirror the host-built octree in
//! `graph-layouts/.../shaders/octree.wgsl` + `gpu_force.rs::OctreeBuild`; that
//! builder is *private* to graph-layouts, so the CPU build is ported here to
//! keep this engine self-contained.
//!
//! v1 note (carried from `gpu_force.rs`): the octree is built **on the host**
//! each `step` from a CPU mirror of the positions, then uploaded. That makes
//! per-step cost `O(n log n)` build + `O(n log n)` GPU walk — already a large
//! win over brute force, but the host build (and the per-step readback that
//! refreshes the CPU mirror) is the next thing to move onto the GPU via the
//! build kernels stubbed in `octree.wgsl` (bbox_reduce → morton_assign →
//! octree_build → com_aggregate). See `todo` in the engine return.
//!
//! References:
//!   - Burtscher & Pingali, "An Efficient CUDA Implementation of the
//!     Tree-based Barnes-Hut n-Body Algorithm" (2011).
//!   - "GraphWaGu", IEEE PacificVis 2022 (harp-lab) — WebGPU/WGSL force layout.

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "fa2-bh";

const WORKGROUP_SIZE: u32 = 64;

/// Octree sentinel: empty child slot / end-of-walk / "this node is internal".
const OCT_END: u32 = u32::MAX;
const OCT_BODY_INTERNAL: u32 = u32::MAX;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct Fa2BhParamsRaw {
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
    /// Barnes-Hut acceptance criterion (s/d < theta). Replaces brute-force pad0.
    theta: f32,
    /// Populated octree slots this step (0 => repulsion skipped). Replaces pad1.
    n_octree: u32,
}

/// FA2 + Barnes-Hut tunables. Serde-roundtrippable so they ride on the wire as
/// `google.protobuf.Struct` (ADR-002). The FA2 knobs default to the same values
/// as `Fa2Settings` so the converged layout matches the brute engine; `theta`
/// is the one Barnes-Hut-specific knob.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Fa2BhSettings {
    pub gravity: f32,
    pub strong_gravity: bool,
    pub scaling_ratio: f32,
    pub edge_weight_influence: f32,
    pub jitter_tolerance: f32,
    pub lin_log_mode: bool,
    pub prevent_overlap: bool,
    pub time_step: f32,
    /// Barnes-Hut acceptance criterion: treat a subtree as a single point mass
    /// when `cell_size / dist < theta`. 0.5..1.0 is the useful range; 0.7 is a
    /// common sweet spot (Burtscher & Pingali 2011 §4.5). Larger = faster, less
    /// accurate; 0.0 degenerates to brute force (every leaf visited).
    pub theta: f32,
}

impl Default for Fa2BhSettings {
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
            theta: 0.7,
        }
    }
}

/// GPU state, built once at `init`.
struct Gpu {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    positions_buf: wgpu::Buffer,
    _velocities_buf: wgpu::Buffer,
    _edges_buf: wgpu::Buffer,
    _edge_weights_buf: wgpu::Buffer,
    _degrees_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    oct_nodes_buf: wgpu::Buffer,
    /// Capacity of `oct_nodes_buf` in OctNode slots.
    oct_capacity: u32,
    readback_buf: wgpu::Buffer,
    n_nodes: u32,
    cached_n_edges: u32,
    /// `n_nodes * 16` — bytes of the positions storage buffer (vec4 per node).
    positions_byte_len: u64,
    /// Per-node Barnes-Hut mass = degree + 1, so a tree-cell COM aggregates the
    /// FA2 repulsion weight `(deg_j + 1)` summed over its bodies. Matches the
    /// `(deg_j + 1)` factor in `force_atlas2.wgsl`'s brute repulsion.
    cpu_mass: Vec<f32>,
    /// CPU mirror of positions (vec4 stride) refreshed from each step's
    /// readback; the octree is rebuilt from this before the next dispatch.
    cpu_positions: Vec<f32>,
    /// Reusable octree build scratch (avoids per-step reallocation).
    oct_build: OctreeBuild,
}

/// Barnes-Hut FA2 engine. Uninitialized until [`LayoutEngine::init`].
pub struct Fa2BhEngine {
    descriptor: LayoutDescriptor,
    settings: Fa2BhSettings,
    gpu: Option<Gpu>,
}

impl Fa2BhEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: Fa2BhSettings::default(),
            gpu: None,
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "ForceAtlas2 (Barnes-Hut)",
            description: "O(n log n) ForceAtlas2: same attraction/gravity/integration as the \
                          brute-force engine, but repulsion uses a Barnes-Hut octree (theta \
                          criterion). Identical layout, scales toward ~1M nodes.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }
}

impl Default for Fa2BhEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for Fa2BhEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: Fa2BhSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode fa2-bh settings: {e}"))?;
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
            .ok_or_else(|| "fa2-bh requires a wgpu device but none is available".to_string())?;
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

        // ---- Positions as vec4<f32> (xyz + 0 pad) ------------------------
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

        let velocities = vec![0.0f32; n.max(1) * 4];

        // Synthesize edges + per-node degree from CSR (same as fa2-brute).
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

        // Barnes-Hut per-body mass = degree + 1 (the FA2 repulsion weight). A
        // tree cell's COM mass is the sum of these over its bodies, so the
        // accepted-cell force equals the brute sum of `(deg_j + 1)` terms.
        let cpu_mass: Vec<f32> = (0..n.max(1))
            .map(|i| degrees.get(i).copied().unwrap_or(0) as f32 + 1.0)
            .collect();

        // ---- GPU buffers -------------------------------------------------
        let positions_byte_len = (positions_vec4.len() as u64) * 4;

        let positions_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_positions"),
            contents: bytemuck::cast_slice(&positions_vec4),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let velocities_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_velocities"),
            contents: bytemuck::cast_slice(&velocities),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let edges_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_edges"),
            contents: bytemuck::cast_slice(&edges_pairs),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let edge_weights_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_edge_weights"),
            contents: bytemuck::cast_slice(&weights),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let degrees_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_degrees"),
            contents: bytemuck::cast_slice(&degrees),
            usage: wgpu::BufferUsages::STORAGE,
        });

        // Octree storage: a Barnes-Hut octree has <= 2N internal+leaf slots in
        // the typical case; pad to 2N + 8 so a pathological build (one body per
        // leaf chain) still fits. Min 1 slot — wgpu refuses zero-sized buffers.
        let oct_capacity = (2 * n_nodes + 8).max(1);
        let oct_byte_len = oct_capacity as u64 * std::mem::size_of::<OctNodeRaw>() as u64;
        let oct_nodes_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fa2bh_oct_nodes"),
            size: oct_byte_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let params_init = build_params_raw(n_nodes, n_edges, &self.settings, 0);
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fa2bh_params"),
            contents: bytemuck::bytes_of(&params_init),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fa2bh_positions_readback"),
            size: positions_byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- Pipeline ----------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fa2bh_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/fa2_barnes_hut.wgsl"
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
            label: Some("fa2bh_bgl"),
            entries: &[
                storage_rw(0), // positions
                storage_rw(1), // velocities
                storage_ro(2), // edges
                storage_ro(3), // edge_weights
                uniform(4),    // params
                storage_ro(5), // degrees
                storage_ro(6), // oct_nodes
            ],
        });

        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fa2bh_pipeline_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fa2bh_pipeline"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("fa2_step"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fa2bh_bind_group"),
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
                    resource: oct_nodes_buf.as_entire_binding(),
                },
            ],
        });

        self.gpu = Some(Gpu {
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
            oct_nodes_buf,
            oct_capacity,
            readback_buf,
            n_nodes,
            cached_n_edges: n_edges,
            positions_byte_len,
            cpu_mass,
            cpu_positions: positions_vec4,
            oct_build: OctreeBuild::default(),
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let gpu = self
            .gpu
            .as_mut()
            .expect("fa2-bh step called before successful init");

        // Rebuild the Barnes-Hut octree on the host from the CPU position
        // mirror and upload it. (v1: host build; v2 moves to GPU — see module
        // doc.) `n_octree` rides on the params uniform below.
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

        // Refresh params (settings + this step's tree size). n_edges is fixed.
        let params = build_params_raw(gpu.n_nodes, gpu.cached_n_edges, &settings, used);
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fa2bh_encoder"),
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fa2bh_pass"),
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

        // Map + read back.
        let slice = gpu.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("map_async channel closed")
            .expect("buffer map failed");

        let data = slice.get_mapped_range();
        let vec4_floats: &[f32] = bytemuck::cast_slice(&data);
        // Refresh the CPU mirror (used to rebuild the tree next step) and emit
        // xyz for the PositionDelta.
        let n = gpu.n_nodes as usize;
        if gpu.cpu_positions.len() == vec4_floats.len() {
            gpu.cpu_positions.copy_from_slice(vec4_floats);
        }
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

/// Build a params uniform payload. `n_edges` is fixed at `init`; settings and
/// `n_octree` (the per-step tree size) can change between calls.
fn build_params_raw(
    n_nodes: u32,
    n_edges: u32,
    s: &Fa2BhSettings,
    n_octree: u32,
) -> Fa2BhParamsRaw {
    Fa2BhParamsRaw {
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
        theta: s.theta.max(0.0),
        n_octree,
    }
}

// ---------- Barnes-Hut octree (CPU build, v1) -------------------------------
//
// Ported from `graph-layouts/.../gpu_force.rs::OctreeBuild` (that type is
// private to graph-layouts). The on-wire `OctNodeRaw` layout, the next/skip
// rope, and the acceptance contract match `fa2_barnes_hut.wgsl`'s `OctNode`:
//
//   pos_size: (cx, cy, cz, half_extent)
//   com_mass: (com_x, com_y, com_z, mass)       <- degree-weighted mass
//   meta:     (body_idx | OCT_BODY_INTERNAL, next_idx, skip_idx, child_count)
//
// next_idx = first child in DFS order (rope descend); skip_idx = next-sibling-
// or-uncle (rope skip after a leaf or an accepted cell). Sentinel OCT_END
// terminates the WGSL walk.

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct OctNodeRaw {
    pos_size: [f32; 4],
    com_mass: [f32; 4],
    meta: [u32; 4],
}

/// Per-build scratch, reused across steps to avoid per-step allocation.
#[derive(Default)]
struct OctreeBuild {
    nodes: Vec<OctNodeRaw>,
    /// Children indices for each node (8 per node, OCT_END = empty slot).
    children: Vec<[u32; 8]>,
}

impl OctreeBuild {
    /// Build the octree in-place from `positions` (vec4 stride), per-body
    /// `mass`, and the body count. Returns the number of populated nodes.
    /// On overflow (capacity exceeded) returns the partial count — a partial
    /// tree is still a valid (slightly weaker) repulsion field rather than a
    /// crash. Returns 0 when there is nothing to build.
    fn rebuild(
        &mut self,
        positions: &[f32],
        mass: &[f32],
        n_bodies: u32,
        max_nodes: u32,
    ) -> u32 {
        self.nodes.clear();
        self.children.clear();
        if n_bodies == 0 || positions.len() < (n_bodies as usize) * 4 || max_nodes == 0 {
            return 0;
        }

        // 1. World bounding box.
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for i in 0..n_bodies as usize {
            for k in 0..3 {
                let v = positions[i * 4 + k];
                if !v.is_finite() {
                    continue;
                }
                if v < mn[k] {
                    mn[k] = v;
                }
                if v > mx[k] {
                    mx[k] = v;
                }
            }
        }
        if !mn[0].is_finite() {
            mn = [-1.0; 3];
            mx = [1.0; 3];
        }
        let center = [
            0.5 * (mn[0] + mx[0]),
            0.5 * (mn[1] + mx[1]),
            0.5 * (mn[2] + mx[2]),
        ];
        let mut half = ((mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2])) * 0.5;
        if !half.is_finite() || half <= 0.0 {
            half = 1.0;
        }
        // Pad slightly so points on the bbox edge land inside the root.
        half *= 1.05;

        // 2. Root.
        self.push_internal(center, half);

        // 3. Insert each body (iterative to avoid recursion-depth pitfalls).
        for body in 0..n_bodies {
            let bx = positions[body as usize * 4];
            let by = positions[body as usize * 4 + 1];
            let bz = positions[body as usize * 4 + 2];
            let bm = mass.get(body as usize).copied().unwrap_or(1.0).max(1e-3);
            if !(bx.is_finite() && by.is_finite() && bz.is_finite()) {
                continue;
            }
            if self
                .insert_body(0, [bx, by, bz], body, bm, max_nodes)
                .is_err()
            {
                // Overflow: stop; the partial tree is still valid.
                break;
            }
        }

        // 4. Aggregate COM/mass (post-order) then assign next/skip ropes.
        self.aggregate_com_postorder();
        self.assign_ropes();

        self.nodes.len() as u32
    }

    fn push_internal(&mut self, c: [f32; 3], h: f32) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(OctNodeRaw {
            pos_size: [c[0], c[1], c[2], h],
            com_mass: [0.0, 0.0, 0.0, 0.0],
            meta: [OCT_BODY_INTERNAL, OCT_END, OCT_END, 0],
        });
        self.children.push([OCT_END; 8]);
        idx
    }

    /// Octant index 0..=7 from sign bits (x=lsb, y, z=msb).
    fn octant_for(center: &[f32; 4], p: [f32; 3]) -> u32 {
        let mut o = 0u32;
        if p[0] >= center[0] {
            o |= 1;
        }
        if p[1] >= center[1] {
            o |= 2;
        }
        if p[2] >= center[2] {
            o |= 4;
        }
        o
    }

    fn child_center(parent_center: &[f32; 4], oct: u32) -> ([f32; 3], f32) {
        let h = parent_center[3] * 0.5;
        let cx = parent_center[0] + if (oct & 1) != 0 { h } else { -h };
        let cy = parent_center[1] + if (oct & 2) != 0 { h } else { -h };
        let cz = parent_center[2] + if (oct & 4) != 0 { h } else { -h };
        ([cx, cy, cz], h)
    }

    fn insert_body(
        &mut self,
        root: u32,
        p: [f32; 3],
        body_idx: u32,
        body_mass: f32,
        max_nodes: u32,
    ) -> Result<(), ()> {
        let mut node_idx = root;
        // Bounded depth: half-extent halves per level; past ~30 levels f32
        // loses meaning. Cap keeps the loop finite on coincident points.
        for _depth in 0..32 {
            let center = self.nodes[node_idx as usize].pos_size;
            let oct = Self::octant_for(&center, p);
            let child_idx = self.children[node_idx as usize][oct as usize];

            if child_idx == OCT_END {
                // Empty slot — drop a leaf here.
                if (self.nodes.len() as u32) >= max_nodes {
                    return Err(());
                }
                let (cc, hh) = Self::child_center(&center, oct);
                let new_idx = self.nodes.len() as u32;
                self.nodes.push(OctNodeRaw {
                    pos_size: [cc[0], cc[1], cc[2], hh],
                    com_mass: [p[0], p[1], p[2], body_mass],
                    meta: [body_idx, OCT_END, OCT_END, 0],
                });
                self.children.push([OCT_END; 8]);
                self.children[node_idx as usize][oct as usize] = new_idx;
                self.nodes[node_idx as usize].meta[3] += 1;
                return Ok(());
            }

            // Slot occupied.
            let child_meta_x = self.nodes[child_idx as usize].meta[0];
            if child_meta_x == OCT_BODY_INTERNAL {
                // Descend into existing internal node.
                node_idx = child_idx;
                continue;
            }
            // Existing leaf — promote it to internal so both bodies fit.
            let prev_com = self.nodes[child_idx as usize].com_mass;
            let prev_body = self.nodes[child_idx as usize].meta[0];
            self.nodes[child_idx as usize].com_mass = [0.0, 0.0, 0.0, 0.0];
            self.nodes[child_idx as usize].meta[0] = OCT_BODY_INTERNAL;
            self.nodes[child_idx as usize].meta[3] = 0;
            // Re-insert the displaced body underneath child_idx. If both bodies
            // share an exact position the depth cap breaks the recursion.
            self.insert_body(
                child_idx,
                [prev_com[0], prev_com[1], prev_com[2]],
                prev_body,
                prev_com[3].max(1e-3),
                max_nodes,
            )?;
            // Retry OUR body at this level — child_idx is now internal.
            node_idx = child_idx;
        }
        Ok(())
    }

    /// Iterative post-order traversal computing COM/mass on internal nodes.
    /// Leaves already carry com_mass from insertion.
    fn aggregate_com_postorder(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack: Vec<(u32, u32)> = Vec::with_capacity(64);
        stack.push((0, 0));
        while let Some(&(idx, cursor)) = stack.last() {
            if self.nodes[idx as usize].meta[0] != OCT_BODY_INTERNAL {
                stack.pop();
                continue;
            }
            if cursor < 8 {
                stack.last_mut().unwrap().1 = cursor + 1;
                let ch = self.children[idx as usize][cursor as usize];
                if ch != OCT_END {
                    stack.push((ch, 0));
                }
                continue;
            }
            let mut total_mass = 0.0f32;
            let mut com = [0.0f32; 3];
            for k in 0..8 {
                let ch = self.children[idx as usize][k];
                if ch == OCT_END {
                    continue;
                }
                let cm = self.nodes[ch as usize].com_mass;
                total_mass += cm[3];
                com[0] += cm[0] * cm[3];
                com[1] += cm[1] * cm[3];
                com[2] += cm[2] * cm[3];
            }
            if total_mass > 0.0 {
                com[0] /= total_mass;
                com[1] /= total_mass;
                com[2] /= total_mass;
            }
            self.nodes[idx as usize].com_mass = [com[0], com[1], com[2], total_mass];
            stack.pop();
        }
    }

    /// Pre-order DFS filling next_idx (first child) and skip_idx (next-sibling-
    /// or-uncle) — the rope that lets the WGSL traversal be stackless.
    fn assign_ropes(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        struct Frame {
            children_left: [u32; 8],
            outer_skip: u32,
        }
        let n = self.nodes.len();
        let mut skip = vec![OCT_END; n];
        let mut next = vec![OCT_END; n];

        let mut stack: Vec<Frame> = Vec::with_capacity(64);
        stack.push(Frame {
            children_left: if self.nodes[0].meta[0] == OCT_BODY_INTERNAL {
                self.children[0]
            } else {
                [OCT_END; 8]
            },
            outer_skip: OCT_END,
        });

        // Seed with the root (node 0): the first child we pick must become the
        // root's `next` (descend) pointer. Starting from `None` left `next[0]`
        // unset (OCT_END), so the WGSL walk descended from the root into nothing
        // and skipped ALL repulsion — every node saw zero repulsive force.
        let mut prev_visited: Option<u32> = Some(0);

        while stack.last().is_some() {
            let top = stack.last_mut().unwrap();
            let mut next_child = OCT_END;
            for k in 0..8 {
                if top.children_left[k] != OCT_END {
                    next_child = top.children_left[k];
                    top.children_left[k] = OCT_END;
                    break;
                }
            }
            if next_child != OCT_END {
                if let Some(prev) = prev_visited {
                    if next[prev as usize] == OCT_END {
                        next[prev as usize] = next_child;
                    }
                }
                // outer_skip for this child = first remaining sibling, else the
                // parent's outer_skip.
                let mut child_outer_skip = top.outer_skip;
                for k in 0..8 {
                    if top.children_left[k] != OCT_END {
                        child_outer_skip = top.children_left[k];
                        break;
                    }
                }
                skip[next_child as usize] = child_outer_skip;
                let is_internal =
                    self.nodes[next_child as usize].meta[0] == OCT_BODY_INTERNAL;
                stack.push(Frame {
                    children_left: if is_internal {
                        self.children[next_child as usize]
                    } else {
                        [OCT_END; 8]
                    },
                    outer_skip: child_outer_skip,
                });
                prev_visited = Some(next_child);
                continue;
            }
            stack.pop();
        }

        skip[0] = OCT_END;
        for i in 0..n {
            self.nodes[i].meta[1] = next[i];
            self.nodes[i].meta[2] = skip[i];
        }
    }
}

#[cfg(test)]
mod octree_tests {
    //! CPU-side correctness for the Barnes-Hut octree + a faithful replica of
    //! the WGSL rope walk (`fa2_barnes_hut.wgsl`). No GPU needed, so this runs
    //! in-sandbox and pins down whether a layout discrepancy is in the host
    //! build or the shader. With `theta = 0` the walk must reproduce brute-force
    //! repulsion exactly.

    use super::*;

    /// Deterministic, non-coincident spiral seed in vec4 stride (x,y,z,pad).
    fn positions_vec4(n: usize) -> Vec<f32> {
        let ga = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
        let mut p = Vec::with_capacity(n * 4);
        for i in 0..n {
            let r = (i as f32 + 1.0).sqrt();
            let a = i as f32 * ga;
            p.extend_from_slice(&[r * a.cos(), r * a.sin(), 0.0, 0.0]);
        }
        p
    }

    /// Brute-force FA2 repulsion on body `i`: sum over j != i of
    /// `d * scaling * deg_i * mass[j] / r²`, with `d = pos_i - pos_j`.
    fn brute_repulsion(pos4: &[f32], mass: &[f32], i: usize, scaling: f32) -> [f32; 3] {
        let n = mass.len();
        let pi = [pos4[i * 4], pos4[i * 4 + 1], pos4[i * 4 + 2]];
        let deg_i = mass[i];
        let mut f = [0.0f32; 3];
        for j in 0..n {
            if j == i {
                continue;
            }
            let d = [
                pi[0] - pos4[j * 4],
                pi[1] - pos4[j * 4 + 1],
                pi[2] - pos4[j * 4 + 2],
            ];
            let r2 = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1.0e-4);
            let c = scaling * deg_i * mass[j] / r2;
            f[0] += d[0] * c;
            f[1] += d[1] * c;
            f[2] += d[2] * c;
        }
        f
    }

    /// Faithful CPU replica of the WGSL rope walk in `fa2_barnes_hut.wgsl`.
    fn walk_repulsion(
        nodes: &[OctNodeRaw],
        pos_i: [f32; 3],
        i: u32,
        deg_i: f32,
        scaling: f32,
        theta: f32,
    ) -> [f32; 3] {
        let theta2 = theta * theta;
        let mut force = [0.0f32; 3];
        if nodes.is_empty() {
            return force;
        }
        let mut idx = 0u32;
        let cap = (nodes.len() as u32 * 4).max(16);
        let mut walk = 0u32;
        loop {
            if idx == OCT_END || walk >= cap {
                break;
            }
            walk += 1;
            let node = nodes[idx as usize];
            let body = node.meta[0];
            let com = [node.com_mass[0], node.com_mass[1], node.com_mass[2]];
            let mass_n = node.com_mass[3];
            let s = node.pos_size[3] * 2.0;
            let d = [pos_i[0] - com[0], pos_i[1] - com[1], pos_i[2] - com[2]];
            let r2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if body != OCT_BODY_INTERNAL {
                if body != i && mass_n > 0.0 {
                    let r2c = r2.max(1.0e-4);
                    let c = scaling * deg_i * mass_n / r2c;
                    force[0] += d[0] * c;
                    force[1] += d[1] * c;
                    force[2] += d[2] * c;
                }
                idx = node.meta[2]; // skip
                continue;
            }
            if mass_n > 0.0 && r2 > 0.0 && (s * s) < (theta2 * r2) {
                let r2c = r2.max(1.0e-4);
                let c = scaling * deg_i * mass_n / r2c;
                force[0] += d[0] * c;
                force[1] += d[1] * c;
                force[2] += d[2] * c;
                idx = node.meta[2]; // accept => skip subtree
            } else {
                idx = node.meta[1]; // descend
            }
        }
        force
    }

    #[test]
    fn octree_walk_theta0_matches_brute_force() {
        let n = 16usize;
        let pos4 = positions_vec4(n);
        let mass: Vec<f32> = (0..n).map(|i| (i % 3) as f32 + 1.0).collect();
        let scaling = 2.0f32;

        let mut build = OctreeBuild::default();
        let used = build.rebuild(&pos4, &mass, n as u32, (2 * n as u32) + 8);
        assert!(used > 0, "octree should be non-empty (used={used})");

        // Sanity: total leaf mass == sum of body masses (no bodies dropped).
        let leaf_mass: f32 = build
            .nodes
            .iter()
            .filter(|nd| nd.meta[0] != OCT_BODY_INTERNAL)
            .map(|nd| nd.com_mass[3])
            .sum();
        let want_mass: f32 = mass.iter().sum();
        assert!(
            (leaf_mass - want_mass).abs() < 1e-3,
            "every body must be a leaf: leaf_mass={leaf_mass} want={want_mass}"
        );

        // theta = 0 ⇒ the walk visits every leaf ⇒ exact brute-force repulsion.
        let mut max_rel = 0.0f32;
        for i in 0..n {
            let pi = [pos4[i * 4], pos4[i * 4 + 1], pos4[i * 4 + 2]];
            let bh = walk_repulsion(&build.nodes, pi, i as u32, mass[i], scaling, 0.0);
            let bf = brute_repulsion(&pos4, &mass, i, scaling);
            for k in 0..3 {
                let denom = bf[k].abs().max(1.0);
                max_rel = max_rel.max((bh[k] - bf[k]).abs() / denom);
            }
        }
        assert!(
            max_rel < 1e-3,
            "Barnes-Hut walk (theta=0) must equal brute force; max rel diff = {max_rel}"
        );
    }
}
