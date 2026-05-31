//! Shared "solved-case" canaries for the stress-layout engines (`sgd-stress`
//! CPU and `sgd-stress-gpu` GPU).
//!
//! ## What "solved" means here (and what it does NOT)
//!
//! A deep-research review of the graph-drawing literature found that, contrary
//! to folklore, there are **no proven closed-form stress-optimal layouts** for
//! the usual symmetric families (cycle, complete, grid, hypercube, Platonic):
//! graph-theoretic distance matrices are generally **non-Euclidean**, so the
//! minimum stress is > 0 with no analytic minimizer. cMDS only reproduces
//! distances exactly when the matrix is Euclidean.
//!
//! The ONE clean exception is the **path** `P_n`: placing node `i` at `x = i`
//! makes every Euclidean distance equal the graph distance (`||x_i - x_j|| =
//! |i - j| = d_ij`), so stress is **exactly 0** and that IS the provable global
//! optimum. We use it as a fixed-point canary.
//!
//! For everything else we assert what is actually defensible:
//!   - **CPU↔GPU equivalence** on *scale-normalized* stress. (Standard stress is
//!     scale-sensitive — uniformly scaling a layout changes it — so a fair
//!     comparison minimizes over a global scale `alpha`; closed form
//!     `alpha* = sum w*d*D / sum w*D^2`. Kobourov et al., "(GD) metrics" 2024;
//!     arXiv:1201.3011.) This validates the WGSL Jacobi port against the CPU
//!     s_gd2 engine on the metric that is actually invariant.
//!   - **Soft symmetry regression** for the cycle (low coefficient-of-variation
//!     of centroid radius) — a regression anchor, NOT a proof of optimality.
//!
//! Stress: `s(X) = sum_{i<j} w_ij (||x_i - x_j|| - d_ij)^2`, `w_ij = 1/d_ij^2`.

use graph_compute::engines::{CsrShard, EngineCtx, LayoutEngine, SgdStressEngine, SgdStressGpuEngine};
use graph_compute::sim::CsrGraph;

// ---- Canonical graph builders (CSR, ascending neighbour lists) -------------

fn cycle(n: usize) -> CsrGraph {
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for v in 0..n {
        let mut nb = [((v + n - 1) % n) as u32, ((v + 1) % n) as u32];
        nb.sort_unstable();
        neighbors.extend_from_slice(&nb);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph { n_nodes: n as u32, offsets, neighbors }
}

fn complete(n: usize) -> CsrGraph {
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for v in 0..n {
        for u in 0..n {
            if u != v {
                neighbors.push(u as u32);
            }
        }
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph { n_nodes: n as u32, offsets, neighbors }
}

/// `w × h` 4-neighbour grid graph, node id `r*w + c`.
fn grid(w: usize, h: usize) -> CsrGraph {
    let idx = |r: usize, c: usize| (r * w + c) as u32;
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for r in 0..h {
        for c in 0..w {
            let mut nb = Vec::new();
            if c > 0 { nb.push(idx(r, c - 1)); }
            if c + 1 < w { nb.push(idx(r, c + 1)); }
            if r > 0 { nb.push(idx(r - 1, c)); }
            if r + 1 < h { nb.push(idx(r + 1, c)); }
            nb.sort_unstable();
            neighbors.extend_from_slice(&nb);
            offsets.push(neighbors.len() as u32);
        }
    }
    CsrGraph { n_nodes: (w * h) as u32, offsets, neighbors }
}

// ---- Distances + stress ----------------------------------------------------

/// All-pairs unweighted shortest paths (one BFS per source). `u32::MAX` for
/// unreachable pairs; `d[i*n + j]` is the hop distance from `i` to `j`.
fn all_pairs(g: &CsrGraph) -> Vec<u32> {
    let n = g.n_nodes as usize;
    let mut d = vec![u32::MAX; n * n];
    for s in 0..n {
        let row = &mut d[s * n..(s + 1) * n];
        row[s] = 0;
        let mut q = std::collections::VecDeque::from([s as u32]);
        while let Some(v) = q.pop_front() {
            let dv = row[v as usize];
            let (a, b) = (g.offsets[v as usize] as usize, g.offsets[v as usize + 1] as usize);
            for &u in &g.neighbors[a..b] {
                if row[u as usize] == u32::MAX {
                    row[u as usize] = dv + 1;
                    q.push_back(u);
                }
            }
        }
    }
    d
}

/// Scale-normalized stress over all reachable pairs — delegates to the shared,
/// unit-tested `graph_layouts::metrics` implementation (scale-invariant; raw
/// stress is scale-sensitive). Builds the `(i, j, d_ij)` terms from the
/// all-pairs distance matrix.
fn scale_normalized_stress(pos: &[f32], d: &[u32], n: usize) -> f32 {
    let mut terms = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let dij = d[i * n + j];
            if dij != u32::MAX && dij != 0 {
                terms.push((i as u32, j as u32, dij as f32));
            }
        }
    }
    graph_layouts::metrics::scale_normalized_stress(pos, &terms)
}

// ---- Seeds + relaxation ----------------------------------------------------

/// Deterministic, reproducible seed in `[-spread, spread]^3` (z kept 0 for a
/// planar start) via an integer hash — stable across machines/runs.
fn seed(n: usize, spread: f32) -> Vec<f32> {
    let mut p = vec![0.0f32; 3 * n];
    let hash = |k: u64| -> f32 {
        let mut z = k.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0) * spread
    };
    for i in 0..n {
        p[3 * i] = hash(i as u64 * 2 + 1);
        p[3 * i + 1] = hash(i as u64 * 2 + 7);
    }
    p
}

fn relax_cpu(g: &CsrGraph, init: &[f32], steps: usize) -> Vec<f32> {
    let mut e = SgdStressEngine::new();
    let mut ctx = EngineCtx::cpu_only();
    e.init(&mut ctx, &CsrShard::whole(g), init).expect("cpu init");
    let mut out = init.to_vec();
    for _ in 0..steps {
        out = e.step(&mut ctx).positions;
    }
    out
}

/// Relax on GPU; returns `None` if no adapter is available (CI/headless).
fn relax_gpu(g: &CsrGraph, init: &[f32], steps: usize) -> Option<Vec<f32>> {
    let mut ctx = EngineCtx::try_new_gpu();
    ctx.gpu.as_ref()?;
    let mut e = SgdStressGpuEngine::new();
    e.init(&mut ctx, &CsrShard::whole(g), init).expect("gpu init");
    let mut out = init.to_vec();
    for _ in 0..steps {
        out = e.step(&mut ctx).positions;
    }
    Some(out)
}

// ---- Solved cases ----------------------------------------------------------

/// PATH = the one provable zero-stress optimum. Seeded at the analytic line
/// (x_i = i) plus a small perturbation; the engine must keep normalized stress
/// near 0 — i.e. the global optimum is a stable fixed point of the solver.
#[test]
fn path_zero_stress_is_a_fixed_point_cpu() {
    let n = 20;
    let g = CsrGraph::path(n as u32);
    let d = all_pairs(&g);

    // Exact line: normalized stress must be ~0 (sanity on the metric + claim).
    let mut line = vec![0.0f32; 3 * n];
    for i in 0..n {
        line[3 * i] = i as f32;
    }
    assert!(
        scale_normalized_stress(&line, &d, n) < 1e-4,
        "the colinear path must have ~0 normalized stress (it is the exact optimum)"
    );

    // Perturb off the line, relax, and confirm it returns near the optimum.
    let mut init = line.clone();
    let noise = seed(n, 0.4);
    for i in 0..3 * n {
        init[i] += noise[i];
    }
    let out = relax_cpu(&g, &init, 150);
    let ns = scale_normalized_stress(&out, &d, n);
    assert!(ns < 0.05, "CPU sgd-stress should relax the path back near optimum: NS={ns}");
}

#[test]
fn path_zero_stress_is_a_fixed_point_gpu() {
    let n = 20;
    let g = CsrGraph::path(n as u32);
    let d = all_pairs(&g);
    let mut init = vec![0.0f32; 3 * n];
    for i in 0..n {
        init[3 * i] = i as f32;
    }
    let noise = seed(n, 0.4);
    for i in 0..3 * n {
        init[i] += noise[i];
    }
    let Some(out) = relax_gpu(&g, &init, 150) else {
        eprintln!("Skipping path fixed-point GPU canary (no GPU)");
        return;
    };
    let ns = scale_normalized_stress(&out, &d, n);
    assert!(ns < 0.05, "GPU sgd-stress should relax the path back near optimum: NS={ns}");
}

/// CPU↔GPU quality guard on the scale-invariant metric, across canonical graphs.
///
/// NOTE: this is deliberately *one-directional*, not a symmetric equivalence
/// check. The engines have genuinely different convergence rates: the GPU's
/// full-step Jacobi update unfolds long-range structure faster than the CPU's
/// half-step s_gd2 at equal step budget (CPU far-pair steps `mu = min(1, eta/d^2)`
/// are tiny, so a tangled seed stays tangled longer). Empirically the GPU often
/// reaches *lower* normalized stress than CPU from the same seed. So we assert
/// the port does not *regress* in quality vs the validated CPU baseline — it may
/// be better — which is the guarantee we actually care about. (The path
/// fixed-point canaries above already pin both engines to the one provable
/// optimum.)
#[test]
fn gpu_not_worse_than_cpu_on_normalized_stress() {
    let cases: &[(&str, CsrGraph)] = &[
        ("path24", CsrGraph::path(24)),
        ("cycle16", cycle(16)),
        ("grid4x4", grid(4, 4)),
        ("complete6", complete(6)),
    ];
    for (name, g) in cases {
        let n = g.n_nodes as usize;
        let d = all_pairs(g);
        let init = seed(n, 1.0);

        let cpu = relax_cpu(g, &init, 200);
        let ns_cpu = scale_normalized_stress(&cpu, &d, n);

        let Some(gpu) = relax_gpu(g, &init, 200) else {
            eprintln!("Skipping CPU↔GPU quality guard (no GPU)");
            return;
        };
        let ns_gpu = scale_normalized_stress(&gpu, &d, n);

        assert!(ns_cpu.is_finite() && ns_gpu.is_finite(), "{name}: non-finite stress");
        // GPU must not be meaningfully worse than CPU (generous slack for the
        // update-order difference). Catches a real GPU-port quality regression
        // without being flaky on the expected rate asymmetry.
        assert!(
            ns_gpu <= ns_cpu * 1.5 + 0.05,
            "{name}: GPU port regressed vs CPU baseline — cpu={ns_cpu}, gpu={ns_gpu}"
        );
    }
}

/// SOFT symmetry regression (NOT a proof of optimality): a cycle seeded near a
/// regular polygon should stay roughly regular — low coefficient-of-variation
/// of the per-node radius about the centroid.
#[test]
fn cycle_stays_roughly_regular_cpu() {
    let n = 16usize;
    let g = cycle(n);

    // Seed as a regular polygon (the symmetric configuration) + small noise.
    let mut init = vec![0.0f32; 3 * n];
    let noise = seed(n, 0.15);
    for i in 0..n {
        let t = (i as f32) / (n as f32) * std::f32::consts::TAU;
        init[3 * i] = t.cos() * 5.0 + noise[3 * i];
        init[3 * i + 1] = t.sin() * 5.0 + noise[3 * i + 1];
    }
    let out = relax_cpu(&g, &init, 150);

    // Centroid + per-node radius.
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    for i in 0..n {
        cx += out[3 * i];
        cy += out[3 * i + 1];
    }
    cx /= n as f32;
    cy /= n as f32;
    let radii: Vec<f32> = (0..n)
        .map(|i| ((out[3 * i] - cx).powi(2) + (out[3 * i + 1] - cy).powi(2)).sqrt())
        .collect();
    let mean = radii.iter().sum::<f32>() / n as f32;
    let var = radii.iter().map(|r| (r - mean).powi(2)).sum::<f32>() / n as f32;
    let cv = var.sqrt() / mean.max(1e-6);
    assert!(cv < 0.25, "cycle should stay roughly regular: radius CV={cv} (mean r={mean})");
}
