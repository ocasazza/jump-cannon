//! Device-side multilevel coarsening seed for the GPU force layout.
//!
//! Builds a heavy-edge-matching coarsening cascade from the fine CSR entirely
//! on the GPU, lays out the coarsest level, and prolongs positions back down
//! into the fine positions buffer — with zero host readback and no per-node
//! host allocation. Selected through [`super::gpu_force::SeedMode::GpuMultilevel`].
//!
//! The cascade is *planned* on the host from the fine node/edge counts alone:
//! each level's node and edge capacities follow a fixed geometric ratio, and
//! every dispatch runs over that host-known capacity while guarding its lanes
//! against the device-computed counts stored in each level's `meta` buffer.
//! Coarse indices are clamped to their level capacity, so a graph that fails
//! to coarsen at the assumed ratio degrades the seed (a few contracted nodes
//! or edges merge into the last slot) rather than overflowing a buffer. The
//! no-progress / exact-count stop rules from the classic algorithm cannot be
//! observed without a readback, so they are subsumed by this schedule: a
//! non-coarsening level is a near-identity level, which the per-level force
//! relaxation simply passes through.
//!
//! Kernels live in `shaders/multilevel.wgsl`; the repulsion + integration
//! step reuses `force.wgsl`'s `force_step` unchanged (the coarse levels bind
//! their own buffers to its layout and run it in `NegativeSampling` mode, so
//! no octree scratch is allocated per level). Only the weighted spring kernel
//! (`spring_step_weighted`) is new; the fine level relaxes with unit weights,
//! which reproduces the unweighted spring exactly.

use std::collections::HashMap;
use wgpu::util::DeviceExt;

use super::gpu_force::{dispatch_grid, ml_coarse_params_bytes, GpuForceOptions};

const CAP_RATIO: f32 = 0.6;
const COARSEST_TARGET: u32 = 1000;
const MAX_LEVELS: usize = 16;
const HUB: u32 = 32;
const MASS_SCALE: f32 = 65536.0;
const WG: u32 = 64;

// Byte offsets of `n_nodes` / `n_edges` inside force.wgsl's `SimParams`
// uniform (see `SimParamsRaw` in gpu_force.rs): three 16-byte rows precede
// `cursor_pos` (12 bytes) + `n_nodes`, so n_nodes lands at 44 and n_edges at
// 48. The device coarse counts are spliced in here without a host readback.
const SIM_N_NODES_OFFSET: u64 = 44;
const SIM_N_EDGES_OFFSET: u64 = 48;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MlParamsRaw {
    n_fine_cap: u32,
    slots_fine_cap: u32,
    nc_cap: u32,
    mc_cap: u32,

    round: u32,
    seed: u32,
    level: u32,
    _pad0: u32,

    spring_len: f32,
    radius: f32,
    mass_scale: f32,
    _pad1: f32,

    sort_nblocks: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MlScanDims {
    len: u32,
    nblocks: u32,
    _p0: u32,
    _p1: u32,
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn kbg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    entries: &[(u32, &wgpu::Buffer)],
) -> wgpu::BindGroup {
    let e: Vec<wgpu::BindGroupEntry> = entries
        .iter()
        .map(|(b, buf)| wgpu::BindGroupEntry {
            binding: *b,
            resource: buf.as_entire_binding(),
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ml_bg"),
        layout,
        entries: &e,
    })
}

fn pass1(
    encoder: &mut wgpu::CommandEncoder,
    pipe: &wgpu::ComputePipeline,
    bg0: &wgpu::BindGroup,
    bg1: &wgpu::BindGroup,
    grid: (u32, u32),
) {
    let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("ml_pass"),
        timestamp_writes: None,
    });
    cp.set_pipeline(pipe);
    cp.set_bind_group(0, bg0, &[]);
    cp.set_bind_group(1, bg1, &[]);
    cp.dispatch_workgroups(grid.0, grid.1, 1);
}

fn pass3(
    encoder: &mut wgpu::CommandEncoder,
    pipe: &wgpu::ComputePipeline,
    bg0: &wgpu::BindGroup,
    bg1: &wgpu::BindGroup,
    bg2: &wgpu::BindGroup,
    grid: (u32, u32),
) {
    let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("ml_force_pass"),
        timestamp_writes: None,
    });
    cp.set_pipeline(pipe);
    cp.set_bind_group(0, bg0, &[]);
    cp.set_bind_group(1, bg1, &[]);
    cp.set_bind_group(2, bg2, &[]);
    cp.dispatch_workgroups(grid.0, grid.1, 1);
}

/// One compute pipeline plus its group-0 bind-group layout.
struct KPipe {
    pipe: wgpu::ComputePipeline,
    g0: wgpu::BindGroupLayout,
}

/// All multilevel.wgsl pipelines and the three group-1 uniform layouts.
struct MlPipes {
    g1_params: wgpu::BindGroupLayout,
    g1_scan: wgpu::BindGroupLayout,
    g1_sp: wgpu::BindGroupLayout,

    init_matched: KPipe,
    fill_ones: KPipe,
    hem_propose: KPipe,
    hem_match: KPipe,
    rep_flag: KPipe,
    write_nc: KPipe,
    assign_parent: KPipe,
    mass_accum: KPipe,
    place_mass: KPipe,
    row_of_slot: KPipe,
    expand_pairs: KPipe,
    histogram_g0: wgpu::BindGroupLayout,
    histogram: Vec<wgpu::ComputePipeline>,
    scatter_g0: wgpu::BindGroupLayout,
    scatter: Vec<wgpu::ComputePipeline>,
    pair_swap: KPipe,
    scan_local: KPipe,
    scan_serial: KPipe,
    scan_fixup: KPipe,
    edge_flag: KPipe,
    write_mc: KPipe,
    edge_accumulate: KPipe,
    coarse_deg: KPipe,
    write_slots: KPipe,
    coarse_fill: KPipe,
    coarse_nv: KPipe,
    coarse_virt_fill: KPipe,
    seed_ball: KPipe,
    prolong: KPipe,
    spring_weighted: KPipe,
}

/// One coarse level's persistent buffers (built once, read during layout).
struct MlLevel {
    n_cap: u32,
    m_cap: u32,
    slots_cap: u32,
    nv_cap: u32,
    off: wgpu::Buffer,
    neigh: wgpu::Buffer,
    ewt: wgpu::Buffer,
    pos: wgpu::Buffer,
    virt_csr: wgpu::Buffer,
    virt_eoff: wgpu::Buffer,
    parent: wgpu::Buffer,
    meta: wgpu::Buffer,
}

/// Transient buffers shared across all cascade steps (levels are built and
/// relaxed one at a time, so a single copy of each suffices).
struct MlScratch {
    pos_scratch: wgpu::Buffer,
    vel: wgpu::Buffer,
    energy: wgpu::Buffer,
    spring_partial: wgpu::Buffer,
    matched: wgpu::Buffer,
    propose: wgpu::Buffer,
    wbuf: wgpu::Buffer,
    slot_row: wgpu::Buffer,
    sk_a: wgpu::Buffer,
    sk_b: wgpu::Buffer,
    sv_a: wgpu::Buffer,
    sv_b: wgpu::Buffer,
    histogram: wgpu::Buffer,
    block_sums: wgpu::Buffer,
    edge_min: wgpu::Buffer,
    edge_max: wgpu::Buffer,
    edge_wt: wgpu::Buffer,
    cmass: wgpu::Buffer,
    c_cursor: wgpu::Buffer,
}

/// Immutable references to the fine (level-0) force resources plus the reused
/// `force.wgsl` pipeline and layouts, threaded through the seed.
struct SeedCtx<'a> {
    force_step: &'a wgpu::ComputePipeline,
    force_bgl: &'a wgpu::BindGroupLayout,
    oct_bgl: &'a wgpu::BindGroupLayout,
    spring_bgl: &'a wgpu::BindGroupLayout,
    oct_dummy: &'a wgpu::Buffer,
    fine_pos: &'a wgpu::Buffer,
    fine_off: &'a wgpu::Buffer,
    fine_neigh: &'a wgpu::Buffer,
    options: &'a GpuForceOptions,
}

/// A uniform view over a level (fine or coarse) for the relaxation loop.
struct LevelRef<'a> {
    off: &'a wgpu::Buffer,
    neigh: &'a wgpu::Buffer,
    ewt: &'a wgpu::Buffer,
    pos: &'a wgpu::Buffer,
    virt_csr: &'a wgpu::Buffer,
    virt_eoff: &'a wgpu::Buffer,
    meta: &'a wgpu::Buffer,
    n_cap: u32,
    nv_cap: u32,
}

/// The seed / prolong operation that opens a level's layout encoder.
enum Prelude<'a> {
    SeedBall { radius: f32 },
    Prolong { src_pos: &'a wgpu::Buffer, parent: &'a wgpu::Buffer },
}

pub(crate) struct GpuMultilevel {
    pipes: MlPipes,
    levels: Vec<MlLevel>,
    parent0: wgpu::Buffer,
    meta0: wgpu::Buffer,
    fine_ewt: wgpu::Buffer,
    scratch: MlScratch,
    params_buf: wgpu::Buffer,
    sim_params_buf: wgpu::Buffer,
    n0: u32,
    fine_slots: u32,
}

fn storage(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    })
}

fn storage_cd(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn storage_copy(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

impl GpuMultilevel {
    /// Plan the cascade from the fine counts and allocate every buffer +
    /// pipeline. `fine_slots` is the fine CSR's directed-slot count
    /// (`edge_offsets[n]`); `m0 = fine_slots / 2` bounds coarse edges.
    pub(crate) fn new(device: &wgpu::Device, n0: u32, fine_slots: u32) -> Self {
        let m0 = (fine_slots / 2).max(1);

        // Geometric level schedule: at least one coarse level; stop once the
        // coarse capacity is within the coarsest target, or MAX_LEVELS.
        let mut level_caps: Vec<(u32, u32)> = Vec::new(); // (n_cap, m_cap)
        let mut fn_cap = n0.max(1);
        let mut fm_cap = m0;
        loop {
            if level_caps.len() >= MAX_LEVELS {
                break;
            }
            let ncap = ((fn_cap as f32 * CAP_RATIO).ceil() as u32).max(1);
            let mcap = ((fm_cap as f32 * CAP_RATIO).ceil() as u32).max(1);
            level_caps.push((ncap, mcap));
            fn_cap = ncap;
            fm_cap = mcap;
            if ncap <= COARSEST_TARGET {
                break;
            }
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("multilevel.wgsl"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "shaders/multilevel.wgsl"
            ))),
        });

        let g1_params = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ml_g1_params"),
            entries: &[uniform_entry(0)],
        });
        let g1_scan = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ml_g1_scan"),
            entries: &[uniform_entry(1)],
        });
        let g1_sp = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ml_g1_sp"),
            entries: &[uniform_entry(2)],
        });

        let kbgl = |storages: &[(u32, bool)]| -> wgpu::BindGroupLayout {
            let e: Vec<wgpu::BindGroupLayoutEntry> =
                storages.iter().map(|(b, ro)| storage_entry(*b, *ro)).collect();
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ml_g0"),
                entries: &e,
            })
        };

        let mk = |name: &str, storages: &[(u32, bool)], g1: &wgpu::BindGroupLayout| -> KPipe {
            let g0 = kbgl(storages);
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[&g0, g1],
                push_constant_ranges: &[],
            });
            let consts: HashMap<String, f64> = HashMap::new();
            let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(name),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &consts,
                    zero_initialize_workgroup_memory: true,
                },
                cache: None,
            });
            KPipe { pipe, g0 }
        };

        let init_matched = mk("init_matched", &[(4, false)], &g1_params);
        let fill_ones = mk("fill_ones", &[(11, false)], &g1_params);
        let hem_propose = mk(
            "hem_propose",
            &[(0, true), (1, true), (2, true), (4, false), (5, false), (7, true)],
            &g1_params,
        );
        let hem_match = mk("hem_match", &[(4, false), (5, false), (7, true)], &g1_params);
        let rep_flag = mk("rep_flag", &[(4, false), (7, true), (19, false)], &g1_params);
        let write_nc = mk("write_nc", &[(7, true), (15, false), (19, false)], &g1_params);
        let assign_parent = mk(
            "assign_parent",
            &[(4, false), (6, false), (7, true), (19, false)],
            &g1_params,
        );
        let mass_accum = mk(
            "mass_accum",
            &[(3, true), (6, false), (7, true), (14, false)],
            &g1_params,
        );
        let place_mass = mk("place_mass", &[(12, false), (14, false), (15, false)], &g1_params);
        let row_of_slot = mk("row_of_slot", &[(0, true), (7, true), (8, false)], &g1_params);
        let expand_pairs = mk(
            "expand_pairs",
            &[(1, true), (6, false), (7, true), (8, false), (22, false), (24, false)],
            &g1_params,
        );

        let histogram_g0 = kbgl(&[(22, false), (26, false)]);
        let scatter_g0 = kbgl(&[(22, false), (23, false), (24, false), (25, false), (26, false)]);
        let mut histogram = Vec::with_capacity(8);
        let mut scatter = Vec::with_capacity(8);
        for pass in 0u32..8u32 {
            let shift = pass * 4;
            let mk_radix = |name: &str, g0: &wgpu::BindGroupLayout| {
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(name),
                    bind_group_layouts: &[g0, &g1_params],
                    push_constant_ranges: &[],
                });
                let mut consts: HashMap<String, f64> = HashMap::new();
                consts.insert("PASS_SHIFT".to_string(), shift as f64);
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: Some(&pl),
                    module: &shader,
                    entry_point: Some(name),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &consts,
                        zero_initialize_workgroup_memory: true,
                    },
                    cache: None,
                })
            };
            histogram.push(mk_radix("radix_histogram", &histogram_g0));
            scatter.push(mk_radix("radix_scatter", &scatter_g0));
        }

        let pair_swap = mk("pair_swap", &[(22, false), (24, false)], &g1_params);
        let scan_local = mk("scan_local_u32", &[(20, false), (21, false)], &g1_scan);
        let scan_serial = mk("scan_serial_u32", &[(21, false)], &g1_scan);
        let scan_fixup = mk("scan_fixup_u32", &[(20, false), (21, false)], &g1_scan);
        let edge_flag = mk("edge_flag", &[(19, false), (22, false), (24, false)], &g1_params);
        let write_mc = mk("write_mc", &[(15, false), (19, false)], &g1_params);
        let edge_accumulate = mk(
            "edge_accumulate",
            &[(16, false), (17, false), (18, false), (19, false), (22, false), (24, false)],
            &g1_params,
        );
        let coarse_deg = mk(
            "coarse_deg",
            &[(15, false), (16, false), (17, false), (29, false)],
            &g1_params,
        );
        let write_slots = mk("write_slots", &[(9, true), (15, false)], &g1_params);
        let coarse_fill = mk(
            "coarse_fill",
            &[(10, false), (11, false), (13, false), (15, false), (16, false), (17, false), (18, false)],
            &g1_params,
        );
        let coarse_nv = mk("coarse_nv", &[(9, true), (15, false), (27, false)], &g1_params);
        let coarse_virt_fill = mk(
            "coarse_virt_fill",
            &[(9, true), (15, false), (27, false), (28, false)],
            &g1_params,
        );
        let seed_ball = mk("seed_ball", &[(12, false), (15, false)], &g1_params);
        let prolong = mk("prolong", &[(6, false), (7, true), (12, false), (30, true)], &g1_params);
        let spring_weighted = mk(
            "spring_step_weighted",
            &[(3, true), (10, false), (11, false), (27, false), (28, false), (31, false)],
            &g1_sp,
        );

        let pipes = MlPipes {
            g1_params,
            g1_scan,
            g1_sp,
            init_matched,
            fill_ones,
            hem_propose,
            hem_match,
            rep_flag,
            write_nc,
            assign_parent,
            mass_accum,
            place_mass,
            row_of_slot,
            expand_pairs,
            histogram_g0,
            histogram,
            scatter_g0,
            scatter,
            pair_swap,
            scan_local,
            scan_serial,
            scan_fixup,
            edge_flag,
            write_mc,
            edge_accumulate,
            coarse_deg,
            write_slots,
            coarse_fill,
            coarse_nv,
            coarse_virt_fill,
            seed_ball,
            prolong,
            spring_weighted,
        };

        // Coarse levels.
        let u4 = 4u64;
        let v16 = 16u64;
        let levels: Vec<MlLevel> = level_caps
            .iter()
            .map(|&(n_cap, m_cap)| {
                let slots_cap = (2 * m_cap).max(1);
                let nv_cap = n_cap + slots_cap / HUB + 1;
                MlLevel {
                    n_cap,
                    m_cap,
                    slots_cap,
                    nv_cap,
                    off: storage_copy(device, "ml_off", (n_cap as u64 + 1) * u4),
                    neigh: storage(device, "ml_neigh", slots_cap as u64 * u4),
                    ewt: storage(device, "ml_ewt", slots_cap as u64 * u4),
                    pos: storage_copy(device, "ml_pos", n_cap as u64 * v16),
                    virt_csr: storage_cd(device, "ml_virt_csr", (n_cap as u64 + 1 + nv_cap as u64) * u4),
                    virt_eoff: storage(device, "ml_virt_eoff", (nv_cap as u64 + 1) * u4),
                    parent: storage(device, "ml_parent", n_cap as u64 * u4),
                    meta: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("ml_meta"),
                        size: 16,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: false,
                    }),
                }
            })
            .collect();

        // Shared scratch, sized to the fine (largest) level.
        let nv0 = n0 + fine_slots / HUB + 1;
        let wmax = (n0 as u64 + 1).max(fine_slots as u64 + 1);
        let scratch = MlScratch {
            pos_scratch: storage_copy(device, "ml_pos_scratch", n0.max(1) as u64 * v16),
            vel: storage_cd(device, "ml_vel", n0.max(1) as u64 * v16),
            energy: storage(device, "ml_energy", (n0.max(1) as u64 * u4).max(64)),
            spring_partial: storage(device, "ml_spring_partial", nv0 as u64 * v16),
            matched: storage(device, "ml_matched", n0.max(1) as u64 * u4),
            propose: storage(device, "ml_propose", n0.max(1) as u64 * u4),
            wbuf: storage_cd(device, "ml_wbuf", wmax * u4),
            slot_row: storage(device, "ml_slot_row", fine_slots.max(1) as u64 * u4),
            sk_a: storage(device, "ml_sk_a", fine_slots.max(1) as u64 * u4),
            sk_b: storage(device, "ml_sk_b", fine_slots.max(1) as u64 * u4),
            sv_a: storage(device, "ml_sv_a", fine_slots.max(1) as u64 * u4),
            sv_b: storage(device, "ml_sv_b", fine_slots.max(1) as u64 * u4),
            histogram: storage(device, "ml_histogram", (16 * fine_slots.max(1).div_ceil(WG)) as u64 * u4),
            block_sums: storage(device, "ml_block_sums", (wmax.div_ceil(WG as u64) + 1) * u4),
            edge_min: storage(device, "ml_edge_min", m0 as u64 * u4),
            edge_max: storage(device, "ml_edge_max", m0 as u64 * u4),
            edge_wt: storage_cd(device, "ml_edge_wt", m0 as u64 * u4),
            cmass: storage_cd(device, "ml_cmass", n0.max(1) as u64 * u4),
            c_cursor: storage_cd(device, "ml_c_cursor", (n0 as u64 + 1) * u4),
        };

        let parent0 = storage(device, "ml_parent0", n0.max(1) as u64 * u4);
        let meta0 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ml_meta0"),
            contents: bytemuck::cast_slice(&[n0, m0, fine_slots, 0u32]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let fine_ewt = storage(device, "ml_fine_ewt", fine_slots.max(1) as u64 * u4);

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ml_params"),
            size: std::mem::size_of::<MlParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sim_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ml_sim_params"),
            size: ml_coarse_params_bytes(&GpuForceOptions::default(), 0, 0, 0).len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipes,
            levels,
            parent0,
            meta0,
            fine_ewt,
            scratch,
            params_buf,
            sim_params_buf,
            n0,
            fine_slots,
        }
    }

    fn params_bg(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        kbg(device, &self.pipes.g1_params, &[(0, &self.params_buf)])
    }

    fn sp_bg(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        kbg(device, &self.pipes.g1_sp, &[(2, &self.sim_params_buf)])
    }

    fn write_params(&self, queue: &wgpu::Queue, p: &MlParamsRaw) {
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(p));
    }

    fn base_params(
        &self,
        n_fine_cap: u32,
        slots_fine_cap: u32,
        nc_cap: u32,
        mc_cap: u32,
        level: u32,
        spring_len: f32,
        radius: f32,
    ) -> MlParamsRaw {
        MlParamsRaw {
            n_fine_cap,
            slots_fine_cap,
            nc_cap,
            mc_cap,
            round: 0,
            seed: 0x9E37_79B1 ^ level.wrapping_mul(0x1000_0001),
            level,
            _pad0: 0,
            spring_len,
            radius,
            mass_scale: MASS_SCALE,
            _pad1: 0.0,
            sort_nblocks: slots_fine_cap.div_ceil(WG).max(1),
            _pad2: 0,
            _pad3: 0,
            _pad4: 0,
        }
    }

    /// Record an exclusive-scan of `data[0..len)` (three passes). `keep`
    /// retains the freshly-created ScanDims uniform until the submit.
    fn scan(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        data: &wgpu::Buffer,
        len: u32,
        keep: &mut Vec<wgpu::Buffer>,
    ) {
        let nblocks = len.div_ceil(WG).max(1);
        let dims = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ml_scan_dims"),
            contents: bytemuck::bytes_of(&MlScanDims { len, nblocks, _p0: 0, _p1: 0 }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let g1 = kbg(device, &self.pipes.g1_scan, &[(1, &dims)]);
        let bg_lf = kbg(device, &self.pipes.scan_local.g0, &[(20, data), (21, &self.scratch.block_sums)]);
        let bg_serial = kbg(device, &self.pipes.scan_serial.g0, &[(21, &self.scratch.block_sums)]);
        pass1(enc, &self.pipes.scan_local.pipe, &bg_lf, &g1, dispatch_grid(len));
        pass1(enc, &self.pipes.scan_serial.pipe, &bg_serial, &g1, (1, 1));
        pass1(enc, &self.pipes.scan_fixup.pipe, &bg_lf, &g1, dispatch_grid(len));
        keep.push(dims);
    }

    /// Record an 8-pass stable LSD radix sort over `slots` elements, keyed on
    /// the current `sk` field, carrying the `sv` payload. Ends in the a-side.
    fn radix_sort(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        slots_fine_cap: u32,
        sort_nblocks: u32,
        params_bg: &wgpu::BindGroup,
        keep: &mut Vec<wgpu::Buffer>,
    ) {
        let grid = dispatch_grid(slots_fine_cap);
        let hist_len = 16 * sort_nblocks;
        for pass in 0usize..8usize {
            let even = pass % 2 == 0;
            let (src_k, dst_k, src_v, dst_v) = if even {
                (&self.scratch.sk_a, &self.scratch.sk_b, &self.scratch.sv_a, &self.scratch.sv_b)
            } else {
                (&self.scratch.sk_b, &self.scratch.sk_a, &self.scratch.sv_b, &self.scratch.sv_a)
            };
            let bg_hist = kbg(device, &self.pipes.histogram_g0, &[(22, src_k), (26, &self.scratch.histogram)]);
            pass1(enc, &self.pipes.histogram[pass], &bg_hist, params_bg, grid);
            self.scan(device, enc, &self.scratch.histogram, hist_len, keep);
            let bg_scatter = kbg(
                device,
                &self.pipes.scatter_g0,
                &[(22, src_k), (23, dst_k), (24, src_v), (25, dst_v), (26, &self.scratch.histogram)],
            );
            pass1(enc, &self.pipes.scatter[pass], &bg_scatter, params_bg, grid);
        }
    }
}

impl GpuMultilevel {
    /// Build the full cascade and lay it out, leaving the finest result in the
    /// fine positions buffer supplied through the arguments.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn seed(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        force_step: &wgpu::ComputePipeline,
        force_bgl: &wgpu::BindGroupLayout,
        oct_bgl: &wgpu::BindGroupLayout,
        spring_bgl: &wgpu::BindGroupLayout,
        oct_dummy: &wgpu::Buffer,
        fine_pos: &wgpu::Buffer,
        fine_off: &wgpu::Buffer,
        fine_neigh: &wgpu::Buffer,
        fine_virt_csr: &wgpu::Buffer,
        fine_virt_eoff: &wgpu::Buffer,
        fine_n_virtual: u32,
        options: &GpuForceOptions,
    ) {
        let ctx = SeedCtx {
            force_step,
            force_bgl,
            oct_bgl,
            spring_bgl,
            oct_dummy,
            fine_pos,
            fine_off,
            fine_neigh,
            options,
        };
        let spring_len = options.spring_len.max(1.0);

        if self.levels.is_empty() {
            return;
        }

        // Fill the fine level's unit edge weights (bound to the c_ewt slot).
        {
            let p = self.base_params(self.n0, self.fine_slots, 0, 0, 0, spring_len, 0.0);
            self.write_params(queue, &p);
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ml_fill_ewt") });
            let pbg = self.params_bg(device);
            let bg = kbg(device, &self.pipes.fill_ones.g0, &[(11, &self.fine_ewt)]);
            pass1(&mut enc, &self.pipes.fill_ones.pipe, &bg, &pbg, dispatch_grid(self.fine_slots));
            queue.submit(Some(enc.finish()));
        }

        // ---- Cascade build (coarse -> coarser) --------------------------------
        for c in 0..self.levels.len() {
            self.build_level(device, queue, &ctx, c, spring_len);
        }

        // ---- Layout: seed coarsest, prolong + relax down ----------------------
        let l = self.levels.len();
        let mut step_index: u32 = 0x51ED_u32;

        // Coarsest level.
        {
            let coarse = &self.levels[l - 1];
            let radius = (coarse.n_cap as f32).max(1.0).sqrt() * spring_len;
            let lv = LevelRef {
                off: &coarse.off,
                neigh: &coarse.neigh,
                ewt: &coarse.ewt,
                pos: &coarse.pos,
                virt_csr: &coarse.virt_csr,
                virt_eoff: &coarse.virt_eoff,
                meta: &coarse.meta,
                n_cap: coarse.n_cap,
                nv_cap: coarse.nv_cap,
            };
            self.layout_level(device, queue, &ctx, &lv, Prelude::SeedBall { radius }, 200, step_index, spring_len);
            step_index = step_index.wrapping_add(1);
        }

        // Intermediate coarse levels (index from fine = c + 1).
        for c in (0..l.saturating_sub(1)).rev() {
            let steps = (120u32 >> (c as u32 + 1)).max(20);
            let finer = &self.levels[c];
            let src_pos = &self.levels[c + 1].pos;
            let lv = LevelRef {
                off: &finer.off,
                neigh: &finer.neigh,
                ewt: &finer.ewt,
                pos: &finer.pos,
                virt_csr: &finer.virt_csr,
                virt_eoff: &finer.virt_eoff,
                meta: &finer.meta,
                n_cap: finer.n_cap,
                nv_cap: finer.nv_cap,
            };
            self.layout_level(
                device,
                queue,
                &ctx,
                &lv,
                Prelude::Prolong { src_pos, parent: &finer.parent },
                steps,
                step_index,
                spring_len,
            );
            step_index = step_index.wrapping_add(1);
        }

        // Fine level: prolong from level 1, then relax at full resolution.
        {
            let src_pos = &self.levels[0].pos;
            let lv = LevelRef {
                off: fine_off,
                neigh: fine_neigh,
                ewt: &self.fine_ewt,
                pos: fine_pos,
                virt_csr: fine_virt_csr,
                virt_eoff: fine_virt_eoff,
                meta: &self.meta0,
                n_cap: self.n0,
                nv_cap: fine_n_virtual,
            };
            self.layout_level(
                device,
                queue,
                &ctx,
                &lv,
                Prelude::Prolong { src_pos, parent: &self.parent0 },
                120,
                step_index,
                spring_len,
            );
        }
    }

    /// Build coarse level `c` from its finer neighbour (fine when `c == 0`).
    fn build_level(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ctx: &SeedCtx<'_>,
        c: usize,
        spring_len: f32,
    ) {
        // Finer-level resource references.
        let (f_off, f_neigh, f_pos, f_ewt, f_meta, f_parent, n_fine_cap, slots_fine_cap) = if c == 0 {
            (
                ctx.fine_off,
                ctx.fine_neigh,
                ctx.fine_pos,
                &self.fine_ewt,
                &self.meta0,
                &self.parent0,
                self.n0,
                self.fine_slots,
            )
        } else {
            let f = &self.levels[c - 1];
            (&f.off, &f.neigh, &f.pos, &f.ewt, &f.meta, &f.parent, f.n_cap, f.slots_cap)
        };
        let coarse = &self.levels[c];
        let mut params = self.base_params(
            n_fine_cap,
            slots_fine_cap,
            coarse.n_cap,
            coarse.m_cap,
            c as u32 + 1,
            spring_len,
            0.0,
        );

        // Matching: three rounds, one submit each so the round-dependent
        // tie-break salt in the uniform is stable within its submit.
        for round in 0u32..3u32 {
            params.round = round;
            self.write_params(queue, &params);
            let pbg = self.params_bg(device);
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ml_match") });
            if round == 0 {
                let bg = kbg(device, &self.pipes.init_matched.g0, &[(4, &self.scratch.matched)]);
                pass1(&mut enc, &self.pipes.init_matched.pipe, &bg, &pbg, dispatch_grid(n_fine_cap));
            }
            let bg_prop = kbg(
                device,
                &self.pipes.hem_propose.g0,
                &[(0, f_off), (1, f_neigh), (2, f_ewt), (4, &self.scratch.matched), (5, &self.scratch.propose), (7, f_meta)],
            );
            pass1(&mut enc, &self.pipes.hem_propose.pipe, &bg_prop, &pbg, dispatch_grid(n_fine_cap));
            let bg_match = kbg(
                device,
                &self.pipes.hem_match.g0,
                &[(4, &self.scratch.matched), (5, &self.scratch.propose), (7, f_meta)],
            );
            pass1(&mut enc, &self.pipes.hem_match.pipe, &bg_match, &pbg, dispatch_grid(n_fine_cap));
            queue.submit(Some(enc.finish()));
        }

        // Build submit: parent map, coarse mass, coarse CSR, Tigr virtual CSR.
        params.round = 0;
        self.write_params(queue, &params);
        let pbg = self.params_bg(device);
        let mut keep: Vec<wgpu::Buffer> = Vec::new();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ml_build") });

        // Representatives -> exclusive scan -> parent + coarse node count.
        enc.clear_buffer(&self.scratch.wbuf, 0, Some((n_fine_cap as u64 + 1) * 4));
        let bg_rep = kbg(
            device,
            &self.pipes.rep_flag.g0,
            &[(4, &self.scratch.matched), (7, f_meta), (19, &self.scratch.wbuf)],
        );
        pass1(&mut enc, &self.pipes.rep_flag.pipe, &bg_rep, &pbg, dispatch_grid(n_fine_cap));
        self.scan(device, &mut enc, &self.scratch.wbuf, n_fine_cap + 1, &mut keep);
        let bg_wnc = kbg(
            device,
            &self.pipes.write_nc.g0,
            &[(7, f_meta), (15, &coarse.meta), (19, &self.scratch.wbuf)],
        );
        pass1(&mut enc, &self.pipes.write_nc.pipe, &bg_wnc, &pbg, (1, 1));
        let bg_ap = kbg(
            device,
            &self.pipes.assign_parent.g0,
            &[(4, &self.scratch.matched), (6, f_parent), (7, f_meta), (19, &self.scratch.wbuf)],
        );
        pass1(&mut enc, &self.pipes.assign_parent.pipe, &bg_ap, &pbg, dispatch_grid(n_fine_cap));

        // Coarse mass (fixed-point atomic accumulate) -> place into .w.
        enc.clear_buffer(&self.scratch.cmass, 0, Some(coarse.n_cap as u64 * 4));
        let bg_mass = kbg(
            device,
            &self.pipes.mass_accum.g0,
            &[(3, f_pos), (6, f_parent), (7, f_meta), (14, &self.scratch.cmass)],
        );
        pass1(&mut enc, &self.pipes.mass_accum.pipe, &bg_mass, &pbg, dispatch_grid(n_fine_cap));
        let bg_place = kbg(
            device,
            &self.pipes.place_mass.g0,
            &[(12, &coarse.pos), (14, &self.scratch.cmass), (15, &coarse.meta)],
        );
        pass1(&mut enc, &self.pipes.place_mass.pipe, &bg_place, &pbg, dispatch_grid(coarse.n_cap));

        // Coarse edges: expand -> two-key sort -> dedup -> CSR.
        let bg_row = kbg(
            device,
            &self.pipes.row_of_slot.g0,
            &[(0, f_off), (7, f_meta), (8, &self.scratch.slot_row)],
        );
        pass1(&mut enc, &self.pipes.row_of_slot.pipe, &bg_row, &pbg, dispatch_grid(n_fine_cap));
        let bg_exp = kbg(
            device,
            &self.pipes.expand_pairs.g0,
            &[(1, f_neigh), (6, f_parent), (7, f_meta), (8, &self.scratch.slot_row), (22, &self.scratch.sk_a), (24, &self.scratch.sv_a)],
        );
        pass1(&mut enc, &self.pipes.expand_pairs.pipe, &bg_exp, &pbg, dispatch_grid(slots_fine_cap));

        let sort_nblocks = params.sort_nblocks;
        // Sort by max (sk), then swap key/payload and sort by min.
        self.radix_sort(device, &mut enc, slots_fine_cap, sort_nblocks, &pbg, &mut keep);
        let bg_swap = kbg(device, &self.pipes.pair_swap.g0, &[(22, &self.scratch.sk_a), (24, &self.scratch.sv_a)]);
        pass1(&mut enc, &self.pipes.pair_swap.pipe, &bg_swap, &pbg, dispatch_grid(slots_fine_cap));
        self.radix_sort(device, &mut enc, slots_fine_cap, sort_nblocks, &pbg, &mut keep);

        // Unique flag -> scan -> per-slot edge id.
        enc.clear_buffer(&self.scratch.wbuf, 0, Some((slots_fine_cap as u64 + 1) * 4));
        let bg_ef = kbg(
            device,
            &self.pipes.edge_flag.g0,
            &[(19, &self.scratch.wbuf), (22, &self.scratch.sk_a), (24, &self.scratch.sv_a)],
        );
        pass1(&mut enc, &self.pipes.edge_flag.pipe, &bg_ef, &pbg, dispatch_grid(slots_fine_cap));
        self.scan(device, &mut enc, &self.scratch.wbuf, slots_fine_cap + 1, &mut keep);
        let bg_wmc = kbg(device, &self.pipes.write_mc.g0, &[(15, &coarse.meta), (19, &self.scratch.wbuf)]);
        pass1(&mut enc, &self.pipes.write_mc.pipe, &bg_wmc, &pbg, (1, 1));

        enc.clear_buffer(&self.scratch.edge_wt, 0, Some(coarse.m_cap as u64 * 4));
        let bg_acc = kbg(
            device,
            &self.pipes.edge_accumulate.g0,
            &[(16, &self.scratch.edge_min), (17, &self.scratch.edge_max), (18, &self.scratch.edge_wt), (19, &self.scratch.wbuf), (22, &self.scratch.sk_a), (24, &self.scratch.sv_a)],
        );
        pass1(&mut enc, &self.pipes.edge_accumulate.pipe, &bg_acc, &pbg, dispatch_grid(slots_fine_cap));

        // Coarse degrees -> offsets -> directed-slot count -> fill both dirs.
        enc.clear_buffer(&coarse.off, 0, Some((coarse.n_cap as u64 + 1) * 4));
        let bg_deg = kbg(
            device,
            &self.pipes.coarse_deg.g0,
            &[(15, &coarse.meta), (16, &self.scratch.edge_min), (17, &self.scratch.edge_max), (29, &coarse.off)],
        );
        pass1(&mut enc, &self.pipes.coarse_deg.pipe, &bg_deg, &pbg, dispatch_grid(coarse.m_cap));
        self.scan(device, &mut enc, &coarse.off, coarse.n_cap + 1, &mut keep);
        let bg_ws = kbg(device, &self.pipes.write_slots.g0, &[(9, &coarse.off), (15, &coarse.meta)]);
        pass1(&mut enc, &self.pipes.write_slots.pipe, &bg_ws, &pbg, (1, 1));
        enc.copy_buffer_to_buffer(&coarse.off, 0, &self.scratch.c_cursor, 0, (coarse.n_cap as u64 + 1) * 4);
        let bg_fill = kbg(
            device,
            &self.pipes.coarse_fill.g0,
            &[(10, &coarse.neigh), (11, &coarse.ewt), (13, &self.scratch.c_cursor), (15, &coarse.meta), (16, &self.scratch.edge_min), (17, &self.scratch.edge_max), (18, &self.scratch.edge_wt)],
        );
        pass1(&mut enc, &self.pipes.coarse_fill.pipe, &bg_fill, &pbg, dispatch_grid(coarse.m_cap));

        // Coarse Tigr virtual CSR.
        enc.clear_buffer(&coarse.virt_csr, 0, Some((coarse.n_cap as u64 + 1) * 4));
        let bg_nv = kbg(
            device,
            &self.pipes.coarse_nv.g0,
            &[(9, &coarse.off), (15, &coarse.meta), (27, &coarse.virt_csr)],
        );
        pass1(&mut enc, &self.pipes.coarse_nv.pipe, &bg_nv, &pbg, dispatch_grid(coarse.n_cap));
        self.scan(device, &mut enc, &coarse.virt_csr, coarse.n_cap + 1, &mut keep);
        let bg_vf = kbg(
            device,
            &self.pipes.coarse_virt_fill.g0,
            &[(9, &coarse.off), (15, &coarse.meta), (27, &coarse.virt_csr), (28, &coarse.virt_eoff)],
        );
        pass1(&mut enc, &self.pipes.coarse_virt_fill.pipe, &bg_vf, &pbg, dispatch_grid(coarse.n_cap));

        queue.submit(Some(enc.finish()));
        drop(keep);
    }

    /// Seed or prolong a level, then relax it. One encoder, one submit.
    fn layout_level(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ctx: &SeedCtx<'_>,
        lv: &LevelRef<'_>,
        prelude: Prelude<'_>,
        steps: u32,
        step_index: u32,
        spring_len: f32,
    ) {
        let is_fine = std::ptr::eq(lv.pos as *const _, ctx.fine_pos as *const _);
        let (n_nodes, n_edges) = if is_fine {
            (self.n0, self.fine_slots / 2)
        } else {
            (lv.n_cap, lv.n_cap)
        };
        let sp = ml_coarse_params_bytes(ctx.options, n_nodes, n_edges, step_index);
        queue.write_buffer(&self.sim_params_buf, 0, &sp);

        let radius = match &prelude {
            Prelude::SeedBall { radius } => *radius,
            Prelude::Prolong { .. } => 0.0,
        };
        let p = self.base_params(lv.n_cap, 0, lv.n_cap, 0, 0, spring_len, radius);
        self.write_params(queue, &p);
        let pbg = self.params_bg(device);
        let spbg = self.sp_bg(device);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ml_layout") });

        // Splice the device coarse counts into the reused force SimParams.
        if !is_fine {
            enc.copy_buffer_to_buffer(lv.meta, 0, &self.sim_params_buf, SIM_N_NODES_OFFSET, 4);
            enc.copy_buffer_to_buffer(lv.meta, 4, &self.sim_params_buf, SIM_N_EDGES_OFFSET, 4);
        }

        // Reset velocities for this level.
        enc.clear_buffer(&self.scratch.vel, 0, Some(lv.n_cap as u64 * 16));

        match &prelude {
            Prelude::SeedBall { .. } => {
                let bg = kbg(device, &self.pipes.seed_ball.g0, &[(12, lv.pos), (15, lv.meta)]);
                pass1(&mut enc, &self.pipes.seed_ball.pipe, &bg, &pbg, dispatch_grid(lv.n_cap));
            }
            Prelude::Prolong { src_pos, parent } => {
                let bg = kbg(
                    device,
                    &self.pipes.prolong.g0,
                    &[(6, parent), (7, lv.meta), (12, lv.pos), (30, src_pos)],
                );
                pass1(&mut enc, &self.pipes.prolong.pipe, &bg, &pbg, dispatch_grid(lv.n_cap));
            }
        }

        // Relax: ping-pong positions between the level buffer and scratch.
        let mut in_pos = true;
        for _ in 0..steps.max(1) {
            let (pin, pout) = if in_pos {
                (lv.pos, &self.scratch.pos_scratch)
            } else {
                (&self.scratch.pos_scratch, lv.pos)
            };
            // Weighted spring pass (multilevel.wgsl) -> per-virtual partials.
            let bg_sp = kbg(
                device,
                &self.pipes.spring_weighted.g0,
                &[(3, pin), (10, lv.neigh), (11, lv.ewt), (27, lv.virt_csr), (28, lv.virt_eoff), (31, &self.scratch.spring_partial)],
            );
            pass1(&mut enc, &self.pipes.spring_weighted.pipe, &bg_sp, &spbg, dispatch_grid(lv.nv_cap));

            // Repulsion + integration: reuse force.wgsl force_step unchanged.
            let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ml_force_g0"),
                layout: ctx.force_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: pin.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: pout.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.scratch.vel.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: lv.off.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: lv.neigh.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: self.sim_params_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: self.scratch.energy.as_entire_binding() },
                ],
            });
            let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ml_force_g1"),
                layout: ctx.oct_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 1, resource: ctx.oct_dummy.as_entire_binding() }],
            });
            let bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ml_force_g2"),
                layout: ctx.spring_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: lv.virt_csr.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: lv.virt_eoff.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.scratch.spring_partial.as_entire_binding() },
                ],
            });
            pass3(&mut enc, ctx.force_step, &bg0, &bg1, &bg2, dispatch_grid(lv.n_cap));
            in_pos = !in_pos;
        }
        // Ensure the latest result lives in the level's own position buffer.
        if !in_pos {
            enc.copy_buffer_to_buffer(&self.scratch.pos_scratch, 0, lv.pos, 0, lv.n_cap as u64 * 16);
        }
        queue.submit(Some(enc.finish()));
    }

    /// TEST-ONLY: read back each level's node count (fine + every coarse
    /// level) by mapping the small meta buffers. Not used by the seed path.
    #[cfg(test)]
    pub(crate) async fn level_node_counts(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Vec<u32> {
        let mut out = vec![self.n0];
        for lvl in &self.levels {
            out.push(read_meta_nc(device, queue, &lvl.meta).await);
        }
        out
    }
}

#[cfg(test)]
async fn read_meta_nc(device: &wgpu::Device, queue: &wgpu::Queue, meta: &wgpu::Buffer) -> u32 {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ml_meta_staging"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ml_meta_read") });
    enc.copy_buffer_to_buffer(meta, 0, &staging, 0, 16);
    queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    let _ = rx.recv();
    let data = slice.get_mapped_range();
    let vals: &[u32] = bytemuck::cast_slice(&data);
    let nc = vals[0];
    drop(data);
    staging.unmap();
    nc
}
