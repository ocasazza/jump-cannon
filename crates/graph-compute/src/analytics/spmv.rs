//! Weighted sparse matrix–vector product `y = A·x` over a CSR matrix — the
//! unifying primitive the graph analytics reduce to (PageRank, CC, BFS are all
//! this SpMV with different semirings). Hardware-agnostic (Metal/Vulkan/DX12).
//!
//! Two value precisions:
//!   - [`gpu_spmv`] — f32 storage + f32 accumulate (the foundation + oracle).
//!   - [`gpu_spmv_f16`] — f16 *storage* (half the footprint — the win for large
//!     weighted matrices like chemical-sim graphs), f32 *accumulate*. Stored as
//!     two halves per u32 + decoded with the core `unpack2x16float` builtin, so
//!     it needs no `SHADER_F16` device feature (which Naga doesn't implement in
//!     wgpu 23 anyway). This is the `Vec<precision>` knob — on the *weighted*
//!     primitive, not PageRank's ranks (which underflow f16 at scale).

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
use half::f16;
use wgpu::util::DeviceExt;

use crate::engines::EngineCtx;

/// A weighted CSR matrix. Row `v` spans `offsets[v]..offsets[v+1]`; column
/// indices in `neighbors`, values in `weights` (same length = nnz). Square
/// `n_nodes × n_nodes` for graph use, but SpMV only needs `x.len() == n_nodes`.
#[derive(Debug, Clone)]
pub struct WeightedCsr {
    pub n_nodes: u32,
    pub offsets: Vec<u32>,
    pub neighbors: Vec<u32>,
    pub weights: Vec<f32>,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct ParamsRaw {
    n: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
}

fn storage_u32(device: &wgpu::Device, label: &str, data: &[u32]) -> wgpu::Buffer {
    let pad = [0u32];
    let bytes: &[u32] = if data.is_empty() { &pad } else { data };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(bytes),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

fn storage_f32(device: &wgpu::Device, label: &str, data: &[f32], copy_src: bool) -> wgpu::Buffer {
    let pad = [0.0f32];
    let bytes: &[f32] = if data.is_empty() { &pad } else { data };
    let mut usage = wgpu::BufferUsages::STORAGE;
    if copy_src {
        usage |= wgpu::BufferUsages::COPY_SRC;
    }
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(bytes),
        usage,
    })
}

/// Pack f32 values to f16 stored two-per-u32 (low 16 bits = even index, high =
/// odd), matching the shader's `unpack2x16float(packed[i>>1])[i&1]` decode.
fn pack_f16_pairs(vals: &[f32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(vals.len().div_ceil(2).max(1));
    let mut i = 0;
    while i < vals.len() {
        let lo = f16::from_f32(vals[i]).to_bits() as u32;
        let hi = if i + 1 < vals.len() {
            f16::from_f32(vals[i + 1]).to_bits() as u32
        } else {
            0
        };
        out.push(lo | (hi << 16));
        i += 2;
    }
    if out.is_empty() {
        out.push(0);
    }
    out
}

/// CPU reference SpMV — the test oracle + no-GPU fallback.
pub fn cpu_spmv(a: &WeightedCsr, x: &[f32]) -> Vec<f32> {
    let n = a.n_nodes as usize;
    (0..n)
        .map(|v| {
            let mut acc = 0.0f32;
            for e in a.offsets[v] as usize..a.offsets[v + 1] as usize {
                acc += a.weights[e] * x[a.neighbors[e] as usize];
            }
            acc
        })
        .collect()
}

/// Compute `y = A·x` on the GPU. Requires a wgpu device on `ctx` (else fall back
/// to [`cpu_spmv`]). `x.len()` must equal `a.n_nodes`.
pub fn gpu_spmv(ctx: &EngineCtx, a: &WeightedCsr, x: &[f32]) -> Result<Vec<f32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_spmv requires a wgpu device".to_string())?;
    let device = gpu.device.clone();
    let queue = gpu.queue.clone();

    let n = a.n_nodes as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if a.offsets.len() != n + 1 {
        return Err(format!(
            "offsets length {} != n + 1 ({})",
            a.offsets.len(),
            n + 1
        ));
    }
    if a.neighbors.len() != a.weights.len() {
        return Err(format!(
            "neighbors ({}) and weights ({}) length mismatch",
            a.neighbors.len(),
            a.weights.len()
        ));
    }
    if x.len() != n {
        return Err(format!("x length {} != n ({n})", x.len()));
    }

    let offsets_buf = storage_u32(&device, "spmv_offsets", &a.offsets);
    let neighbors_buf = storage_u32(&device, "spmv_neighbors", &a.neighbors);
    let weights_buf = storage_f32(&device, "spmv_weights", &a.weights, false);
    let x_buf = storage_f32(&device, "spmv_x", x, false);
    let y_buf = storage_f32(&device, "spmv_y", &vec![0.0f32; n], true);

    let params = ParamsRaw {
        n: n as u32,
        _p0: 0,
        _p1: 0,
        _p2: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("spmv_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let y_bytes = (n * std::mem::size_of::<f32>()) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spmv_readback"),
        size: y_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spmv_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../shaders/spmv.wgsl"))),
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
        label: Some("spmv_bgl"),
        entries: &[
            storage_ro(0),
            storage_ro(1),
            storage_ro(2),
            storage_ro(3),
            storage_rw(4),
            uniform(5),
        ],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("spmv_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("spmv_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("spmv"),
        compilation_options: Default::default(),
        cache: None,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("spmv_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: offsets_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: neighbors_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: weights_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: x_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: y_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });

    let workgroups = (n as u32).div_ceil(64).max(1);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("spmv_encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&y_buf, 0, &readback, 0, y_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("spmv readback channel: {e}"))?
        .map_err(|e| format!("spmv readback map: {e:?}"))?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
    drop(data);
    readback.unmap();
    Ok(out)
}

/// Half-precision `y = A·x`: `weights` and `x` are stored as **f16** (half the
/// footprint — the win for large weighted matrices) packed two-per-u32 and
/// decoded in-shader via `unpack2x16float`, while products accumulate in
/// **f32**. Works on any wgpu adapter (no SHADER_F16 device feature — that
/// path is unimplemented in Naga on wgpu 23). Results match the f32 path to
/// within f16 rounding (~1e-2 for unit-scale values), not exactly — that's the
/// precision/footprint trade.
pub fn gpu_spmv_f16(ctx: &EngineCtx, a: &WeightedCsr, x: &[f32]) -> Result<Vec<f32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_spmv_f16 requires a wgpu device".to_string())?;
    let device = gpu.device.clone();
    let queue = gpu.queue.clone();

    let n = a.n_nodes as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if a.offsets.len() != n + 1 {
        return Err(format!(
            "offsets length {} != n + 1 ({})",
            a.offsets.len(),
            n + 1
        ));
    }
    if a.neighbors.len() != a.weights.len() {
        return Err(format!(
            "neighbors ({}) and weights ({}) length mismatch",
            a.neighbors.len(),
            a.weights.len()
        ));
    }
    if x.len() != n {
        return Err(format!("x length {} != n ({n})", x.len()));
    }

    let weights_packed = pack_f16_pairs(&a.weights);
    let x_packed = pack_f16_pairs(x);

    let offsets_buf = storage_u32(&device, "spmv16_offsets", &a.offsets);
    let neighbors_buf = storage_u32(&device, "spmv16_neighbors", &a.neighbors);
    let weights_buf = storage_u32(&device, "spmv16_weights", &weights_packed);
    let x_buf = storage_u32(&device, "spmv16_x", &x_packed);
    let y_buf = storage_f32(&device, "spmv16_y", &vec![0.0f32; n], true);

    let params = ParamsRaw {
        n: n as u32,
        _p0: 0,
        _p1: 0,
        _p2: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("spmv16_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let y_bytes = (n * std::mem::size_of::<f32>()) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spmv16_readback"),
        size: y_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spmv16_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../shaders/spmv_f16.wgsl"))),
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
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("spmv16_bgl"),
        entries: &[
            storage_ro(0),
            storage_ro(1),
            storage_ro(2),
            storage_ro(3),
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
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

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("spmv16_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("spmv16_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("spmv_f16"),
        compilation_options: Default::default(),
        cache: None,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("spmv16_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: offsets_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: neighbors_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: weights_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: x_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: y_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });

    let workgroups = (n as u32).div_ceil(64).max(1);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("spmv16_encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&y_buf, 0, &readback, 0, y_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("spmv16 readback channel: {e}"))?
        .map_err(|e| format!("spmv16 readback map: {e:?}"))?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
    drop(data);
    readback.unmap();
    Ok(out)
}

/// Hub-aware (load-balanced) `y = A·x` for **power-law** graphs — same result as
/// [`gpu_spmv`] (up to f32 summation-order tolerance), but it does not let one
/// thread serialize a giant hub row.
///
/// The baseline [`gpu_spmv`] is thread-per-row: a hub row of degree ~`n` is
/// reduced by a *single* thread while its 63 workgroup neighbours sit idle — the
/// long pole on a scale-free graph (a handful of rows touch almost every column).
/// This variant is **workgroup-per-row** (a warp/CTA-per-row segmented
/// reduction): it dispatches `n` workgroups, and the 64 lanes of row `v`'s
/// workgroup cooperatively walk that row (lane `l` strides edges `start+l`,
/// `start+l+64`, …) into private accumulators, then a workgroup-shared tree
/// reduction collapses them to `y[v]`. Long hub rows get the whole workgroup;
/// short rows just leave most lanes idle for one cheap pass. f32 accumulate
/// throughout, so it is oracle-equivalent — not an approximation. (The summation
/// order differs from the serial baseline, hence the ~1e-4 tolerance in tests.)
pub fn gpu_spmv_hybrid(ctx: &EngineCtx, a: &WeightedCsr, x: &[f32]) -> Result<Vec<f32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_spmv_hybrid requires a wgpu device".to_string())?;
    let device = gpu.device.clone();
    let queue = gpu.queue.clone();

    let n = a.n_nodes as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if a.offsets.len() != n + 1 {
        return Err(format!(
            "offsets length {} != n + 1 ({})",
            a.offsets.len(),
            n + 1
        ));
    }
    if a.neighbors.len() != a.weights.len() {
        return Err(format!(
            "neighbors ({}) and weights ({}) length mismatch",
            a.neighbors.len(),
            a.weights.len()
        ));
    }
    if x.len() != n {
        return Err(format!("x length {} != n ({n})", x.len()));
    }

    let offsets_buf = storage_u32(&device, "spmvh_offsets", &a.offsets);
    let neighbors_buf = storage_u32(&device, "spmvh_neighbors", &a.neighbors);
    let weights_buf = storage_f32(&device, "spmvh_weights", &a.weights, false);
    let x_buf = storage_f32(&device, "spmvh_x", x, false);
    let y_buf = storage_f32(&device, "spmvh_y", &vec![0.0f32; n], true);

    let params = ParamsRaw {
        n: n as u32,
        _p0: 0,
        _p1: 0,
        _p2: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("spmvh_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let y_bytes = (n * std::mem::size_of::<f32>()) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spmvh_readback"),
        size: y_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spmvh_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "../shaders/spmv_hybrid.wgsl"
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
        label: Some("spmvh_bgl"),
        entries: &[
            storage_ro(0),
            storage_ro(1),
            storage_ro(2),
            storage_ro(3),
            storage_rw(4),
            uniform(5),
        ],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("spmvh_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("spmvh_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("spmv_hybrid"),
        compilation_options: Default::default(),
        cache: None,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("spmvh_bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: offsets_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: neighbors_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: weights_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: x_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: y_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });

    // One workgroup per row — that is the load-balancing knob. The 64 lanes of
    // each workgroup cooperatively reduce that one row.
    let workgroups = n as u32;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("spmvh_encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&y_buf, 0, &readback, 0, y_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("spmvh readback channel: {e}"))?
        .map_err(|e| format!("spmvh readback map: {e:?}"))?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
    drop(data);
    readback.unmap();
    Ok(out)
}
