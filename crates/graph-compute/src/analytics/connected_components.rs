//! GPU connected components — min-label propagation over the symmetrized CSR.
//!
//! A one-shot analytic (per-node component label), so a free function like
//! [`super::gpu_pagerank`], not a `LayoutEngine`. Hardware-agnostic: the WGSL
//! runs on Metal / Vulkan / DX12 via wgpu. Each node ends labeled with the
//! smallest node index in its undirected component (the canonical WCC labeling),
//! so two nodes share a component iff they share a label.
//!
//! Unlike PageRank this needs **no dangling handling** — an isolated node simply
//! keeps its own label (a singleton component). Labels are `u32` (exact, no
//! precision concern). Convergence uses a `u32` atomic flag and is O(diameter).

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::engines::EngineCtx;
use crate::sim::CsrGraph;

const WORKGROUP_SIZE: u32 = 64;

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

/// CPU connected-components reference (union-find), used as the test oracle and
/// the no-GPU fallback. Same labeling convention as the GPU kernel: each node
/// gets the minimum node index in its component.
pub fn cpu_connected_components(graph: &CsrGraph) -> Vec<u32> {
    let n = graph.n_nodes as usize;
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize]; // path-halving
            x = parent[x as usize];
        }
        x
    }
    for v in 0..n {
        for e in graph.offsets[v] as usize..graph.offsets[v + 1] as usize {
            let u = graph.neighbors[e];
            let (rv, ru) = (find(&mut parent, v as u32), find(&mut parent, u));
            if rv != ru {
                // Union toward the smaller root so labels are the component min.
                let (lo, hi) = (rv.min(ru), rv.max(ru));
                parent[hi as usize] = lo;
            }
        }
    }
    // Flatten so every node points directly at its component-min root.
    (0..n as u32).map(|v| find(&mut parent, v)).collect()
}

/// Connected components on the GPU. Returns each node's component label (the
/// min node index in its undirected component), in CSR node order. Requires a
/// wgpu device on `ctx` (callers without a GPU should fall back to
/// [`cpu_connected_components`]).
pub fn gpu_connected_components(ctx: &EngineCtx, graph: &CsrGraph) -> Result<Vec<u32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_connected_components requires a wgpu device".to_string())?;
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

    let offsets_buf = storage_u32(&device, "cc_offsets", &graph.offsets);
    let neighbors_buf = storage_u32(&device, "cc_neighbors", &graph.neighbors);

    let label_init: Vec<u32> = (0..n as u32).collect();
    let label_a = storage_u32(&device, "cc_label_a", &label_init);
    let label_b = storage_u32(&device, "cc_label_b", &label_init);

    let changed = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cc_changed"),
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
        label: Some("cc_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Small readback buffers for the convergence flag and the final labels.
    let flag_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cc_flag_readback"),
        size: 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let label_bytes = (n * std::mem::size_of::<u32>()) as u64;
    let label_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cc_label_readback"),
        size: label_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cc_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "../shaders/connected_components.wgsl"
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
        label: Some("cc_bgl"),
        entries: &[
            storage_ro(0), // offsets
            storage_ro(1), // neighbors
            storage_ro(2), // label_in
            storage_rw(3), // label_out
            storage_rw(4), // changed
            uniform(5),    // params
        ],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("cc_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("cc_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("cc_step"),
        compilation_options: Default::default(),
        cache: None,
    });

    let make_bg = |label: &str, lin: &wgpu::Buffer, lout: &wgpu::Buffer| {
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
                    resource: lin.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: lout.as_entire_binding(),
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
    let bg_a_to_b = make_bg("cc_bg_a_to_b", &label_a, &label_b);
    let bg_b_to_a = make_bg("cc_bg_b_to_a", &label_b, &label_a);

    let workgroups = (n as u32).div_ceil(WORKGROUP_SIZE).max(1);

    // Iterate min-propagation until no label changes. One submit + flag readback
    // per step (a sync point) — fine since convergence is O(diameter); capped at
    // n steps as a safety net against a pathological non-converging input.
    let mut cur = 0u32; // 0 ⇒ live labels in label_a
    let mut converged = false;
    for _ in 0..n {
        queue.write_buffer(&changed, 0, bytemuck::bytes_of(&0u32));
        let bg = if cur == 0 { &bg_a_to_b } else { &bg_b_to_a };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cc_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
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
            .map_err(|e| format!("cc flag readback channel: {e}"))?
            .map_err(|e| format!("cc flag readback map: {e:?}"))?;
        let flag = {
            let data = slice.get_mapped_range();
            let v = bytemuck::cast_slice::<u8, u32>(&data)[0];
            v
        };
        flag_readback.unmap();

        cur ^= 1; // labels just written live in the "other" buffer
        if flag == 0 {
            converged = true;
            break;
        }
    }
    let _ = converged;

    // Read the live labels.
    let live = if cur == 0 { &label_a } else { &label_b };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("cc_final_copy"),
    });
    encoder.copy_buffer_to_buffer(live, 0, &label_readback, 0, label_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = label_readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("cc label readback channel: {e}"))?
        .map_err(|e| format!("cc label readback map: {e:?}"))?;
    let data = slice.get_mapped_range();
    let out: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data).to_vec();
    drop(data);
    label_readback.unmap();
    Ok(out)
}
