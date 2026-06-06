//! Weighted sparse matrix–vector product `y = A·x` over a CSR matrix — the
//! unifying primitive the graph analytics reduce to (PageRank, CC, BFS are all
//! this SpMV with different semirings). Hardware-agnostic (Metal/Vulkan/DX12).
//!
//! Values accumulate in **f32**. The f16-storage variant (half-precision
//! `weights`/`x`, f32 accumulate — the real memory win for weighted
//! chemical-sim matrices) is gated behind `wgpu::Features::SHADER_F16` and is a
//! follow-up; this f32 primitive is the foundation + the correctness oracle for
//! it.

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
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
