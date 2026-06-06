//! GPU dynamic-edge (self-assembly) bonding stage — P4.
//!
//! Atomics-free, sort-based, deterministic port of the CPU `update_dynamic_bonds`
//! pipeline (`super::geometric`). WebGPU has **no f32 atomics**, and we avoid u32
//! atomics too so the bond set is bit-reproducible run-to-run — the property the
//! solved-case canary methodology relies on. The work splits exactly along the
//! parallel/serial seam the design (`docs/dynamic-edge-bonding-plan.md` §1/§2/§4)
//! calls out:
//!
//!   - **GPU, parallel:** the O(n) cell hash (`calc_hash`) and the O(n·27)
//!     candidate scan over the 3×3×3 neighbour-cell stencil (`scan_candidates`).
//!     This is the heavy geometry pass — every candidate-pair distance + class
//!     test happens on the device.
//!   - **Host, serial + deterministic:** the counting sort that turns the hashes
//!     into `sorted_nodes` + `cell_start`/`cell_end` (the design's
//!     `radix/counting sort → findCellStart`, kept host-side so it is exactly
//!     reproducible and atomics-free), the hysteretic break of over-stretched
//!     existing bonds, and the **conflict-free valence-cap accept/reject**. The
//!     accept/reject is an inherently sequential dependency (a bond accepted
//!     earlier spends valence later candidates must see), so it is one
//!     deterministic pass over the sorted candidate keys — identical, key for key,
//!     to the CPU algorithm. No atomics anywhere.
//!
//! Because the candidate generation runs on the GPU and the bond decisions follow
//! the exact CPU rule, `gpu_dynamic_bonds` returns the **same canonical bond set**
//! the CPU `update_dynamic_bonds` does on the same frozen configuration — that is
//! the CPU↔GPU equivalence gate (`tests/geometric_solver.rs`).
//!
//! A companion [`gpu_relax_bonds`] runs the dynamic-edge spring + integration on
//! the GPU (the `spring_step` kernel) so the gate can also assert the two backends
//! relax a seeded bond config to the same coordination histogram / distances.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::geometric::GeometricSettings;
use super::GpuCtx;

const WORKGROUP_SIZE: u32 = 64;
const EMPTY: u32 = u32::MAX;

/// Per-node candidate slate capacity. Soft Langevin particles aren't hard-sphere
/// packed, so the 27-cell stencil at cell = r_break can hold more than the ~12
/// contacts a close-packed lattice has; 64 is generous headroom (design open
/// question (a)). A node that overflows simply drops the surplus candidates —
/// matched on the CPU side of the gate by capping there too, so the two agree.
const MAX_CANDIDATES: u32 = 64;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct BondParamsRaw {
    n_nodes: u32,
    grid_min_x: f32,
    grid_min_y: f32,
    grid_min_z: f32,
    inv_cell: f32,
    grid_dim_x: u32,
    grid_dim_y: u32,
    grid_dim_z: u32,
    r_bond2: f32,
    class_affinity_dim: u32,
    max_candidates: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct SpringParamsRaw {
    n_nodes: u32,
    bond_stiffness: f32,
    rest_len: f32,
    time_step: f32,
    damping: f32,
    max_step: f32,
    _p0: u32,
    _p1: u32,
}

/// Effective break cutoff with the same hysteresis fallback as the CPU engine
/// (`super::geometric::effective_r_break`): a valid `r_break` is finite and `>
/// r_bond`; otherwise `1.3·r_bond`.
fn effective_r_break(s: &GeometricSettings) -> f32 {
    if s.r_break.is_finite() && s.r_break > s.r_bond {
        s.r_break
    } else {
        1.3 * s.r_bond
    }
}

fn lookup_max_valence(s: &GeometricSettings, class: usize) -> u32 {
    let v = s
        .max_valence
        .get(class)
        .copied()
        .unwrap_or(s.default_max_valence);
    if v == 0 {
        s.default_max_valence
    } else {
        v
    }
}

/// Compute the GPU dynamic-bond set for a frozen configuration, applied on top of
/// an `existing` bond set (canonical `(a,b)`, `a<b`) — exactly as the CPU
/// `update_dynamic_bonds` does: break over-stretched existing bonds (hysteresis),
/// then create new in-range/compatible/under-cap pairs, conflict-free.
///
/// `positions` is interleaved x,y,z (length `3n`). Returns the new canonical bond
/// set, sorted by `(a,b)`. Deterministic and atomics-free.
pub fn gpu_dynamic_bonds(
    gpu: &GpuCtx,
    settings: &GeometricSettings,
    positions: &[f32],
    classes: &[u32],
    existing: &[(u32, u32)],
) -> Vec<(u32, u32)> {
    let n = classes.len();
    let r_bond = settings.r_bond.max(0.0);
    let r_break = effective_r_break(settings);
    let r_bond2 = r_bond * r_bond;
    let r_break2 = r_break * r_break;

    // --- 1. break over-stretched existing bonds (hysteresis), host-side. ----
    let mut bonds: Vec<(u32, u32)> = existing
        .iter()
        .copied()
        .filter(|&(a, b)| {
            let (a, b) = (a as usize, b as usize);
            let dx = positions[3 * b] - positions[3 * a];
            let dy = positions[3 * b + 1] - positions[3 * a + 1];
            let dz = positions[3 * b + 2] - positions[3 * a + 2];
            dx * dx + dy * dy + dz * dz <= r_break2
        })
        .collect();
    let mut bonded: std::collections::HashSet<(u32, u32)> = bonds.iter().copied().collect();

    if n == 0 {
        return bonds;
    }

    // --- GPU candidate generation (calc_hash + scan_candidates) ------------
    let candidates = gpu_candidate_pairs(gpu, settings, positions, classes, r_break, r_bond2);

    // --- conflict-free valence-cap accept/reject (serial, host) ------------
    // Candidates already come back sorted by (a,b) and deduped; iterate in that
    // deterministic order seeding valence from the surviving bonds.
    let capped = settings.default_max_valence != 0 || !settings.max_valence.is_empty();
    let mut valence: Vec<u32> = vec![0; n];
    if capped {
        for &(a, b) in &bonds {
            valence[a as usize] += 1;
            valence[b as usize] += 1;
        }
    }
    for (a, b) in candidates {
        let key = (a, b);
        if bonded.contains(&key) {
            continue;
        }
        let (ai, bi) = (a as usize, b as usize);
        if capped {
            let cap_a = lookup_max_valence(settings, classes[ai] as usize);
            let cap_b = lookup_max_valence(settings, classes[bi] as usize);
            if valence[ai] >= cap_a || valence[bi] >= cap_b {
                continue;
            }
        }
        if bonded.insert(key) {
            if capped {
                valence[ai] += 1;
                valence[bi] += 1;
            }
            bonds.push(key);
        }
    }

    bonds.sort_unstable();
    bonds
}

/// Run the GPU candidate-generation kernels and return the deduped, sorted set of
/// in-range, class-compatible candidate pairs `(a,b)` with `a<b`. This is the
/// device half of [`gpu_dynamic_bonds`]; split out so a test can target the
/// grid+scan equivalence directly.
pub fn gpu_candidate_pairs(
    gpu: &GpuCtx,
    settings: &GeometricSettings,
    positions: &[f32],
    classes: &[u32],
    r_break: f32,
    r_bond2: f32,
) -> Vec<(u32, u32)> {
    let n = classes.len();
    if n == 0 {
        return Vec::new();
    }
    let device = &gpu.device;
    let queue = &gpu.queue;

    // Grid bounds: a cubic grid sized to span the cloud, cell = r_break. We pad
    // the max so a node exactly on the upper edge still lands in-grid, then clamp
    // dims to ≥1. The candidate scan skips out-of-grid neighbour cells (no wrap).
    let cell = if r_break.is_finite() && r_break > 1e-4 {
        r_break
    } else {
        1e-4
    };
    let inv_cell = 1.0 / cell;
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for d in 0..3 {
            let p = positions[3 * i + d];
            if p.is_finite() {
                if p < min[d] {
                    min[d] = p;
                }
                if p > max[d] {
                    max[d] = p;
                }
            }
        }
    }
    for d in 0..3 {
        if !min[d].is_finite() {
            min[d] = 0.0;
        }
        if !max[d].is_finite() {
            max[d] = min[d];
        }
    }
    let dim = |d: usize| -> u32 {
        let span = (max[d] - min[d]).max(0.0);
        ((span * inv_cell).floor() as i64 + 1).max(1) as u32
    };
    let (gx, gy, gz) = (dim(0), dim(1), dim(2));
    let n_cells = (gx as u64 * gy as u64 * gz as u64).max(1) as usize;

    // --- positions (vec4) + classes buffers ---
    let mut pos4 = vec![0.0f32; n * 4];
    for i in 0..n {
        pos4[4 * i] = positions[3 * i];
        pos4[4 * i + 1] = positions[3 * i + 1];
        pos4[4 * i + 2] = positions[3 * i + 2];
    }
    let positions_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_positions"),
        contents: bytemuck::cast_slice(&pos4),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let class_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_class"),
        contents: bytemuck::cast_slice(classes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let affinity_src: &[f32] = if settings.class_affinity.is_empty() {
        &[0.0]
    } else {
        &settings.class_affinity
    };
    let affinity_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_affinity"),
        contents: bytemuck::cast_slice(affinity_src),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let params = BondParamsRaw {
        n_nodes: n as u32,
        grid_min_x: min[0],
        grid_min_y: min[1],
        grid_min_z: min[2],
        inv_cell,
        grid_dim_x: gx,
        grid_dim_y: gy,
        grid_dim_z: gz,
        r_bond2,
        class_affinity_dim: settings.class_affinity_dim,
        max_candidates: MAX_CANDIDATES,
        _pad: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let cell_hash_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bond_cell_hash"),
        size: (n * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let hash_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bond_hash_readback"),
        size: (n * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bond_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "../shaders/geometric_bonding.wgsl"
        ))),
    });

    // --- Phase A: calc_hash (only needs positions + params + cell_hash) -----
    // We build the bind group with placeholder cell-start/end/sorted buffers (size
    // 1) for calc_hash; scan_candidates gets the real ones after the host sort.
    let tmp_u32 = |label: &str, len: usize| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&vec![0u32; len.max(1)]),
            usage: wgpu::BufferUsages::STORAGE,
        })
    };

    // Run calc_hash.
    {
        let sorted_tmp = tmp_u32("sorted_tmp", n);
        let cstart_tmp = tmp_u32("cstart_tmp", n_cells);
        let cend_tmp = tmp_u32("cend_tmp", n_cells);
        let cand_tmp = tmp_u32("cand_tmp", n * MAX_CANDIDATES as usize);
        let candc_tmp = tmp_u32("candc_tmp", n);
        let (bgl, bg) = bonding_bind_group(
            device,
            &positions_buf,
            &class_buf,
            &params_buf,
            &cell_hash_buf,
            &sorted_tmp,
            &cstart_tmp,
            &cend_tmp,
            &affinity_buf,
            &cand_tmp,
            &candc_tmp,
        );
        let pipeline = bonding_pipeline(device, &shader, &bgl, "calc_hash");
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            let wg = (n as u32 + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            pass.dispatch_workgroups(wg.max(1), 1, 1);
        }
        enc.copy_buffer_to_buffer(&cell_hash_buf, 0, &hash_readback, 0, (n * 4) as u64);
        queue.submit(std::iter::once(enc.finish()));
    }

    // Read hashes back.
    let hashes: Vec<u32> = map_read_u32(device, &hash_readback, n);

    // --- Phase B (host): counting sort by cell + cell_start/cell_end --------
    // Stable counting sort: ascending node id within each cell (ties broken by the
    // ascending input order), so the candidate scan visits partners in ascending
    // id — matching the CPU cell list's per-bucket insertion order.
    let mut counts = vec![0u32; n_cells + 1];
    for &h in &hashes {
        counts[h as usize + 1] += 1;
    }
    for c in 0..n_cells {
        counts[c + 1] += counts[c];
    }
    let cell_start: Vec<u32> = counts[..n_cells].to_vec();
    let cell_end: Vec<u32> = counts[1..].to_vec();
    let mut sorted_nodes = vec![0u32; n];
    let mut cursor = cell_start.clone();
    for i in 0..n {
        let c = hashes[i] as usize;
        let slot = cursor[c] as usize;
        sorted_nodes[slot] = i as u32;
        cursor[c] += 1;
    }

    // --- Phase C: scan_candidates with the real sorted/cell buffers ---------
    let sorted_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_sorted"),
        contents: bytemuck::cast_slice(&sorted_nodes),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let cstart_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_cell_start"),
        contents: bytemuck::cast_slice(&cell_start),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let cend_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_cell_end"),
        contents: bytemuck::cast_slice(&cell_end),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let cand_len = n * MAX_CANDIDATES as usize;
    let cand_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_cand"),
        contents: bytemuck::cast_slice(&vec![EMPTY; cand_len]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let candc_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bond_cand_count"),
        contents: bytemuck::cast_slice(&vec![0u32; n]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let cand_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bond_cand_readback"),
        size: (cand_len * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let candc_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bond_candc_readback"),
        size: (n * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    {
        let (bgl, bg) = bonding_bind_group(
            device,
            &positions_buf,
            &class_buf,
            &params_buf,
            &cell_hash_buf,
            &sorted_buf,
            &cstart_buf,
            &cend_buf,
            &affinity_buf,
            &cand_buf,
            &candc_buf,
        );
        let pipeline = bonding_pipeline(device, &shader, &bgl, "scan_candidates");
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            let wg = (n as u32 + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            pass.dispatch_workgroups(wg.max(1), 1, 1);
        }
        enc.copy_buffer_to_buffer(&cand_buf, 0, &cand_readback, 0, (cand_len * 4) as u64);
        enc.copy_buffer_to_buffer(&candc_buf, 0, &candc_readback, 0, (n * 4) as u64);
        queue.submit(std::iter::once(enc.finish()));
    }

    let cand_flat = map_read_u32(device, &cand_readback, cand_len);
    let cand_counts = map_read_u32(device, &candc_readback, n);

    // Flatten the per-node slates into a sorted, deduped candidate pair list.
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    for i in 0..n {
        let c = cand_counts[i].min(MAX_CANDIDATES) as usize;
        let base = i * MAX_CANDIDATES as usize;
        for k in 0..c {
            let j = cand_flat[base + k];
            if j != EMPTY {
                pairs.push((i as u32, j));
            }
        }
    }
    pairs.sort_unstable();
    pairs.dedup();
    pairs
}

/// Relax a seeded dynamic-bond configuration on the GPU: run the `spring_step`
/// kernel (harmonic dynamic-edge spring + damped integration, no thermostat) for
/// `steps` steps and return the final interleaved x,y,z positions. The CPU↔GPU
/// equivalence gate uses this to confirm the same bonds relax to the same
/// coordination histogram / closed-form distances on both backends.
pub fn gpu_relax_bonds(
    gpu: &GpuCtx,
    settings: &GeometricSettings,
    positions: &[f32],
    bonds: &[(u32, u32)],
    steps: usize,
) -> Vec<f32> {
    let n = positions.len() / 3;
    let device = &gpu.device;
    let queue = &gpu.queue;

    // Build a per-node bond CSR (both directions) so each thread walks its bonds.
    let mut deg = vec![0u32; n];
    for &(a, b) in bonds {
        deg[a as usize] += 1;
        deg[b as usize] += 1;
    }
    let mut bond_off = vec![0u32; n + 1];
    for i in 0..n {
        bond_off[i + 1] = bond_off[i] + deg[i];
    }
    let total = *bond_off.last().unwrap() as usize;
    let mut bond_adj = vec![0u32; total.max(1)];
    let mut cur = bond_off[..n].to_vec();
    for &(a, b) in bonds {
        let (ai, bi) = (a as usize, b as usize);
        bond_adj[cur[ai] as usize] = b;
        cur[ai] += 1;
        bond_adj[cur[bi] as usize] = a;
        cur[bi] += 1;
    }

    let mut pos4 = vec![0.0f32; n * 4];
    for i in 0..n {
        pos4[4 * i] = positions[3 * i];
        pos4[4 * i + 1] = positions[3 * i + 1];
        pos4[4 * i + 2] = positions[3 * i + 2];
    }
    let positions_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sp_positions"),
        contents: bytemuck::cast_slice(&pos4),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
    });
    let velocities_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sp_velocities"),
        contents: bytemuck::cast_slice(&vec![0.0f32; n * 4]),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let params = SpringParamsRaw {
        n_nodes: n as u32,
        bond_stiffness: settings.bond_stiffness,
        rest_len: settings.r_bond.max(0.0),
        time_step: settings.time_step,
        damping: settings.damping.clamp(0.0, 1.0),
        max_step: settings.max_step,
        _p0: 0,
        _p1: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sp_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let off_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sp_bond_off"),
        contents: bytemuck::cast_slice(&bond_off),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let adj_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("sp_bond_adj"),
        contents: bytemuck::cast_slice(&bond_adj),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sp_readback"),
        size: (n * 4 * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sp_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "../shaders/geometric_bonding.wgsl"
        ))),
    });

    let storage_rw = |b: u32| storage_entry(b, false);
    let storage_ro = |b: u32| storage_entry(b, true);
    let uniform = |b: u32| uniform_entry(b);
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sp_bgl"),
        entries: &[
            storage_rw(0),
            storage_rw(1),
            uniform(2),
            storage_ro(3),
            storage_ro(4),
        ],
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sp_bg"),
        layout: &bgl,
        entries: &[
            entry(0, &positions_buf),
            entry(1, &velocities_buf),
            entry(2, &params_buf),
            entry(3, &off_buf),
            entry(4, &adj_buf),
        ],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sp_pipeline"),
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sp_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        })),
        module: &shader,
        entry_point: Some("spring_step"),
        compilation_options: Default::default(),
        cache: None,
    });

    let wg = (n as u32 + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
    for _ in 0..steps {
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(wg.max(1), 1, 1);
        }
        queue.submit(std::iter::once(enc.finish()));
    }
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&positions_buf, 0, &readback, 0, (n * 4 * 4) as u64);
    queue.submit(std::iter::once(enc.finish()));

    let out4 = map_read_f32(device, &readback, n * 4);
    let mut out = Vec::with_capacity(3 * n);
    for i in 0..n {
        out.push(out4[4 * i]);
        out.push(out4[4 * i + 1]);
        out.push(out4[4 * i + 2]);
    }
    out
}

// ---------------------------------------------------------------------------
// wgpu plumbing helpers
// ---------------------------------------------------------------------------

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

fn entry(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}

#[allow(clippy::too_many_arguments)]
fn bonding_bind_group(
    device: &Arc<wgpu::Device>,
    positions: &wgpu::Buffer,
    class: &wgpu::Buffer,
    params: &wgpu::Buffer,
    cell_hash: &wgpu::Buffer,
    sorted: &wgpu::Buffer,
    cell_start: &wgpu::Buffer,
    cell_end: &wgpu::Buffer,
    affinity: &wgpu::Buffer,
    cand: &wgpu::Buffer,
    cand_count: &wgpu::Buffer,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bond_bgl"),
        entries: &[
            storage_entry(0, true),  // positions
            storage_entry(1, true),  // node_class
            uniform_entry(2),        // params
            storage_entry(3, false), // cell_hash (rw)
            storage_entry(4, true),  // sorted_nodes
            storage_entry(5, true),  // cell_start
            storage_entry(6, true),  // cell_end
            storage_entry(7, true),  // class_affinity
            storage_entry(8, false), // cand (rw)
            storage_entry(9, false), // cand_count (rw)
        ],
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bond_bg"),
        layout: &bgl,
        entries: &[
            entry(0, positions),
            entry(1, class),
            entry(2, params),
            entry(3, cell_hash),
            entry(4, sorted),
            entry(5, cell_start),
            entry(6, cell_end),
            entry(7, affinity),
            entry(8, cand),
            entry(9, cand_count),
        ],
    });
    (bgl, bg)
}

fn bonding_pipeline(
    device: &Arc<wgpu::Device>,
    shader: &wgpu::ShaderModule,
    bgl: &wgpu::BindGroupLayout,
    entry_point: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("bond_pipeline"),
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bond_layout"),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        })),
        module: shader,
        entry_point: Some(entry_point),
        compilation_options: Default::default(),
        cache: None,
    })
}

fn map_read_u32(device: &Arc<wgpu::Device>, buf: &wgpu::Buffer, len: usize) -> Vec<u32> {
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let v: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data)[..len].to_vec();
    drop(data);
    buf.unmap();
    v
}

fn map_read_f32(device: &Arc<wgpu::Device>, buf: &wgpu::Buffer, len: usize) -> Vec<f32> {
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range();
    let v: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data)[..len].to_vec();
    drop(data);
    buf.unmap();
    v
}
