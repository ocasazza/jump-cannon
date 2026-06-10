//! GPU SGD stress-majorization engine (`"sgd-stress-gpu"`).
//!
//! WGSL port of [`SgdStressEngine`](super::sgd_stress::SgdStressEngine). Shares
//! the CPU engine's sparse-stress precompute — maxmin pivot selection + per-pivot
//! BFS distance rows (`super::sgd_stress::select_pivots`) — so a given seed picks
//! identical landmarks on both backends.
//!
//! The per-sweep update is the **Jacobi** form of s_gd2: one GPU thread per node
//! reads the start-of-sweep positions, slides only itself along its `k` pivot
//! terms toward the target graph distances, and writes to a second buffer. The
//! two position buffers ping-pong between sweeps. Unlike the CPU engine (which
//! moves *both* endpoints of a sampled pair, Gauss-Seidel), moving only the
//! optimized node makes every write target distinct → no atomics, no races, the
//! whole graph updates in one dispatch.
//!
//! ## Why Jacobi-move-self (design rationale, lit-checked)
//!
//! The canonical s_gd2 update (Zheng/Pawar/Goodman, TVCG 2019) moves *both*
//! endpoints symmetrically — an in-place Gauss-Seidel step. Reproducing that on
//! the GPU race-free would need either f32 atomic accumulation (NOT available in
//! core WebGPU/WGSL — only i32/u32 atomics) or a CYCLADES-style conflict-graph
//! coloring schedule. Conflict-free Jacobi is the pragmatic WebGPU fit, and it
//! sits in HOGWILD!'s favourable regime (lock-free async SGD converges
//! near-optimally when updates are *sparse* — pivot-stress per-node updates are).
//! The known cost is that moving only one endpoint and reading stale
//! within-sweep pivot positions can slow convergence vs the both-endpoint step.
//!
//! Value proposition: this engine's edge over the CPU path is **parallel
//! throughput + robustness to poor initial layouts**, NOT lower final stress —
//! SGD does not beat full majorization on quality once initialization fixes the
//! global arrangement (Börsig/Brandes/Pásztor, GD 2020). A good seed (e.g.
//! PivotMDS/spectral) matters more than the choice of minimizer. The
//! `gpu_stress_comparable_to_cpu_sgd` test pins the GPU/CPU quality gap.
//! Escalation path if quality lags: CYCLADES batching or same-endpoint thread
//! allocation (JCADC 2023) to recover the both-endpoint update.
//!
//! See `super::sgd_stress` for the algorithm/citations.

use std::borrow::Cow;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use wgpu::util::DeviceExt;

use super::sgd_stress::{anneal_eta, select_pivots, SgdStressSettings, SplitMix64};
use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "sgd-stress-gpu";

const WORKGROUP_SIZE: u32 = 64;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
struct ParamsRaw {
    n: u32,
    k: u32,
    eta: f32,
    _pad: f32,
}

#[allow(dead_code)]
struct Gpu {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: wgpu::ComputePipeline,
    /// Reads `pos_a`, writes `pos_b`.
    bg_a_to_b: wgpu::BindGroup,
    /// Reads `pos_b`, writes `pos_a`.
    bg_b_to_a: wgpu::BindGroup,
    pos_a: wgpu::Buffer,
    pos_b: wgpu::Buffer,
    pivot_nodes_buf: wgpu::Buffer,
    dist_buf: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    readback_buf: wgpu::Buffer,

    n: u32,
    k: u32,
    positions_byte_len: u64,
    /// Which buffer currently holds the live positions: 0 = `pos_a`, 1 = `pos_b`.
    cur: u32,
    /// Global SGD sweep counter — drives the annealing schedule across steps.
    sweep: u64,
}

pub struct SgdStressGpuEngine {
    descriptor: LayoutDescriptor,
    settings: SgdStressSettings,
    gpu: Option<Gpu>,
}

impl SgdStressGpuEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: LayoutDescriptor {
                id: LAYOUT_ID,
                kind: LayoutKind::Physics,
                display_name: "SGD stress (GPU, pivot)",
                description: "GPU-parallel stress majorization (s_gd2 SGD + Ortmann sparse \
                              pivots). One thread per node runs a conflict-free Jacobi pivot \
                              update in WGSL (WebGPU has no f32 atomics, so it moves only the \
                              optimized node, not both pair endpoints). Honors shortest-path \
                              distances. Its edge over CPU sgd-stress is parallel throughput and \
                              robustness to poor initial layouts — not lower final stress, which \
                              matches majorization given a good seed.",
                requirements: LayoutRequirements {
                    needs_edges: true,
                    needs_cpu_positions: true,
                    needs_gpu_positions_buffer: false,
                },
            },
            settings: SgdStressSettings::default(),
            gpu: None,
        }
    }
}

impl Default for SgdStressGpuEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a non-empty storage buffer from a slice, padding an empty slice to one
/// zero element so the binding stays valid (degenerate empty-graph case).
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

impl LayoutEngine for SgdStressGpuEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: SgdStressSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode sgd-stress-gpu settings: {e}"))?;
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        ctx: &mut EngineCtx,
        graph_shard: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let gpu_ctx = ctx
            .gpu
            .as_ref()
            .ok_or_else(|| "sgd-stress-gpu requires a wgpu device".to_string())?;
        let device = gpu_ctx.device.clone();
        let queue = gpu_ctx.queue.clone();

        let graph = graph_shard.graph;
        let n = graph.n_nodes as usize;
        if positions.len() != 3 * n {
            return Err(format!(
                "initial positions length {} != 3 * n_nodes {}",
                positions.len(),
                3 * n
            ));
        }

        // Shared sparse-stress precompute: identical pivots to the CPU engine.
        let mut rng = SplitMix64::new(self.settings.seed);
        let pivots = select_pivots(graph, self.settings.n_pivots, &mut rng);
        let k = pivots.len();

        // Per-node pivot distance rows, row-major dist[i*k + t]. Sentinel 0.0 for
        // unreachable / self so the shader skips the term.
        let mut pivot_nodes: Vec<u32> = Vec::with_capacity(k);
        for row in &pivots {
            pivot_nodes.push(row.pivot);
        }
        let mut dist: Vec<f32> = vec![0.0; n * k];
        for (t, row) in pivots.iter().enumerate() {
            for i in 0..n {
                let d = row.dist[i];
                dist[i * k + t] = if d == u32::MAX || d == 0 {
                    0.0
                } else {
                    d as f32
                };
            }
        }

        // vec4-packed positions (w unused) for tidy 16-byte storage alignment.
        let mut pos_vec4 = vec![0.0f32; n.max(1) * 4];
        for i in 0..n {
            pos_vec4[4 * i] = positions[3 * i];
            pos_vec4[4 * i + 1] = positions[3 * i + 1];
            pos_vec4[4 * i + 2] = positions[3 * i + 2];
        }
        let positions_byte_len = (n.max(1) * 4) as u64 * 4;

        let pos_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sgd_pos_a"),
            contents: bytemuck::cast_slice(&pos_vec4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sgd_pos_b"),
            contents: bytemuck::cast_slice(&vec![0.0f32; n.max(1) * 4]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pivot_nodes_buf = storage_u32(&device, "sgd_pivot_nodes", &pivot_nodes);
        let dist_buf = storage_f32(&device, "sgd_dist", &dist);

        let params_init = ParamsRaw {
            n: n as u32,
            k: k as u32,
            eta: self.settings.eta_max,
            _pad: 0.0,
        };
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sgd_params"),
            contents: bytemuck::bytes_of(&params_init),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sgd_readback"),
            size: positions_byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sgd_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/sgd_stress.wgsl"
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
            label: Some("sgd_bgl"),
            entries: &[
                storage_ro(0), // pos_in
                storage_rw(1), // pos_out
                storage_ro(2), // pivot_nodes
                storage_ro(3), // dist
                uniform(4),    // params
            ],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sgd_pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("sgd_layout"),
                    bind_group_layouts: &[&bgl],
                    push_constant_ranges: &[],
                }),
            ),
            module: &shader,
            entry_point: Some("sgd_step"),
            compilation_options: Default::default(),
            cache: None,
        });

        let make_bg = |label: &str, in_buf: &wgpu::Buffer, out_buf: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: in_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: out_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: pivot_nodes_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: dist_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: params_buf.as_entire_binding(),
                    },
                ],
            })
        };
        let bg_a_to_b = make_bg("sgd_bg_a_to_b", &pos_a, &pos_b);
        let bg_b_to_a = make_bg("sgd_bg_b_to_a", &pos_b, &pos_a);

        self.gpu = Some(Gpu {
            device,
            queue,
            pipeline,
            bg_a_to_b,
            bg_b_to_a,
            pos_a,
            pos_b,
            pivot_nodes_buf,
            dist_buf,
            params_buf,
            readback_buf,
            n: n as u32,
            k: k as u32,
            positions_byte_len,
            cur: 0,
            sweep: 0,
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let gpu = self.gpu.as_mut().expect("sgd-stress-gpu step before init");

        let n = gpu.n;
        if n == 0 || gpu.k == 0 {
            return StepOutput::positions_only(Vec::new());
        }

        let sweeps = settings.sweeps_per_step.max(1);
        let workgroups = n.div_ceil(WORKGROUP_SIZE).max(1);

        // One eta for the whole step. Annealing within a single step (≤ a few
        // dozen sweeps) is negligible, and a constant eta lets us record ALL
        // sweeps + the readback copy in ONE command encoder / ONE submit —
        // instead of a submit per sweep, which dominated GPU wall-time. Adjacent
        // compute passes in one encoder get an automatic storage barrier, so the
        // ping-pong dependency between sweeps is honored.
        let eta = anneal_eta(
            gpu.sweep,
            settings.eta_max,
            settings.eta_min,
            settings.n_anneal_steps,
        );
        let params = ParamsRaw {
            n,
            k: gpu.k,
            eta,
            _pad: 0.0,
        };
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut cur = gpu.cur;
        for _ in 0..sweeps {
            let bind_group = if cur == 0 {
                &gpu.bg_a_to_b
            } else {
                &gpu.bg_b_to_a
            };
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: None,
                    timestamp_writes: None,
                });
                pass.set_pipeline(&gpu.pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.dispatch_workgroups(workgroups, 1, 1);
            }
            cur ^= 1;
        }

        // Read back whichever buffer now holds the live positions (same encoder).
        let live = if cur == 0 { &gpu.pos_a } else { &gpu.pos_b };
        encoder.copy_buffer_to_buffer(live, 0, &gpu.readback_buf, 0, gpu.positions_byte_len);
        gpu.queue.submit(std::iter::once(encoder.finish()));
        gpu.cur = cur;
        gpu.sweep = gpu.sweep.wrapping_add(sweeps as u64);

        let slice = gpu.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let vec4_floats: &[f32] = bytemuck::cast_slice(&data);
        let mut out = Vec::with_capacity(3 * n as usize);
        for i in 0..n as usize {
            out.push(vec4_floats[4 * i]);
            out.push(vec4_floats[4 * i + 1]);
            out.push(vec4_floats[4 * i + 2]);
        }
        drop(data);
        gpu.readback_buf.unmap();
        StepOutput::positions_only(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::CsrGraph;

    fn ring_positions(n: usize) -> Vec<f32> {
        let mut p = vec![0.0f32; 3 * n];
        for i in 0..n {
            let t = (i as f32) / (n.max(1) as f32) * std::f32::consts::TAU;
            p[3 * i] = t.cos();
            p[3 * i + 1] = t.sin();
        }
        p
    }

    /// Reference full O(n²) stress (small n only) — mirrors the CPU engine's test.
    fn full_stress(g: &CsrGraph, pos: &[f32]) -> f32 {
        use super::super::sgd_stress::bfs_distances;
        let n = g.n_nodes as usize;
        let mut total = 0.0f64;
        for i in 0..n {
            let d = bfs_distances(g, i as u32);
            for j in (i + 1)..n {
                let dij = d[j];
                if dij == u32::MAX || dij == 0 {
                    continue;
                }
                let dij = dij as f32;
                let dx = pos[3 * i] - pos[3 * j];
                let dy = pos[3 * i + 1] - pos[3 * j + 1];
                let dz = pos[3 * i + 2] - pos[3 * j + 2];
                let mag = (dx * dx + dy * dy + dz * dz).sqrt();
                let w = 1.0 / (dij * dij);
                let r = mag - dij;
                total += (w * r * r) as f64;
            }
        }
        total as f32
    }

    #[test]
    fn smoke_sgd_stress_gpu_init_step_reduces_stress() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping sgd-stress-gpu test (no GPU)");
            return;
        }

        let g = CsrGraph::path(16);
        let positions = ring_positions(16);
        let mut engine = SgdStressGpuEngine::new();
        let shard = CsrShard::whole(&g);
        engine.init(&mut ctx, &shard, &positions).expect("init");

        let stress_before = full_stress(&g, &positions);
        let mut out = positions.clone();
        for _ in 0..60 {
            out = engine.step(&mut ctx).positions;
        }
        assert_eq!(out.len(), positions.len());
        assert!(
            out.iter().all(|v| v.is_finite()),
            "positions must stay finite"
        );
        let stress_after = full_stress(&g, &out);
        assert!(
            stress_after < stress_before,
            "GPU SGD stress should decrease: before={stress_before} after={stress_after}"
        );
    }

    /// Empirical quality gap between the GPU Jacobi update (move-only-self) and
    /// the canonical CPU s_gd2 (both-endpoint Gauss-Seidel). The literature
    /// (s_gd2 TVCG'19; conflict-free parallel SGD JCADC'23) predicts Jacobi may
    /// converge slower / land at slightly higher stress, but no published
    /// benchmark isolates it for pivot stress — so we measure it here. Both run
    /// from the SAME seed + initial layout; both must reduce stress substantially
    /// and the GPU result must stay in the same quality ballpark (generous bound,
    /// since the update order differs by design).
    #[test]
    fn gpu_stress_comparable_to_cpu_sgd() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping sgd-stress-gpu parity test (no GPU)");
            return;
        }

        let g = CsrGraph::path(24);
        let init = ring_positions(24); // deliberately poor (tangled) initial layout
        const STEPS: usize = 200;

        // All-pairs (i, j, d_ij) terms for the scale-invariant stress metric.
        let terms = {
            use crate::engines::sgd_stress::bfs_distances;
            let n = g.n_nodes as usize;
            let mut t = Vec::new();
            for i in 0..n {
                let d = bfs_distances(&g, i as u32);
                for (j, &dij) in d.iter().enumerate().skip(i + 1) {
                    if dij != u32::MAX && dij != 0 {
                        t.push((i as u32, j as u32, dij as f32));
                    }
                }
            }
            t
        };
        let ns0 = graph_layouts::metrics::scale_normalized_stress(&init, &terms);

        // GPU Jacobi (move only self).
        let mut gpu = SgdStressGpuEngine::new();
        gpu.init(&mut ctx, &CsrShard::whole(&g), &init)
            .expect("gpu init");
        let mut gpu_out = init.clone();
        for _ in 0..STEPS {
            gpu_out = gpu.step(&mut ctx).positions;
        }
        let gpu_ns = graph_layouts::metrics::scale_normalized_stress(&gpu_out, &terms);

        // CPU s_gd2 (both endpoints) from the same seed + init.
        let mut cpu = crate::engines::sgd_stress::SgdStressEngine::new();
        let mut cctx = EngineCtx::cpu_only();
        cpu.init(&mut cctx, &CsrShard::whole(&g), &init)
            .expect("cpu init");
        let mut cpu_out = init.clone();
        for _ in 0..STEPS {
            cpu_out = cpu.step(&mut cctx).positions;
        }
        let cpu_ns = graph_layouts::metrics::scale_normalized_stress(&cpu_out, &terms);

        // The GPU engine must reduce the (scale-invariant) objective.
        assert!(
            gpu_ns < ns0,
            "GPU SGD should reduce normalized stress: {ns0} -> {gpu_ns}"
        );
        // ...and must not regress vs the validated CPU baseline. One-directional
        // by design: the full-step Jacobi often converges FASTER than half-step
        // s_gd2 at equal step budget (so GPU may be < CPU); we only guard against
        // it being meaningfully worse. Scale-normalized per Kobourov GD-metrics —
        // raw stress is scale-sensitive and not a fair cross-layout comparison.
        assert!(
            gpu_ns <= cpu_ns * 1.5 + 0.05,
            "GPU normalized stress {gpu_ns} regressed vs CPU s_gd2 {cpu_ns} (init {ns0})"
        );
    }

    #[test]
    fn handles_empty_and_singleton() {
        let mut ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_none() {
            eprintln!("Skipping sgd-stress-gpu test (no GPU)");
            return;
        }
        for n in [0u32, 1] {
            let g = CsrGraph::path(n);
            let positions = ring_positions(n as usize);
            let mut engine = SgdStressGpuEngine::new();
            let shard = CsrShard::whole(&g);
            engine.init(&mut ctx, &shard, &positions).expect("init");
            let out = engine.step(&mut ctx).positions;
            assert_eq!(out.len(), positions.len());
        }
    }
}
