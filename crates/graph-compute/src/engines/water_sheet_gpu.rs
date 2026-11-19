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
//! Work is O(N · R²) with R bounded (staged tiles cap it at 8), never O(N²): every force is a fixed-order sum
//! over a bounded window, so the kernels stay atomic-free and deterministic.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use wgpu::util::DeviceExt;

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};
use crate::analytics::workgroup_dims_2d;
use crate::water::{cpu, WaterPrecision, WaterSheetParams, WaterSheetSettings};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "water-sheet-gpu";

/// Implementation actually running on this device. `F64Pos` needs no device
/// feature; `F64` is only reachable on adapters exposing `SHADER_F64` (the
/// engine hard-errors otherwise, naming the mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectiveMode {
    /// f32 storage and arithmetic (the original kernel).
    F32,
    /// 64-bit fixed-point positions (u32 pair, 2^-40 quantum), f32
    /// velocities and forces. Needs no device feature.
    F64Pos,
    /// Native f64 storage and arithmetic (requires `SHADER_F64`).
    F64,
}

impl EffectiveMode {
    fn pos_bytes(self) -> u64 {
        match self {
            EffectiveMode::F32 => 4,
            EffectiveMode::F64Pos | EffectiveMode::F64 => 8,
        }
    }

    fn vel_bytes(self) -> u64 {
        match self {
            EffectiveMode::F64 => 8,
            EffectiveMode::F32 | EffectiveMode::F64Pos => 4,
        }
    }
}

/// Workgroup shape of the staged sheet stencil (8×8 → a `(8+2R)²` shared tile;
/// must match the WGSL const). The radius `R` is `settings.r_sheet`,
/// substituted into the WGSL at pipeline build.
const SHEET_WG: u32 = 8;
/// Largest staged radius (shared tile `(8+2R)²` stays well under 32 KiB).
const MAX_STAGED_RADIUS: u32 = 8;

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
    params_buf: wgpu::Buffer,
    // One contiguous MAP_READ buffer for the 3 out-axis copies.
    readback: Option<wgpu::Buffer>,

    params: WaterSheetParams,
    settings: WaterSheetSettings,
    mode: EffectiveMode,
    /// Bytes of one position axis (readback copy size per axis).
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

    /// Encode `n` ping-pong steps into ONE compute pass / command buffer and
    /// submit it, with no host readback. Pure GPU throughput; pair with
    /// [`sync`](Self::sync) to wait for completion.
    pub fn step_n(&mut self, n: u32) {
        let gpu = self.gpu.as_mut().expect("step_n before init");
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut idx = gpu.read_idx;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            let p = &gpu.params;
            let s = &gpu.settings;
            let wg = s.workgroup.max(1);
            let (sheet_gx, sheet_gy) = (p.cols.div_ceil(SHEET_WG).max(1), p.rows.div_ceil(SHEET_WG).max(1));
            let mol_dims = workgroup_dims_2d((3 * p.molecules).div_ceil(wg).max(1));
            let all_dims = workgroup_dims_2d(p.n_nodes().div_ceil(wg).max(1));
            for _ in 0..n {
                pass.set_bind_group(0, &gpu.bind_groups[idx], &[]);
                if s.staged {
                    // Sheet stencil: 2-D grid of 8×8 workgroups over (cols, rows).
                    pass.set_pipeline(&gpu.pipelines[1]);
                    pass.dispatch_workgroups(sheet_gx, sheet_gy, 1);
                    // Molecule section: 1-D over 3M nodes, tiled into 2-D if huge.
                    pass.set_pipeline(&gpu.pipelines[2]);
                    pass.dispatch_workgroups(mol_dims.0, mol_dims.1, mol_dims.2);
                } else {
                    pass.set_pipeline(&gpu.pipelines[0]);
                    pass.dispatch_workgroups(all_dims.0, all_dims.1, all_dims.2);
                }
                idx ^= 1;
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        gpu.read_idx = idx;
    }

    /// One step, no readback.
    pub fn step_deferred(&mut self) {
        self.step_n(1);
    }

    /// Block until every submitted step has finished on the GPU (queue sync).
    pub fn sync(&self) {
        let gpu = self.gpu.as_ref().expect("sync before init");
        gpu.device.poll(wgpu::Maintain::Wait);
    }

    /// Total bytes of device buffers this engine holds (excluding the lazily
    /// created readback buffer). Per-node: 6 ping-pong position axes + 3
    /// velocity axes, each 4 B (f32) or 8 B (f64/f64pos positions) per node.
    pub fn device_bytes(&self) -> u64 {
        let gpu = self.gpu.as_ref().expect("device_bytes before init");
        let n = gpu.params.n_nodes() as u64;
        let (pb, vb) = (gpu.mode.pos_bytes(), gpu.mode.vel_bytes());
        6 * pb * n + 3 * vb * n + std::mem::size_of::<WaterParamsRaw>() as u64
    }

    /// Copy the current positions into the (lazily created) readback buffer,
    /// map it, and run `f` over the mapped bytes. `n` is the node count and
    /// `mode` the effective precision, so `f` can unpack per mode.
    fn map_readback<R>(&mut self, f: impl FnOnce(&[u8], usize, EffectiveMode) -> R) -> R {
        let gpu = self.gpu.as_mut().expect("readback before init");
        let nb = gpu.n_bytes_per_axis;
        let n = gpu.params.n_nodes() as usize;
        let mode = gpu.mode;
        if gpu.readback.is_none() {
            gpu.readback = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("water_readback"),
                size: 3 * nb,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let readback = gpu.readback.as_ref().unwrap();
        // The current positions are the INPUT buffers of the next step.
        let (px, py, pz) = if gpu.read_idx == 0 {
            (&gpu.px_a, &gpu.py_a, &gpu.pz_a)
        } else {
            (&gpu.px_b, &gpu.py_b, &gpu.pz_b)
        };
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(px, 0, readback, 0, nb);
        encoder.copy_buffer_to_buffer(py, 0, readback, nb, nb);
        encoder.copy_buffer_to_buffer(pz, 0, readback, 2 * nb, nb);
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let out = f(&data, n, mode);
        drop(data);
        readback.unmap();
        out
    }

    /// Unpack mapped readback bytes into SoA f32 axes — the display / force
    /// precision. The packed `F64Pos` mode collapses each `(hi, lo)` pair with
    /// an f32 add, exactly the value its force model consumes.
    fn unpack_f32(
        data: &[u8],
        n: usize,
        mode: EffectiveMode,
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let mut x = vec![0.0f32; n];
        let mut y = vec![0.0f32; n];
        let mut z = vec![0.0f32; n];
        let stride = match mode {
            EffectiveMode::F32 => 4,
            EffectiveMode::F64Pos | EffectiveMode::F64 => 8,
        };
        for (axis, out) in [&mut x, &mut y, &mut z].into_iter().enumerate() {
            let buf = &data[axis * stride * n..(axis + 1) * stride * n];
            match mode {
                EffectiveMode::F32 => {
                    out.copy_from_slice(bytemuck::cast_slice(buf));
                }
                EffectiveMode::F64Pos => {
                    let u: &[u32] = bytemuck::cast_slice(buf);
                    for (i, slot) in out.iter_mut().enumerate() {
                        *slot = (u[2 * i] as i32) as f32 / 256.0
                            + (u[2 * i + 1] as f32) / 1099511627776.0;
                    }
                }
                EffectiveMode::F64 => {
                    let v: &[f64] = bytemuck::cast_slice(buf);
                    for (i, slot) in out.iter_mut().enumerate() {
                        *slot = v[i] as f32;
                    }
                }
            }
        }
        (x, y, z)
    }

    /// Unpack mapped readback bytes into SoA f64 axes — the full stored state.
    /// The `F64Pos` mode decodes its 64-bit fixed-point pair in f64, so the
    /// 2^-40 position quantum is not collapsed to a single f32.
    fn unpack_f64(
        data: &[u8],
        n: usize,
        mode: EffectiveMode,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut x = vec![0.0f64; n];
        let mut y = vec![0.0f64; n];
        let mut z = vec![0.0f64; n];
        let stride = match mode {
            EffectiveMode::F32 => 4,
            EffectiveMode::F64Pos | EffectiveMode::F64 => 8,
        };
        for (axis, out) in [&mut x, &mut y, &mut z].into_iter().enumerate() {
            let buf = &data[axis * stride * n..(axis + 1) * stride * n];
            match mode {
                EffectiveMode::F32 => {
                    let v: &[f32] = bytemuck::cast_slice(buf);
                    for (i, slot) in out.iter_mut().enumerate() {
                        *slot = f64::from(v[i]);
                    }
                }
                EffectiveMode::F64Pos => {
                    let u: &[u32] = bytemuck::cast_slice(buf);
                    for (i, slot) in out.iter_mut().enumerate() {
                        *slot = f64::from(u[2 * i] as i32) / 256.0
                            + f64::from(u[2 * i + 1]) / 1099511627776.0;
                    }
                }
                EffectiveMode::F64 => {
                    let v: &[f64] = bytemuck::cast_slice(buf);
                    out.copy_from_slice(v);
                }
            }
        }
        (x, y, z)
    }

    /// Copy the current positions into the (lazily created) readback buffer,
    /// map it, and return the interleaved f32 positions.
    pub fn read_positions(&mut self) -> Vec<f32> {
        let (x, y, z) = self.map_readback(Self::unpack_f32);
        let gpu = self.gpu.as_mut().expect("read_positions before init");
        gpu.host.x = x;
        gpu.host.y = y;
        gpu.host.z = z;
        gpu.host.to_interleaved()
    }

    /// Read the current positions back as interleaved f64. The `F64Pos` mode's
    /// fixed-point components are decoded in f64, preserving the 2^-40 position
    /// quantum that the f32 display readback rounds away. Used by the
    /// precision drift tests.
    pub fn read_positions_f64(&mut self) -> Vec<f64> {
        let n = self
            .gpu
            .as_ref()
            .expect("read_positions_f64 before init")
            .params
            .n_nodes() as usize;
        let (x, y, z) = self.map_readback(Self::unpack_f64);
        let mut out = Vec::with_capacity(3 * n);
        for i in 0..n {
            out.push(x[i]);
            out.push(y[i]);
            out.push(z[i]);
        }
        out
    }

    /// SoA positions as of the last [`read_positions`](Self::read_positions).
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
        if s.workgroup == 0 || s.workgroup > 1024 {
            return Err(format!(
                "water-sheet: workgroup {} out of range 1..=1024",
                s.workgroup
            ));
        }
        if s.staged && s.r_sheet > MAX_STAGED_RADIUS {
            return Err(format!(
                "water-sheet: staged kernel supports r_sheet <= {MAX_STAGED_RADIUS}, got {}",
                s.r_sheet
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
        // Shape resolution: an explicit `set_params` whose shape matches the
        // graph is used as-is (validated below, as before); otherwise the
        // shape is inferred from the CSR, so a bare graph push (UI engine
        // picker, graph-api compute broker) needs no `set_params`. Tuning
        // knobs come from `set_params` when present, else the defaults.
        let graph = shard.graph;
        let settings = match &self.settings {
            Some(s) if s.params().n_nodes() == graph.n_nodes => s.clone(),
            _ => {
                let inferred = WaterSheetParams::infer_from_csr(graph)?;
                match &self.settings {
                    Some(s) => WaterSheetSettings {
                        molecules: inferred.molecules,
                        rows: inferred.rows,
                        cols: inferred.cols,
                        ..*s
                    },
                    None => WaterSheetSettings {
                        molecules: inferred.molecules,
                        rows: inferred.rows,
                        cols: inferred.cols,
                        ..WaterSheetSettings::default()
                    },
                }
            }
        };
        let params = settings.params();
        params.validate_csr(graph)?;

        let device = gpu_ctx.device.clone();
        let queue = gpu_ctx.queue.clone();
        let n = params.n_nodes() as usize;
        if positions.len() != 3 * n {
            return Err(format!(
                "water-sheet-gpu: seed has {} floats, graph needs {}",
                positions.len(),
                3 * n
            ));
        }
        let host = cpu::Positions::from_interleaved(positions);

        // Effective precision: `F64Pos` needs no device feature, so it always
        // runs. `F64` is native WGSL f64 and requires `SHADER_F64`; on an
        // adapter without it (Apple GPUs expose no fp64) the request is a hard
        // error naming the mode — never a silent precision downgrade.
        let has_f64 = device.features().contains(wgpu::Features::SHADER_F64);
        let mode = match settings.precision {
            WaterPrecision::F32 => EffectiveMode::F32,
            WaterPrecision::F64Pos => EffectiveMode::F64Pos,
            WaterPrecision::F64 if has_f64 => EffectiveMode::F64,
            WaterPrecision::F64 => {
                return Err(format!(
                    "water-sheet-gpu: precision 'f64' requires wgpu::Features::SHADER_F64, \
                     not exposed by this adapter ({} / {:?}); use precision 'f64pos' \
                     (64-bit fixed-point positions, no fp64 hardware needed) or 'f32'",
                    gpu_ctx.adapter_info.name, gpu_ctx.adapter_info.backend
                ));
            }
        };
        tracing::info!(
            requested = ?settings.precision,
            effective = ?mode,
            shader_f64 = has_f64,
            "water-sheet precision mode"
        );
        let nb = mode.pos_bytes() * n as u64;

        let usage = wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST;
        let init_buf = |label: &'static str, contents: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let init_f32 = |label: &'static str, axis: &[f32]| init_buf(label, bytemuck::cast_slice(axis));
        let init_u32 = |label: &'static str, axis: &[u32]| init_buf(label, bytemuck::cast_slice(axis));
        let init_f64 = |label: &'static str, axis: &[f64]| init_buf(label, bytemuck::cast_slice(axis));

        // Ping-pong SoA positions, seeded identically so a 0-step read is correct.
        let (px_a, py_a, pz_a, px_b, py_b, pz_b, vx, vy, vz) = match mode {
            EffectiveMode::F32 => {
                let z = vec![0.0f32; n];
                (
                    init_f32("water_px_a", &host.x),
                    init_f32("water_py_a", &host.y),
                    init_f32("water_pz_a", &host.z),
                    init_f32("water_px_b", &host.x),
                    init_f32("water_py_b", &host.y),
                    init_f32("water_pz_b", &host.z),
                    init_f32("water_vx", &z),
                    init_f32("water_vy", &z),
                    init_f32("water_vz", &z),
                )
            }
            EffectiveMode::F64Pos => {
                // 64-bit fixed-point (integer word, fraction word), scale
                // 2^-40: value = hi/2^8 + lo/2^40. The seed is f32, so the
                // split is exact in f64.
                let fixed = |axis: &[f32]| -> Vec<u32> {
                    let mut v = vec![0u32; 2 * n];
                    for (i, &x) in axis.iter().enumerate() {
                        let scaled = f64::from(x) * 1099511627776.0; // * 2^40
                        let hi = (scaled / 4294967296.0).floor();
                        let lo = scaled - hi * 4294967296.0;
                        v[2 * i] = (hi as i32) as u32;
                        v[2 * i + 1] = lo as u32;
                    }
                    v
                };
                let (pxa, pya, pza) = (fixed(&host.x), fixed(&host.y), fixed(&host.z));
                let z = vec![0.0f32; n];
                (
                    init_u32("water_px_a", &pxa),
                    init_u32("water_py_a", &pya),
                    init_u32("water_pz_a", &pza),
                    init_u32("water_px_b", &pxa),
                    init_u32("water_py_b", &pya),
                    init_u32("water_pz_b", &pza),
                    init_f32("water_vx", &z),
                    init_f32("water_vy", &z),
                    init_f32("water_vz", &z),
                )
            }
            EffectiveMode::F64 => {
                let widen =
                    |axis: &[f32]| -> Vec<f64> { axis.iter().map(|&x| f64::from(x)).collect() };
                let (pxa, pya, pza) = (widen(&host.x), widen(&host.y), widen(&host.z));
                let z = vec![0.0f64; n];
                (
                    init_f64("water_px_a", &pxa),
                    init_f64("water_py_a", &pya),
                    init_f64("water_pz_a", &pza),
                    init_f64("water_px_b", &pxa),
                    init_f64("water_py_b", &pya),
                    init_f64("water_pz_b", &pza),
                    init_f64("water_vx", &z),
                    init_f64("water_vy", &z),
                    init_f64("water_vz", &z),
                )
            }
        };

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

        // The 1-D kernels' workgroup size and the staged tile radius are WGSL
        // consts; substitute the configured values so they are tunable per run.
        let src = match mode {
            EffectiveMode::F32 => include_str!("../shaders/water_sheet.wgsl"),
            EffectiveMode::F64Pos => include_str!("../shaders/water_sheet_f64pos.wgsl"),
            EffectiveMode::F64 => include_str!("../shaders/water_sheet_f64.wgsl"),
        };
        let wgsl = src
            .replace(
                "const NAIVE_WG: u32 = 256u;",
                &format!("const NAIVE_WG: u32 = {}u;", settings.workgroup),
            )
            .replace(
                "const R_S_STAGE: u32 = 3u;",
                &format!("const R_S_STAGE: u32 = {}u;", settings.r_sheet),
            );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water_sheet_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(wgsl)),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
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
                    wgpu::BindGroupEntry { binding: 9, resource: params_buf.as_entire_binding() },
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
            params_buf,
            readback: None,
            params,
            settings,
            mode,
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
