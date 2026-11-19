//! GPU water-sheet layout (`water-sheet-gpu`) vs the deterministic CPU
//! reference (`graph_compute::water::cpu`).
//!
//! Needs a real wgpu adapter (Metal on the dev box). Gated exactly like
//! `gpu_engines.rs`: with no adapter the test SKIPS (passes), unless
//! `GPU_PAGERANK_REQUIRE_ADAPTER` is set (CI), in which case a missing adapter
//! is a hard failure (see `common::gpu_ctx_or_skip`).
//!
//! ```text
//! cargo test -p graph-compute --test gpu_water_layout -- --nocapture
//! ```
//!
//! Each case runs the SAME canonical seed through the CPU oracle and the GPU
//! kernels (naive 1-D kernel AND the shared-memory staged sheet kernel), then
//! asserts (a) GPU ≈ CPU within an f32-contraction tolerance and (b) the
//! geometry the layout exists to produce: lattice spacing and O–H bond lengths.

mod common;

use graph_compute::engines::WaterSheetGpuEngine;
use graph_compute::water::cpu::{
    geometry_report, max_abs_diff_64, max_abs_diff_f64, run_cpu, run_cpu_f64, run_cpu_f64pos,
    Positions, Positions64,
};
use graph_compute::water::{seed_positions, WaterPrecision, WaterSheetSettings};
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};

/// Serializes GPU tests in this binary — concurrent wgpu device creation trips
/// Metal validation errors under load.
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn gpu_guard() -> std::sync::MutexGuard<'static, ()> {
    GPU_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn run_gpu(ctx: &mut EngineCtx, s: &WaterSheetSettings, steps: u32) -> Positions {
    let p = s.params();
    let g = p.graph();
    let seed = seed_positions(&p, s);
    let mut e = WaterSheetGpuEngine::new();
    e.set_params(&serde_json::to_value(s).unwrap())
        .expect("set_params");
    e.init(ctx, &CsrShard::whole(&g), &seed).expect("gpu init");
    e.step_n(steps);
    Positions::from_interleaved(&e.read_positions())
}

fn max_abs_diff(a: &Positions, b: &Positions) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut m = 0.0f32;
    for i in 0..a.len() {
        m = m
            .max((a.x[i] - b.x[i]).abs())
            .max((a.y[i] - b.y[i]).abs())
            .max((a.z[i] - b.z[i]).abs());
    }
    m
}

fn small() -> WaterSheetSettings {
    WaterSheetSettings {
        molecules: 10,
        rows: 8,
        cols: 8,
        ..WaterSheetSettings::default()
    }
}

/// Short horizon: trajectories are still close to the seed, so GPU/CPU must
/// agree to near f32 round-off.
const SHORT_STEPS: u32 = 20;
const SHORT_TOL: f32 = 1e-3;
/// Long horizon: the damped system has converged, so contraction-order noise
/// has been damped out — compare loosely and assert the geometry directly.
const LONG_STEPS: u32 = 300;
const LONG_TOL: f32 = 2e-2;

#[test]
fn gpu_matches_cpu_and_relaxes_geometry() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("gpu_matches_cpu_and_relaxes_geometry") else {
        return;
    };
    // (staged, (r_sheet, r_mol)): the tuned default window and the original
    // wide one, each through both the naive and the staged sheet kernel.
    for (staged, (r_sheet, r_mol)) in [false, true]
        .into_iter()
        .flat_map(|st| [(st, (1, 1)), (st, (3, 2))])
    {
        let s = WaterSheetSettings {
            staged,
            r_sheet,
            r_mol,
            ..small()
        };
        eprintln!("--- staged={staged} r_sheet={r_sheet} r_mol={r_mol}");
        let cpu_short = run_cpu(&s, SHORT_STEPS as usize);
        let gpu_short = run_gpu(&mut ctx, &s, SHORT_STEPS);
        let d_short = max_abs_diff(&cpu_short, &gpu_short);
        eprintln!("staged={staged}: {SHORT_STEPS}-step max|gpu-cpu| = {d_short:e}");
        assert!(d_short < SHORT_TOL, "staged={staged}: short-horizon diff {d_short}");

        let cpu_long = run_cpu(&s, LONG_STEPS as usize);
        let gpu_long = run_gpu(&mut ctx, &s, LONG_STEPS);
        let d_long = max_abs_diff(&cpu_long, &gpu_long);
        eprintln!("staged={staged}: {LONG_STEPS}-step max|gpu-cpu| = {d_long:e}");
        assert!(d_long < LONG_TOL, "staged={staged}: long-horizon diff {d_long}");

        // Geometry asserts, on the GPU result itself (same tolerances as the
        // CPU test in water::cpu).
        let r = geometry_report(&gpu_long, &s);
        eprintln!("staged={staged}: gpu geometry {r:?}");
        assert!(
            gpu_long
                .x
                .iter()
                .chain(&gpu_long.y)
                .chain(&gpu_long.z)
                .all(|v| v.is_finite()),
            "staged={staged}: non-finite positions"
        );
        assert!(r.max_sheet_spacing_err < 0.15, "staged={staged}: sheet spacing {r:?}");
        assert!(r.max_oh_err < 0.05, "staged={staged}: O-H length {r:?}");
        assert!(r.max_lattice_offset < 0.5, "staged={staged}: lattice offset {r:?}");
    }
}

/// A sheet whose dimensions are NOT multiples of the 8×8 staged workgroup
/// (13×11) and a molecule count that is not a perfect square (7): exercises
/// the staged kernel's tile-edge masking and the molecule grid's ragged row.
#[test]
fn gpu_matches_cpu_on_ragged_shapes() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("gpu_matches_cpu_on_ragged_shapes") else {
        return;
    };
    for (staged, (r_sheet, r_mol)) in [false, true]
        .into_iter()
        .flat_map(|st| [(st, (1, 1)), (st, (3, 2))])
    {
        let s = WaterSheetSettings {
            molecules: 7,
            rows: 13,
            cols: 11,
            staged,
            r_sheet,
            r_mol,
            ..WaterSheetSettings::default()
        };
        let cpu = run_cpu(&s, SHORT_STEPS as usize);
        let gpu = run_gpu(&mut ctx, &s, SHORT_STEPS);
        let d = max_abs_diff(&cpu, &gpu);
        eprintln!("ragged staged={staged} r={r_sheet}/{r_mol}: max|gpu-cpu| = {d:e}");
        assert!(d < SHORT_TOL, "ragged staged={staged} r={r_sheet}/{r_mol}: diff {d}");
    }
}

/// Multiple steps per submit must equal the same number of single-step submits
/// (ping-pong parity is tracked across the batched pass).
#[test]
fn batched_steps_equal_single_steps() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("batched_steps_equal_single_steps") else {
        return;
    };
    let s = WaterSheetSettings {
        staged: true,
        ..small()
    };
    let p = s.params();
    let g = p.graph();
    let seed = seed_positions(&p, &s);
    let mut single = WaterSheetGpuEngine::new();
    single.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
    single.init(&mut ctx, &CsrShard::whole(&g), &seed).unwrap();
    for _ in 0..7 {
        single.step_n(1);
    }
    let a = Positions::from_interleaved(&single.read_positions());
    let b = run_gpu(&mut ctx, &s, 7);
    assert_eq!(a, b, "7×step_n(1) must be bit-identical to step_n(7)");
}

/// A bare graph push with NO `set_params` — the UI engine picker / graph-api
/// compute broker path: the engine must infer the waterSheet shape from the
/// CSR, fall back to the default tuning, and still match the CPU oracle.
#[test]
fn gpu_infers_shape_without_set_params() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("gpu_infers_shape_without_set_params") else {
        return;
    };
    // `small()` is (10, 8x8) with default physics — exactly what the engine
    // reconstructs from the inferred shape plus `WaterSheetSettings::default()`.
    let s = small();
    let p = s.params();
    let g = p.graph();
    let seed = seed_positions(&p, &s);
    let cpu = run_cpu(&s, 1);

    let mut e = WaterSheetGpuEngine::new();
    e.init(&mut ctx, &CsrShard::whole(&g), &seed)
        .expect("init without set_params");
    e.step_n(1);
    let gpu = Positions::from_interleaved(&e.read_positions());
    let d = max_abs_diff(&cpu, &gpu);
    eprintln!("inferred-shape 1-step max|gpu-cpu| = {d:e}");
    assert!(
        gpu.x.iter().chain(&gpu.y).chain(&gpu.z).all(|v| v.is_finite()),
        "non-finite positions after inferred-shape step"
    );
    assert!(d < SHORT_TOL, "inferred-shape diff {d}");

    // A `set_params` shape that does NOT match the graph: the shape is
    // re-inferred from the CSR while the tuning knobs are kept.
    let wrong_shape = WaterSheetSettings {
        molecules: 1000,
        rows: 64,
        cols: 64,
        ..small()
    };
    let mut e2 = WaterSheetGpuEngine::new();
    e2.set_params(&serde_json::to_value(&wrong_shape).unwrap())
        .expect("set_params");
    e2.init(&mut ctx, &CsrShard::whole(&g), &seed)
        .expect("init with a mismatched set_params shape");
    e2.step_n(1);
    let gpu2 = Positions::from_interleaved(&e2.read_positions());
    let d2 = max_abs_diff(&cpu, &gpu2);
    eprintln!("mismatched-shape 1-step max|gpu-cpu| = {d2:e}");
    assert!(d2 < SHORT_TOL, "mismatched-shape diff {d2}");
}

// ---------------------------------------------------------------------------
// f64 precision modes (`WaterPrecision::{F32, F64Pos, F64}`).
// ---------------------------------------------------------------------------

/// Whether the device exposes native WGSL f64 (`SHADER_F64`). `F64` only runs
/// when true; `F64Pos` needs no feature and never depends on it.
fn f64_native(ctx: &EngineCtx) -> bool {
    ctx.gpu
        .as_ref()
        .map(|g| g.device.features().contains(wgpu::Features::SHADER_F64))
        .unwrap_or(false)
}

/// Run `steps` on the GPU and return the FULL f64 position state. The `F64Pos`
/// mode decodes its `(integer, fraction)` fixed-point words in f64, so the
/// 2^-40 position quantum is not collapsed to a single f32 by the readback.
fn run_gpu_f64(ctx: &mut EngineCtx, s: &WaterSheetSettings, steps: u32) -> Positions64 {
    let p = s.params();
    let g = p.graph();
    let seed = seed_positions(&p, s);
    let mut e = WaterSheetGpuEngine::new();
    e.set_params(&serde_json::to_value(s).unwrap())
        .expect("set_params");
    e.init(ctx, &CsrShard::whole(&g), &seed).expect("gpu init");
    e.step_n(steps);
    Positions64::from_interleaved(&e.read_positions_f64())
}

/// Initialize the engine and return the error string; panics if init succeeds.
fn gpu_init_error(ctx: &mut EngineCtx, s: &WaterSheetSettings) -> String {
    let p = s.params();
    let g = p.graph();
    let seed = seed_positions(&p, s);
    let mut e = WaterSheetGpuEngine::new();
    e.set_params(&serde_json::to_value(s).unwrap())
        .expect("set_params");
    match e.init(ctx, &CsrShard::whole(&g), &seed) {
        Ok(()) => panic!("init unexpectedly succeeded for precision {:?}", s.precision),
        Err(err) => err,
    }
}

/// Short-horizon tolerance against the FULL f64 oracle: the f64 modes still
/// compute forces in f32 (`F64Pos`) or f64 (`F64`), so this bound is the
/// f32-force contraction, not the position-storage precision.
const F64_SHORT_TOL: f64 = 1e-4;
/// Long-horizon (300-step, converged) bound against the full f64 oracle.
const F64_LONG_TOL: f64 = 1e-3;
/// GPU `F64Pos` vs its own CPU oracle (`step_cpu_f64pos`, the same f32-force /
/// f32-velocity / high-precision-position operation sequence). The remaining
/// gap is GPU-vs-CPU force rounding (Metal contracts/reassociates the f32 dot
/// products, so the two implementations differ by ~1 ulp), amplified by the
/// transient: measured 2e-7 at 20 steps, 1e-6 at 300.
const F64POS_ORACLE_TOL: f64 = 1e-5;

/// Per-mode oracle test on the canonical (10, 8×8) shape, naive and staged
/// kernels. `F64Pos` is compared in f64 against its own oracle and against the
/// full-f64 oracle; `F64` runs natively only when `SHADER_F64` is exposed and
/// otherwise must reject the mode with a named error.
#[test]
fn f64_precision_modes_match_their_oracles() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("f64_precision_modes_match_their_oracles")
    else {
        return;
    };
    let native = f64_native(&ctx);
    eprintln!("SHADER_F64 on device: {native}");

    for staged in [false, true] {
        let base = WaterSheetSettings {
            staged,
            ..small()
        };

        // F32 baseline (f32 CPU oracle), unchanged behaviour.
        let cpu32 = run_cpu(&base, SHORT_STEPS as usize);
        let gpu32 = run_gpu(&mut ctx, &base, SHORT_STEPS);
        let d32 = max_abs_diff(&cpu32, &gpu32);
        eprintln!("f32 staged={staged}: {SHORT_STEPS}-step max|gpu-cpu| = {d32:e}");
        assert!(d32 < SHORT_TOL, "f32 staged={staged}: {d32:e}");

        // F64Pos: 64-bit fixed-point positions (2^-40 quantum), f32 forces.
        let s_pos = WaterSheetSettings {
            precision: WaterPrecision::F64Pos,
            ..base.clone()
        };
        let oracle_pos = run_cpu_f64pos(&s_pos, SHORT_STEPS as usize);
        let gpu_pos = run_gpu_f64(&mut ctx, &s_pos, SHORT_STEPS);
        let d_pos = max_abs_diff_64(&gpu_pos, &oracle_pos);
        eprintln!("f64pos staged={staged}: {SHORT_STEPS}-step max|gpu-oracle| (f64) = {d_pos:e}");
        assert!(
            d_pos < F64POS_ORACLE_TOL,
            "f64pos staged={staged}: gpu vs own oracle {d_pos:e}"
        );

        // It must also track the TRUE full-f64 oracle at f32-force precision.
        let d_true_short = max_abs_diff_64(&gpu_pos, &run_cpu_f64(&s_pos, SHORT_STEPS as usize));
        eprintln!(
            "f64pos staged={staged}: {SHORT_STEPS}-step vs full f64 oracle = {d_true_short:e}"
        );
        assert!(
            d_true_short < F64_SHORT_TOL,
            "f64pos staged={staged} vs full oracle: {d_true_short:e}"
        );

        // Long horizon: the damped layout converges; the fixed-point state
        // must stay on its oracle and well inside the f32 long-horizon bound.
        let oracle_pos_long = run_cpu_f64pos(&s_pos, LONG_STEPS as usize);
        let gpu_pos_long = run_gpu_f64(&mut ctx, &s_pos, LONG_STEPS);
        let d_pos_long = max_abs_diff_64(&gpu_pos_long, &oracle_pos_long);
        eprintln!("f64pos staged={staged}: {LONG_STEPS}-step max|gpu-oracle| (f64) = {d_pos_long:e}");
        assert!(
            d_pos_long < F64POS_ORACLE_TOL,
            "f64pos long staged={staged}: {d_pos_long:e}"
        );

        // Geometry on the f64 result itself (same bounds as the f32 test).
        let r = geometry_report(&gpu_pos_long.to_f32(), &s_pos);
        eprintln!("f64pos long staged={staged}: geometry {r:?}");
        assert!(r.max_sheet_spacing_err < 0.15, "staged={staged}: {r:?}");
        assert!(r.max_oh_err < 0.05, "staged={staged}: {r:?}");
        assert!(r.max_lattice_offset < 0.5, "staged={staged}: {r:?}");

        // F64: native f64 kernel only where the adapter exposes SHADER_F64;
        // otherwise `init` must hard-error naming the mode (no silent
        // precision downgrade — measurable on Apple GPUs, which have no fp64).
        let s64 = WaterSheetSettings {
            precision: WaterPrecision::F64,
            ..base.clone()
        };
        if native {
            let gpu64 = run_gpu_f64(&mut ctx, &s64, SHORT_STEPS);
            let d64 = max_abs_diff_64(&gpu64, &run_cpu_f64(&s64, SHORT_STEPS as usize));
            eprintln!("f64 (native) staged={staged}: {SHORT_STEPS}-step = {d64:e}");
            assert!(d64 < F64_SHORT_TOL, "f64 native staged={staged}: {d64:e}");

            let gpu64_long = run_gpu_f64(&mut ctx, &s64, LONG_STEPS);
            let d64_long =
                max_abs_diff_64(&gpu64_long, &run_cpu_f64(&s64, LONG_STEPS as usize));
            eprintln!("long staged={staged}: f64 {LONG_STEPS}-step = {d64_long:e}");
            assert!(d64_long < F64_LONG_TOL, "f64 long staged={staged}: {d64_long:e}");
        } else {
            let err = gpu_init_error(&mut ctx, &s64);
            eprintln!("f64 staged={staged}: unsupported on this adapter: {err}");
            assert!(
                err.contains("'f64'") && err.contains("SHADER_F64"),
                "f64 rejection must name the mode and the missing feature: {err}"
            );
        }
    }
}

/// Drift demonstration over 10,000 steps on a modest shape (100 molecules +
/// 16×16 sheet = 556 nodes), comparing the FULL f64 GPU state against the
/// full-f64 CPU oracle.
///
/// Measured on this box, 10,000 steps, absolute (positions are O(1)):
///   f32 state vs the full-f64 oracle:      1.36e-3 — per-step f32 position
///     rounding accumulates (the drift the mode exists to remove)
///   F64Pos state vs the full-f64 oracle:   1.46e-5 — 93x tighter; what is
///     left is the f32 *force* model plus GPU-vs-CPU dot-product rounding
///     (~1 ulp), not position-storage drift (growth: 6.8e-7 at 100 steps,
///     1.6e-6 at 1k, 3.1e-6 at 3k, 1.4e-5 at 10k)
///   F64Pos state vs its own CPU oracle:    1.43e-5 — same cause
///
/// `F64POS_DRIFT_MAX` sits between the two bands with margin, and the explicit
/// 20x ratio assertion keeps the test discriminating: a silent f32 fallback
/// (or a readback collapse) lands in the f32 band and fails both checks.
const F32_DRIFT_MIN: f64 = 1e-4;
const F64POS_DRIFT_MAX: f64 = 5e-5;
/// Minimum f32/f64pos drift ratio the demonstration must show.
const MIN_DRIFT_RATIO: f64 = 20.0;

#[test]
fn f32_drifts_while_f64_modes_track_the_oracle() {
    let _g = gpu_guard();
    let Some(mut ctx) = common::gpu_ctx_or_skip("f32_drifts_while_f64_modes_track_the_oracle")
    else {
        return;
    };
    let base = WaterSheetSettings {
        molecules: 100,
        rows: 16,
        cols: 16,
        ..WaterSheetSettings::default()
    };
    const STEPS: u32 = 10_000;
    let oracle = run_cpu_f64(&base, STEPS as usize);

    let s32 = WaterSheetSettings {
        precision: WaterPrecision::F32,
        ..base.clone()
    };
    let s_pos = WaterSheetSettings {
        precision: WaterPrecision::F64Pos,
        ..base.clone()
    };
    let s64 = WaterSheetSettings {
        precision: WaterPrecision::F64,
        ..base.clone()
    };

    // F32 is read back as f32 (its whole state is f32), widened for the diff.
    let gpu32 = run_gpu(&mut ctx, &s32, STEPS);
    let e32 = max_abs_diff_f64(&gpu32, &oracle);

    // F64Pos is read back as f64: the fixed-point state, not its f32 shadow.
    let gpu_pos = run_gpu_f64(&mut ctx, &s_pos, STEPS);
    let e_pos = max_abs_diff_64(&gpu_pos, &oracle);
    let e_pos_oracle = max_abs_diff_64(&gpu_pos, &run_cpu_f64pos(&s_pos, STEPS as usize));

    // F64: native where supported; otherwise the mode must be rejected by name.
    let native = f64_native(&ctx);
    let e64 = if native {
        let g = run_gpu_f64(&mut ctx, &s64, STEPS);
        Some(max_abs_diff_64(&g, &run_cpu_f64(&s64, STEPS as usize)))
    } else {
        let err = gpu_init_error(&mut ctx, &s64);
        assert!(
            err.contains("'f64'") && err.contains("SHADER_F64"),
            "f64 rejection must name the mode and the missing feature: {err}"
        );
        eprintln!("f64: unsupported on this adapter ({err})");
        None
    };

    eprintln!(
        "{STEPS}-step drift vs full-f64 oracle (556 nodes, SHADER_F64={native}): \
         f32={e32:e} f64pos={e_pos:e} f64pos-vs-own-oracle={e_pos_oracle:e} \
         f64={e64:?}"
    );

    assert!(
        e32 > F32_DRIFT_MIN,
        "f32 must visibly drift from the f64 oracle: {e32:e} <= {F32_DRIFT_MIN:e}"
    );
    assert!(
        e_pos < F64POS_DRIFT_MAX,
        "f64pos must track the f64 oracle: {e_pos:e} >= {F64POS_DRIFT_MAX:e}"
    );
    assert!(
        e32 / e_pos.max(f64::MIN_POSITIVE) > MIN_DRIFT_RATIO,
        "f32 drift must exceed f64pos drift by >{MIN_DRIFT_RATIO}x: {e32:e} / {e_pos:e}"
    );
    assert!(
        e_pos_oracle < F64POS_DRIFT_MAX,
        "f64pos must mirror its own oracle: {e_pos_oracle:e} >= {F64POS_DRIFT_MAX:e}"
    );
    if let Some(e64) = e64 {
        assert!(
            e64 < F64POS_DRIFT_MAX,
            "f64 must track the f64 oracle: {e64:e} >= {F64POS_DRIFT_MAX:e}"
        );
    }
}
