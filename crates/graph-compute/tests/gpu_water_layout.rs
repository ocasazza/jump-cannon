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
use graph_compute::water::cpu::{geometry_report, run_cpu, Positions};
use graph_compute::water::{seed_positions, WaterSheetSettings};
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
