//! GPU PageRank — pull-style power iteration over the symmetrized CSR.
//!
//! A **one-shot analytic, not a `LayoutEngine`**: it returns one scalar per node
//! (the stationary rank), not positions, so it's a free function rather than an
//! engine behind `StepOutput { positions }` (mirrors `geometric_bonding_gpu`'s
//! free-function GPU helpers). Hardware-agnostic by construction — the WGSL runs
//! on Metal / Vulkan / DX12 via wgpu, replacing the NVIDIA-only cuGraph
//! diagnostic.
//!
//! Numerically matches the CPU oracle `super::super::engines::geometric::pagerank`
//! (push form) on the undirected/symmetrized CSR. See `shaders/pagerank.wgsl`.
//!
//! ## Precision
//!
//! Ranks are **f32**. At 8M nodes ranks ≈ 1/n ≈ 1.3e-7, below f16's smallest
//! normal (~6.1e-5), so f16 ranks underflow — the rank vector stays f32. The f16
//! storage win is for the *weighted* SpMV primitive's edge values (a later
//! milestone, gated on `wgpu::Features::SHADER_F16`), not for these ranks.

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
    damping: f32,
    teleport: f32,
    _pad: f32,
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

fn storage_f32(device: &wgpu::Device, label: &str, data: &[f32]) -> wgpu::Buffer {
    let pad = [0.0f32];
    let bytes: &[f32] = if data.is_empty() { &pad } else { data };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(bytes),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    })
}

/// Power-iteration PageRank on the GPU. `damping` is the usual 0.85; `iters` is a
/// fixed iteration count (matches the CPU oracle, whose default is 50).
///
/// Returns the per-node rank in CSR node order. Requires a wgpu device on `ctx`
/// (callers without a GPU should fall back to the CPU oracle). For now the graph
/// must have **no dangling (degree-0) nodes** — the global dangling-mass
/// redistribution is a follow-up; `Err` is returned if a dangling node is found
/// so the caller can fall back.
pub fn gpu_pagerank(
    ctx: &EngineCtx,
    graph: &CsrGraph,
    damping: f32,
    iters: u32,
) -> Result<Vec<f32>, String> {
    let gpu = ctx
        .gpu
        .as_ref()
        .ok_or_else(|| "gpu_pagerank requires a wgpu device".to_string())?;
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

    // Host-precompute inv_deg; reject dangling nodes (degree 0) for this version.
    // (range loop: `v` indexes both the offsets pair and inv_deg in lock-step.)
    let mut inv_deg = vec![0.0f32; n];
    #[allow(clippy::needless_range_loop)]
    for v in 0..n {
        let deg = graph.offsets[v + 1] - graph.offsets[v];
        if deg == 0 {
            return Err(format!(
                "gpu_pagerank requires no dangling nodes (node {v} has degree 0); \
                 fall back to the CPU oracle until dangling-mass redistribution lands"
            ));
        }
        inv_deg[v] = 1.0 / deg as f32;
    }

    let inv_n = 1.0 / n as f32;
    let teleport = (1.0 - damping) * inv_n;

    let offsets_buf = storage_u32(&device, "pr_offsets", &graph.offsets);
    let neighbors_buf = storage_u32(&device, "pr_neighbors", &graph.neighbors);
    let inv_deg_buf = storage_f32(&device, "pr_inv_deg", &inv_deg);

    let rank_init = vec![inv_n; n];
    let rank_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pr_rank_a"),
        contents: bytemuck::cast_slice(&rank_init),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let rank_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pr_rank_b"),
        contents: bytemuck::cast_slice(&vec![0.0f32; n]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });

    let params = ParamsRaw {
        n: n as u32,
        damping,
        teleport,
        _pad: 0.0,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pr_params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let rank_bytes = (n * std::mem::size_of::<f32>()) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pr_readback"),
        size: rank_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pr_shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../shaders/pagerank.wgsl"))),
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
        label: Some("pr_bgl"),
        entries: &[
            storage_ro(0), // offsets
            storage_ro(1), // neighbors
            storage_ro(2), // inv_deg
            storage_ro(3), // rank_in
            storage_rw(4), // rank_out
            uniform(5),    // params
        ],
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pr_pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pr_layout"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            }),
        ),
        module: &shader,
        entry_point: Some("pr_step"),
        compilation_options: Default::default(),
        cache: None,
    });

    let make_bg = |label: &str, rank_in: &wgpu::Buffer, rank_out: &wgpu::Buffer| {
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
                    resource: inv_deg_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: rank_in.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: rank_out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        })
    };
    let bg_a_to_b = make_bg("pr_bg_a_to_b", &rank_a, &rank_b);
    let bg_b_to_a = make_bg("pr_bg_b_to_a", &rank_b, &rank_a);

    let workgroups = (n as u32).div_ceil(WORKGROUP_SIZE).max(1);

    // All iterations + the readback copy in ONE encoder / ONE submit. Adjacent
    // compute passes in one encoder get an automatic storage barrier, so the
    // ping-pong dependency between iterations is honored without a submit each.
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("pr_encoder"),
    });
    let mut cur = 0u32; // 0 ⇒ live ranks in rank_a
    for _ in 0..iters {
        let bg = if cur == 0 { &bg_a_to_b } else { &bg_b_to_a };
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        cur ^= 1;
    }
    let live = if cur == 0 { &rank_a } else { &rank_b };
    encoder.copy_buffer_to_buffer(live, 0, &readback, 0, rank_bytes);
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| format!("pagerank readback channel: {e}"))?
        .map_err(|e| format!("pagerank readback map: {e:?}"))?;

    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
    drop(data);
    readback.unmap();
    Ok(out)
}

/// CPU PageRank — the push form from `engines::geometric::pagerank`, reproduced
/// here as the public reference + the CPU fallback for hosts without a wgpu
/// adapter (the `pagerank` bin uses it when `EngineCtx::try_new_gpu` finds no
/// GPU). Unlike [`gpu_pagerank`] it handles dangling (degree-0) nodes by
/// redistributing their mass uniformly, so it is always correct; the GPU path
/// rejects dangling nodes until that redistribution is ported to a kernel.
///
/// On a symmetrized CSR with no dangling nodes it is numerically identical to
/// [`gpu_pagerank`] (the cross-oracle for the GPU kernel).
pub fn cpu_pagerank(g: &CsrGraph, damping: f32, iters: u32) -> Vec<f32> {
    let n = g.n_nodes as usize;
    if n == 0 {
        return Vec::new();
    }
    let inv_n = 1.0 / n as f32;
    let mut rank = vec![inv_n; n];
    let out_deg: Vec<f32> = (0..n)
        .map(|v| (g.offsets[v + 1] - g.offsets[v]) as f32)
        .collect();
    let teleport = (1.0 - damping) * inv_n;
    for _ in 0..iters {
        let mut next = vec![0.0f32; n];
        let mut dangling = 0.0f32;
        // range loop: `v` indexes out_deg, rank, and the offsets pair together.
        #[allow(clippy::needless_range_loop)]
        for v in 0..n {
            if out_deg[v] == 0.0 {
                dangling += rank[v];
                continue;
            }
            let share = rank[v] / out_deg[v];
            for e in g.offsets[v] as usize..g.offsets[v + 1] as usize {
                next[g.neighbors[e] as usize] += share;
            }
        }
        let dangling_share = damping * dangling * inv_n;
        for r in next.iter_mut() {
            *r = teleport + dangling_share + damping * *r;
        }
        rank = next;
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Undirected ring 0—1—…—(n-1)—0 (every node degree 2, no dangling).
    fn ring(n: u32) -> CsrGraph {
        let mut offsets = Vec::with_capacity((n + 1) as usize);
        let mut neighbors = Vec::new();
        for i in 0..n {
            offsets.push(neighbors.len() as u32);
            neighbors.push((i + n - 1) % n);
            neighbors.push((i + 1) % n);
        }
        offsets.push(neighbors.len() as u32);
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    #[test]
    fn gpu_pagerank_matches_cpu_oracle() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping gpu_pagerank parity test (no GPU)");
            return;
        }
        let _ = &mut ctx;

        // path(16) is symmetric with degree-1 endpoints (no dangling); ring(32)
        // is regular. Both exercise the pull==push equivalence.
        for g in [CsrGraph::path(16), ring(32)] {
            let cpu = cpu_pagerank(&g, 0.85, 50);
            let gpu = gpu_pagerank(&ctx, &g, 0.85, 50).expect("gpu pagerank");
            assert_eq!(gpu.len(), cpu.len());
            for (i, (a, b)) in gpu.iter().zip(cpu.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-4,
                    "node {i}: gpu {a} vs cpu {b} (Δ={})",
                    (a - b).abs()
                );
            }
            // Mass is conserved (no dangling, symmetric stochastic).
            let sum: f32 = gpu.iter().sum();
            assert!((sum - 1.0).abs() < 1e-2, "rank mass {sum} != 1");
        }
    }

    #[test]
    fn gpu_pagerank_rejects_dangling() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping gpu_pagerank dangling test (no GPU)");
            return;
        }
        let _ = &mut ctx;
        // node 1 has degree 0 (dangling).
        let g = CsrGraph {
            n_nodes: 2,
            offsets: vec![0, 1, 1],
            neighbors: vec![1],
        };
        assert!(gpu_pagerank(&ctx, &g, 0.85, 10).is_err());
    }
}
