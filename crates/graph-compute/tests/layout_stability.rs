//! Vault-scale stability regression tests for the remote layout engines.
//!
//! Locks down the two failure modes measured on the real vault graph
//! (9,724 nodes / ~48k edges) before the 2026-06 fixes:
//!
//!  * **fa2 divergence** — the original Euler `vel=(vel+force)·0.5` integrator
//!    grew positions ~37×/step from a `√n·5`-radius random ball and hit NaN by
//!    step 23. Fixed by porting the paper's adaptive global speed (Jacomy,
//!    Venturini, Heymann, Bastian 2014, PLOS ONE; Gephi's reference
//!    ForceAtlas2.java — see `engines::fa2_speed`).
//!  * **geometric flip-flop** — at vault stiffness the damped-Euler update
//!    overshoots (K·dt² ≫ 2) and the `max_step` clamp turned divergence into a
//!    ±10-unit limit cycle (median displacement pinned at the clamp forever).
//!    Fixed by exposing `time_step`/`damping`/`max_step` on the lens; the
//!    validated preset (dt=0.1, damping=0.6) converges monotonically on both
//!    the CPU and GPU geometric engines.
//!
//! CI-safe: the graphs are seeded synthetic scale-free fixtures
//! (`stability::synthetic_scale_free`) so nothing depends on the vault; the
//! GPU tests skip cleanly without an adapter. A companion test additionally
//! runs against the real vault snapshot when `/tmp/jc-edges.bin` is present
//! (and skips otherwise). The interactive measurement tool is the
//! `graph-layout-stability` bin.

use graph_compute::engines::{Fa2BhEngine, Fa2BruteEngine, GeometricEngine, GeometricGpuEngine};
use graph_compute::sim::CsrGraph;
use graph_compute::stability::{ball_seed, csr_from_pair_file, synthetic_scale_free, StepStats};
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};

mod common;

/// Serialize GPU tests in this binary (concurrent wgpu device creation trips
/// Metal validation under load).
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn gpu_guard() -> std::sync::MutexGuard<'static, ()> {
    GPU_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Run `engine` for `steps` ticks from the vault-convention random-ball seed,
/// collecting per-step telemetry. Panics on any non-finite coordinate (the
/// pre-fix fa2 failure mode).
fn run_collect(
    engine: &mut dyn LayoutEngine,
    ctx: &mut EngineCtx,
    graph: &CsrGraph,
    steps: usize,
) -> Vec<StepStats> {
    let n = graph.n_nodes as usize;
    let seed = ball_seed(n, 0x5EED);
    engine
        .init(ctx, &CsrShard::whole(graph), &seed)
        .expect("engine init");
    let mut prev = seed;
    let mut stats = Vec::with_capacity(steps);
    for step in 1..=steps {
        let out = engine.step(ctx).positions;
        let st = StepStats::measure(&prev, &out);
        assert_eq!(
            st.nonfinite, 0,
            "non-finite positions at step {step} (max_abs so far {})",
            st.max_abs
        );
        stats.push(st);
        prev = out;
    }
    stats
}

/// Median of the per-step MEDIAN displacement over a window — robust to the
/// handful of isolated nodes that legitimately drift toward FA2's far-field
/// gravity/repulsion equilibrium.
fn window_disp(stats: &[StepStats], range: std::ops::Range<usize>) -> f32 {
    let mut v: Vec<f32> = stats[range].iter().map(|s| s.p50_disp).collect();
    v.sort_unstable_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Shared assertion: no exponential growth (the pre-fix signature was ×37 per
/// step), the connected bulk settles, and the median displacement decays after
/// warm-up instead of oscillating.
///
/// Two deliberate allowances, both measured on the real vault:
///  * `max_abs` is checked against the KINEMATIC ceiling (seed + steps·cap),
///    not a tight bound — a handful of isolated/near-isolated nodes
///    legitimately drift at the displacement cap toward FA2's far-field
///    gravity⇔repulsion equilibrium (r* = k_r·Σ(deg+1)/k_g ≈ 2·10⁵ at vault
///    parameters; reference Gephi behaves identically for orphans). The bulk
///    bound is asserted on the MEDIAN radius instead.
///  * the decay assertion has an absolute "already settled" escape so a run
///    that converges before the early window doesn't fail the ratio test.
fn assert_stable(name: &str, stats: &[StepStats], seed_radius: f32, max_displacement: f32) {
    let steps = stats.len();
    let last = stats.last().unwrap();

    // Divergence detector: nothing may move faster than the per-step cap, so
    // any breach of the kinematic ceiling means the integrator exploded
    // (pre-fix: ceiling broken ~250× over by step 2, 1e38 by step 23).
    let ceiling = seed_radius + steps as f32 * max_displacement;
    assert!(
        last.max_abs <= 1.01 * ceiling,
        "{name}: max |pos| {} breaches the kinematic ceiling {} — divergence",
        last.max_abs,
        ceiling
    );

    // The connected bulk must neither run away nor collapse to a point.
    assert!(
        last.p50_radius > 1.0 && last.p50_radius < 0.25 * ceiling,
        "{name}: bulk median radius {} degenerate (ceiling {})",
        last.p50_radius,
        ceiling
    );

    // Convergence, not oscillation: median displacement decays across thirds
    // (small tolerance for noise) and ends either much lower than it started
    // or at an absolute settled floor.
    let early = window_disp(stats, steps / 10..steps / 3);
    let mid = window_disp(stats, steps / 3..2 * steps / 3);
    let late = window_disp(stats, 8 * steps / 10..steps);
    assert!(
        late <= mid * 1.15 && mid <= early * 1.15,
        "{name}: displacement not decaying (early {early}, mid {mid}, late {late})"
    );
    assert!(
        late < 0.5 * early || late < 0.05,
        "{name}: displacement barely decayed (early {early} -> late {late}) — \
         flip-flop/limit-cycle signature"
    );
}

/// fa2-bh at vault scale on a synthetic scale-free graph (same n/m profile as
/// the vault). Pre-fix: NaN by step 23.
#[test]
fn fa2_bh_stable_at_vault_scale_synthetic() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("fa2_bh_stable_at_vault_scale_synthetic") else {
        return;
    };
    let graph = synthetic_scale_free(9724, 5, 0xC0FFEE);
    let n = graph.n_nodes as usize;
    let stats = run_collect(&mut Fa2BhEngine::new(), &mut ctx, &graph, 1000);
    assert_stable(
        "fa2-bh/synthetic-10k",
        &stats,
        (n as f32).sqrt() * 5.0,
        10.0,
    );
}

/// fa2-brute shares the force math + integrator; validate on a smaller
/// synthetic graph (its per-step cost is O(n·m), too slow at 10k for CI).
#[test]
fn fa2_brute_stable_on_synthetic_scale_free() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("fa2_brute_stable_on_synthetic_scale_free") else {
        return;
    };
    let graph = synthetic_scale_free(2000, 5, 0xC0FFEE);
    let n = graph.n_nodes as usize;
    let stats = run_collect(&mut Fa2BruteEngine::new(), &mut ctx, &graph, 700);
    assert_stable(
        "fa2-brute/synthetic-2k",
        &stats,
        (n as f32).sqrt() * 5.0,
        10.0,
    );
}

/// fa2-bh on the REAL vault topology (flat u32-LE edge pairs snapshot). Skips
/// when the snapshot isn't present so CI stays hermetic.
#[test]
fn fa2_bh_stable_on_real_vault_snapshot() {
    let _g = gpu_guard();
    let path = "/tmp/jc-edges.bin";
    if !std::path::Path::new(path).exists() {
        eprintln!("SKIP fa2_bh_stable_on_real_vault_snapshot: {path} not present");
        return;
    }
    let Some(mut ctx) = common::gpu_ctx_or_skip("fa2_bh_stable_on_real_vault_snapshot") else {
        return;
    };
    let graph = csr_from_pair_file(path).expect("load vault pair snapshot");
    let n = graph.n_nodes as usize;
    eprintln!(
        "vault snapshot: {} nodes / {} adjacency entries",
        n,
        graph.neighbors.len()
    );
    let stats = run_collect(&mut Fa2BhEngine::new(), &mut ctx, &graph, 1000);
    assert_stable("fa2-bh/vault", &stats, (n as f32).sqrt() * 5.0, 10.0);
}

/// The lens-resolved geometric force knobs that drove the vault flip-flop
/// (LensConfig defaults), as engine-settings JSON.
fn vault_lens_force_params(time_step: f32, damping: f32, max_step: f32) -> serde_json::Value {
    serde_json::json!({
        "edge_stiffness": 0.1,
        "angle_stiffness": 0.05,
        "exclusion_strength": 100.0,
        "affinity_strength": 0.0,
        "gravity": 0.005,
        "time_step": time_step,
        "damping": damping,
        "max_step": max_step,
    })
}

/// geometric-gpu at vault scale with the validated integrator preset
/// (dt=0.1, damping=0.6 — the LensPreset values). Pre-fix (dt=1, damping=.9)
/// the median displacement stayed pinned near the ±10 clamp indefinitely.
#[test]
fn geometric_gpu_converges_with_preset_integrator_at_vault_scale() {
    let _g = gpu_guard();
    let Some(mut ctx) =
        common::gpu_ctx_or_skip("geometric_gpu_converges_with_preset_integrator_at_vault_scale")
    else {
        return;
    };
    let graph = synthetic_scale_free(9724, 5, 0xC0FFEE);
    let n = graph.n_nodes as usize;
    let mut engine = GeometricGpuEngine::new();
    engine
        .set_params(&vault_lens_force_params(0.1, 0.6, 10.0))
        .expect("set_params");
    let stats = run_collect(&mut engine, &mut ctx, &graph, 600);
    assert_stable(
        "geometric-gpu/preset-integrator",
        &stats,
        (n as f32).sqrt() * 5.0,
        10.0,
    );
    // Stronger than the generic decay bar: the residual motion must be tiny
    // (the flip-flop regime plateaued at ~1.3).
    let late = window_disp(&stats, 500..600);
    assert!(
        late < 0.1,
        "geometric-gpu preset integrator should settle (late median disp {late})"
    );
}

/// The CPU geometric engine shares `GeometricSettings`; validate the same
/// preset on a smaller synthetic graph (CPU pair scan is O(n²)/step). Runs
/// everywhere — no GPU needed — so CI always exercises the knob path.
#[test]
fn geometric_cpu_converges_with_preset_integrator() {
    let graph = synthetic_scale_free(600, 5, 0xC0FFEE);
    let n = graph.n_nodes as usize;
    let mut ctx = EngineCtx::cpu_only();
    let mut engine = GeometricEngine::new();
    engine
        .set_params(&vault_lens_force_params(0.1, 0.6, 10.0))
        .expect("set_params");
    let stats = run_collect(&mut engine, &mut ctx, &graph, 500);
    assert_stable(
        "geometric-cpu/preset-integrator",
        &stats,
        (n as f32).sqrt() * 5.0,
        10.0,
    );
}
