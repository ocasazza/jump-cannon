//! GPU single-source BFS — unweighted shortest-path distances over the
//! symmetrized CSR, by iterated distance relaxation (Bellman-Ford, unit
//! weights). A one-shot analytic free function like [`super::gpu_pagerank`] /
//! [`super::gpu_connected_components`]; same CSR gather, `min(dist, 1+nbr)`
//! relaxation instead of sum/min-label. Hardware-agnostic (Metal/Vulkan/DX12).
//!
//! Distances are `u32` hops; unreachable nodes are [`UNREACHABLE`] (`u32::MAX`).
//! Convergence (a `u32` atomic flag) is O(diameter).

use std::borrow::Cow;
use std::collections::VecDeque;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::engines::EngineCtx;
use crate::sim::CsrGraph;

const WORKGROUP_SIZE: u32 = 64;

/// Distance value for nodes not reachable from the source (matches the kernel's
/// `INF` sentinel).
pub const UNREACHABLE: u32 = u32::MAX;

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
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    })
}

/// CPU single-source BFS reference (queue) — the test oracle + no-GPU fallback.
/// Same convention as the GPU kernel: hop distances, [`UNREACHABLE`] for nodes
/// not reachable from `source`.
pub fn cpu_bfs(graph: &CsrGraph, source: u32) -> Vec<u32> {
    let n = graph.n_nodes as usize;
    let mut dist = vec![UNREACHABLE; n];
    if (source as usize) >= n {
        return dist;
    }
    dist[source as usize] = 0;
    let mut q = VecDeque::new();
    q.push_back(source);
    while let Some(v) = q.pop_front() {
        let d = dist[v as usize];
        for e in graph.offsets[v as usize] as usize..graph.offsets[v as usize + 1] as usize {
            let u = graph.neighbors[e];
            if dist[u as usize] == UNREACHABLE {
                dist[u as usize] = d + 1;
                q.push_back(u);
            }
        }
    }
    dist
}

/// Single-source BFS distances on the GPU. Returns hop distance per node from
/// `source` ([`UNREACHABLE`] if not reachable), in CSR node order. Requires a
/// wgpu device on `ctx` (else fall back to [`cpu_bfs`]).
pub fn gpu_bfs(ctx: &EngineCtx, graph: &CsrGraph, source: u32) -> Result<Vec<u32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_bfs requires a wgpu device".to_string())?;
    let device = gpu.device.clone();
    let queue = gpu.queue.clone();

    let n = graph.n_nodes as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if graph.offsets.len() != n + 1 {
        return Err(format!(
            "CSR offsets length {} != n_nodes + 1 ({})",
            graph.offsets.len(),
            n + 1
        ));
    }
    if (source as usize) >= n {
        return Err(format!("source {source} out of range (n = {n})"));
    }

    let offsets_buf = storage_u32(&device, "bfs_offsets", &graph.offsets);
    let neighbors_buf = storage_u32(&device, "bfs_neighbors", &graph.neighbors);

    let mut dist_init = vec![UNREACHABLE; n];
    dist_init[source as usize] = 0;
    let dist_a = storage_u32(&device, "bfs_dist_a", &dist_init);
    let dist_b = storage_u32(&device, "bfs_dist_b", &dist_init);

    let changed = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bfs_changed"),
        size: 4,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let params = ParamsRaw {
        n: n as u32,
        _p0: 0,
        _p1: 0,
        _p2: 0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("bfs_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let flag_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bfs_flag_readback"),
        size: 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let dist_bytes = (n * std::mem::size_of::<u32>()) as u64;
    let dist_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bfs_dist_readback"),
        size: dist_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("bfs_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../shaders/bfs.wgsl"))),
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
        label: Some("bfs_bgl"),
        entries: &[
            storage_ro(0),
            storage_ro(1),
            storage_ro(2),
            storage_rw(3),
            storage_rw(4),
            uniform(5),
        ],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("bfs_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bfs_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("bfs_step"),
        compilation_options: Default::default(),
        cache: None,
    });

    let make_bg = |label: &str, din: &wgpu::Buffer, dout: &wgpu::Buffer| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
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
                    resource: din.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: dout.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: changed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        })
    };
    let bg_a_to_b = make_bg("bfs_bg_a_to_b", &dist_a, &dist_b);
    let bg_b_to_a = make_bg("bfs_bg_b_to_a", &dist_b, &dist_a);

    let workgroups = (n as u32).div_ceil(WORKGROUP_SIZE).max(1);
    let (wg_x, wg_y, wg_z) = super::workgroup_dims_2d(workgroups);

    let mut cur = 0u32;
    for _ in 0..n {
        queue.write_buffer(&changed, 0, bytemuck::bytes_of(&0u32));
        let bg = if cur == 0 { &bg_a_to_b } else { &bg_b_to_a };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bfs_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, wg_z);
        }
        encoder.copy_buffer_to_buffer(&changed, 0, &flag_readback, 0, 4);
        queue.submit(std::iter::once(encoder.finish()));

        let slice = flag_readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|e| format!("bfs flag readback channel: {e}"))?
            .map_err(|e| format!("bfs flag readback map: {e:?}"))?;
        let flag = {
            let data = slice.get_mapped_range();
            bytemuck::cast_slice::<u8, u32>(&data)[0]
        };
        flag_readback.unmap();

        cur ^= 1;
        if flag == 0 {
            break;
        }
    }

    let live = if cur == 0 { &dist_a } else { &dist_b };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("bfs_final_copy"),
    });
    encoder.copy_buffer_to_buffer(live, 0, &dist_readback, 0, dist_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = dist_readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("bfs dist readback channel: {e}"))?
        .map_err(|e| format!("bfs dist readback map: {e:?}"))?;
    let data = slice.get_mapped_range();
    let out: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data).to_vec();
    drop(data);
    dist_readback.unmap();
    Ok(out)
}
