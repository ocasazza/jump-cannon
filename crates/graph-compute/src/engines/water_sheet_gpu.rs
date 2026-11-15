//! Water-sheet GPU engine (`"water-sheet-gpu"`).
//!
//! GPU-resident layout for the `waterSheet` topology (a molecule pool of
//! 3-node H–O–H paths plus a rows×cols sheet lattice — the SoA graph from
//! `native-graph-generator` / `graph.nix`). Positions live GPU-side as SoA
//! (one storage buffer per axis), ping-ponged deterministically like
//! [`GeometricGpuEngine`](super::geometric_gpu::GeometricGpuEngine).
//!
//! Passes per frame (single compute pass, two dispatches when staged):
//!   - sheet pass — 2-D dispatch over the lattice grid; with `staged` on, the
//!     repulsion window is staged through workgroup shared memory (stencil
//!     tiling) so 100M-node sheets don't re-read each position dozens of
//!     times;
//!   - molecule pass — 1-D dispatch over the molecule section (windowed O–O
//!     repulsion + O–H bonds).
//!
//! Work is O(N · R²) with R ≤ 3, never O(N²): every force is a fixed-order sum
//! over a bounded window, so the kernels stay atomic-free and deterministic.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use wgpu::util::DeviceExt;

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};
use crate::analytics::workgroup_dims_2d;
use crate::water::{cpu, WaterSheetParams, WaterSheetSettings};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "water-sheet-gpu";

/// Workgroup shape of the staged sheet stencil (8×8 → a 14×14 shared tile at
/// `R_S_STAGE = 3`; must match the WGSL consts).
const SHEET_WG: u32 = 8;
/// Compile-time radius of the staged sheet kernel; must match the WGSL const.
const R_S_STAGE: u32 = 3;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct WaterParamsRaw {
    n_nodes: u32,
    n_mol: u32,
    cols: u32,
    rows: u32,
    mol_grid_w: u32,
    r_sheet: u32,
    r_mol: u32,
    _pad0: u32,
    bond_rest: f32,
    sheet_rest: f32,
    k_bond: f32,
    k_rep: f32,
    rcut: f32,
    dt: f32,
    damping: f32,
    _pad1: f32,
}

#[allow(dead_code)]
struct Gpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    /// `pipelines[0]` = `step_naive`; `pipelines[1]`/`[2]` = `sheet_staged` +
    /// `mol_staged` (dispatched together in staged mode).
    pipelines: [wgpu::ComputePipeline; 3],
    /// Two bind groups for SoA position ping-pong (timeline P2 determinism):
    /// group 0 reads A → writes B, group 1 reads B → writes A.
    bind_groups: [wgpu::BindGroup; 2],
    read_idx: usize,

    // SoA positions, ping-pong pairs (a/b).
    px_a: wgpu::Buffer,
    py_a: wgpu::Buffer,
    pz_a: wgpu::Buffer,
    px_b: wgpu::Buffer,
    py_b: wgpu::Buffer,
    pz_b: wgpu::Buffer,
    // SoA velocities (in place; each invocation owns its node's slot).
    vx: wgpu::Buffer,
    vy: wgpu::Buffer,
    vz: wgpu::Buffer,
    csr_offsets: wgpu::Buffer,
    csr_neighbors: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    // One contiguous MAP_READ buffer for the 3 out-axis copies.
    readback: wgpu::Buffer,

    params: WaterSheetParams,
    settings: WaterSheetSettings,
    n_bytes_per_axis: u64,
    /// Host copy of the current positions (SoA), refreshed on readback.
    host: cpu::Positions,
}

pub struct WaterSheetGpuEngine {
    descriptor: LayoutDescriptor,
    settings: Option<WaterSheetSettings>,
    gpu: Option<Gpu>,
}

impl WaterSheetGpuEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: LayoutDescriptor {
                id: LAYOUT_ID,
                kind: LayoutKind::Physics,
                display_name: "Water sheet (GPU)",
                description: "GPU-resident SoA water-sheet layout: windowed lattice/molecule \
                              repulsion + sheet-bond springs, 2-D dispatch tiling for 100M+ nodes.",
                requirements: LayoutRequirements {
                    needs_edges: true,
                    needs_cpu_positions: true,
                    needs_gpu_positions_buffer: false,
                },
            },
            settings: None,
            gpu: None,
        }
    }

    /// Dispatch `steps` frames without host readback (pure GPU throughput).
    pub fn step_deferred(&mut self) {
        let gpu = self.gpu.as_mut().expect("step_deferred before init");
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            let p = &gpu.params;
            let s = &gpu.settings;
            let wg = s.workgroup.max(1);
            pass.set_bind_group(0, &gpu.bind_groups[gpu.read_idx], &[]);
            if s.staged {
                // Sheet stencil: 2-D grid of 8×8 workgroups over (cols, rows).
                let gx = p.cols.div_ceil(SHEET_WG).max(1);
                let gy = p.rows.div_ceil(SHEET_WG).max(1);
                pass.set_pipeline(&gpu.pipelines[1]);
                pass.dispatch_workgroups(gx, gy, 1);
                // Molecule section: 1-D over 3M nodes, tiled into 2-D if huge.
                let mols_wg = (3 * p.molecules).div_ceil(wg).max(1);
                let (mx, my, mz) = workgroup_dims_2d(mols_wg);
                pass.set_pipeline(&gpu.pipelines[2]);
                pass.dispatch_workgroups(mx, my, mz);
            } else {
                let total = p.n_nodes().div_ceil(wg).max(1);
                let (x, y, z) = workgroup_dims_2d(total);
                pass.set_pipeline(&gpu.pipelines[0]);
                pass.dispatch_workgroups(x, y, z);
            }
        }
        // OUT buffer for this step: group 0 wrote B, group 1 wrote A.
        let (out_px, out_py, out_pz) = if gpu.read_idx == 0 {
            (&gpu.px_b, &gpu.py_b, &gpu.pz_b)
        } else {
            (&gpu.px_a, &gpu.py_a, &gpu.pz_a)
        };
        let nb = gpu.n_bytes_per_axis;
        encoder.copy_buffer_to_buffer(out_px, 0, &gpu.readback, 0, nb);
        encoder.copy_buffer_to_buffer(out_py, 0, &gpu.readback, nb, nb);
        encoder.copy_buffer_to_buffer(out_pz, 0, &gpu.readback, 2 * nb, nb);
        gpu.queue.submit(std::iter::once(encoder.finish()));
        gpu.read_idx ^= 1;
    }

    /// Map the readback buffer and return the current interleaved positions.
    pub fn read_positions(&mut self) -> Vec<f32> {
        let gpu = self.gpu.as_mut().expect("read_positions before init");
        gpu.device.poll(wgpu::Maintain::Wait);
        let slice = gpu.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let n = gpu.params.n_nodes() as usize;
        let xs: &[f32] = bytemuck::cast_slice(&data[0..4 * n]);
        let ys: &[f32] = bytemuck::cast_slice(&data[4 * n..8 * n]);
        let zs: &[f32] = bytemuck::cast_slice(&data[8 * n..12 * n]);
        gpu.host.x.copy_from_slice(xs);
        gpu.host.y.copy_from_slice(ys);
        gpu.host.z.copy_from_slice(zs);
        let out = gpu.host.to_interleaved();
        drop(data);
        gpu.readback.unmap();
        out
    }

    /// Readback-free current SoA positions (valid after any deferred step).
    pub fn host_positions(&self) -> &cpu::Positions {
        &self.gpu.as_ref().expect("host_positions before init").host
    }
}

impl Default for WaterSheetGpuEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for WaterSheetGpuEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, v: &serde_json::Value) -> Result<(), String> {
        if v.is_null() {
            return Ok(());
        }
        let s: WaterSheetSettings =
            serde_json::from_value(v.clone()).map_err(|e| format!("decode water-sheet settings: {e}"))?;
        if s.staged && s.r_sheet != R_S_STAGE {
            return Err(format!(
                "water-sheet: staged kernel fixes its repulsion radius at {R_S_STAGE}; \
                 set r_sheet = {R_S_STAGE} or staged = false"
            ));
        }
        self.settings = Some(s);
        Ok(())
    }

    fn init(
        &mut self,
        ctx: &mut EngineCtx,
        shard: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let gpu_ctx = ctx
            .gpu
            .as_ref()
            .ok_or_else(|| "water-sheet-gpu requires a wgpu device".to_string())?;
        let settings = self
            .settings
            .clone()
            .ok_or_else(|| "water-sheet-gpu requires set_params (waterSheet shape) before init")?;
        let params = settings.params();
        params.validate_csr(shard.graph)?;

        let device = gpu_ctx.device.clone();
        let queue = gpu_ctx.queue.clone();
        let n = params.n_nodes() as usize;
        let nb = 4u64 * n as u64;
        if positions.len() != 3 * n {
            return Err(format!(
                "water-sheet-gpu: seed has {} floats, graph needs {}",
                positions.len(),
                3 * n
            ));
        }
        let host = cpu::Positions::from_interleaved(positions);

        let soa_init = |label: &'static str, axis: &[f32]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(axis),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            })
        };
        // Ping-pong SoA positions, seeded identically so a 0-step read is correct.
        let px_a = soa_init("water_px_a", &host.x);
        let py_a = soa_init("water_py_a", &host.y);
        let pz_a = soa_init("water_pz_a", &host.z);
        let px_b = soa_init("water_px_b", &host.x);
        let py_b = soa_init("water_py_b", &host.y);
        let pz_b = soa_init("water_pz_b", &host.z);
        let vx = soa_init("water_vx", &vec![0.0f32; n]);
        let vy = soa_init("water_vy", &vec![0.0f32; n]);
        let vz = soa_init("water_vz", &vec![0.0f32; n]);

        let csr_offsets = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("water_csr_offsets"),
            contents: bytemuck::cast_slice(&shard.graph.offsets),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let csr_neighbors = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("water_csr_neighbors"),
            contents: bytemuck::cast_slice(&shard.graph.neighbors),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let w = params.mol_grid_w();
        let raw = WaterParamsRaw {
            n_nodes: params.n_nodes(),
            n_mol: params.molecules,
            cols: params.cols,
            rows: params.rows,
            mol_grid_w: w,
            r_sheet: settings.r_sheet,
            r_mol: settings.r_mol,
            _pad0: 0,
            bond_rest: settings.bond_rest,
            sheet_rest: settings.sheet_rest,
            k_bond: settings.k_bond,
            k_rep: settings.k_rep,
            rcut: settings.rcut,
            dt: settings.dt,
            damping: settings.damping,
            _pad1: 0.0,
        };
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("water_params"),
            contents: bytemuck::bytes_of(&raw),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("water_readback"),
            size: 3 * nb,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water_sheet_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/water_sheet.wgsl"
            ))),
        });

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
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water_bgl"),
            entries: &[
                storage_ro(0),
                storage_ro(1),
                storage_ro(2),
                storage_rw(3),
                storage_rw(4),
                storage_rw(5),
                storage_rw(6),
                storage_rw(7),
                storage_rw(8),
                storage_ro(9),
                storage_ro(10),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipe = |entry: &'static str, label: &'static str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("water_layout"),
                    bind_group_layouts: &[&bgl],
                    push_constant_ranges: &[],
                })),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let pipelines = [
            pipe("step_naive", "water_pipe_naive"),
            pipe("sheet_staged", "water_pipe_sheet_staged"),
            pipe("mol_staged", "water_pipe_mol_staged"),
        ];

        let make_bg = |label: &'static str,
                       (ix, iy, iz): (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer),
                       (ox, oy, oz): (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer)| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ix.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: iy.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: iz.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: ox.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: oy.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: oz.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: vx.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: vy.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 8, resource: vz.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 9, resource: csr_offsets.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 10, resource: csr_neighbors.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 11, resource: params_buf.as_entire_binding() },
                ],
            })
        };
        let bind_groups = [
            make_bg(
                "water_bg_a_to_b",
                (&px_a, &py_a, &pz_a),
                (&px_b, &py_b, &pz_b),
            ),
            make_bg(
                "water_bg_b_to_a",
                (&px_b, &py_b, &pz_b),
                (&px_a, &py_a, &pz_a),
            ),
        ];

        self.gpu = Some(Gpu {
            device,
            queue,
            pipelines,
            bind_groups,
            read_idx: 0,
            px_a,
            py_a,
            pz_a,
            px_b,
            py_b,
            pz_b,
            vx,
            vy,
            vz,
            csr_offsets,
            csr_neighbors,
            params_buf,
            readback,
            params,
            settings,
            n_bytes_per_axis: nb,
            host,
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        self.step_deferred();
        StepOutput::positions_only(self.read_positions())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_raw_layout_is_64_bytes() {
        assert_eq!(std::mem::size_of::<WaterParamsRaw>(), 64);
    }
}
