//! Property-based fuzz for the worker layout engines — the graph-compute
//! counterpart of `crates/graph-layouts/tests/fuzz.rs`, covering the three
//! axes the targeted suites can't: adversarial graph SHAPES (hub stars,
//! isolated nodes, self-loops, duplicate edges, disconnected components),
//! adversarial SEED positions (coincident points — the degenerate class that
//! NaN'd the old unit-ring placeholder — collinear lines, huge coordinates,
//! all-zeros), and the settings cross-product.
//!
//! The invariants are deliberately WEAKER than `tests/layout_stability.rs`:
//! under arbitrary fuzzed settings an engine may legitimately oscillate or
//! crawl, so convergence is NOT asserted here. What must hold for every
//! well-formed input is:
//!
//!   1. `init` accepts any well-formed CSR + any finite seed;
//!   2. every step returns exactly `3·n` coordinates, all finite;
//!   3. positions stay under the kinematic ceiling implied by the per-step
//!      displacement cap (catches the exponential-divergence class even when
//!      it hasn't reached NaN yet).
//!
//! Volume: proptest honors `PROPTEST_CASES` (the `just test fuzz [N]` knob).
//! GPU targets clamp their case count — adapter bring-up dominates their
//! runtime — so a high `N` buys CPU-engine coverage, not GPU churn.

use graph_compute::engines::{
    CpuSpringEngine, Fa2BhEngine, Fa2BruteEngine, GeometricEngine, SgdStressEngine,
};
use graph_compute::sim::CsrGraph;
use graph_compute::stability::{ball_seed, csr_from_pairs, synthetic_scale_free, SplitMix64};
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};
use proptest::prelude::*;

mod common;

/// Serialize GPU cases (concurrent wgpu device creation trips Metal
/// validation under load — same guard as tests/layout_stability.rs).
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// GPU fuzz case budget: env-respecting but capped — `just test fuzz 10000`
/// must not request 10k Metal adapters.
fn gpu_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(16)
        .min(48)
}

// ---- graph strategies --------------------------------------------------------

/// A well-formed CSR from raw (possibly degenerate) pairs: self-loops,
/// duplicates, and out-of-order endpoints are all legal inputs at this layer
/// (`csr_from_pairs` symmetrizes + dedups like the production loader).
fn raw_pairs_graph(n: u32, edge_seed: u64, m_per_node: usize) -> CsrGraph {
    let mut rng = SplitMix64(edge_seed);
    let mut pairs = Vec::with_capacity(n as usize * m_per_node);
    for v in 0..n {
        for _ in 0..m_per_node {
            // % n keeps endpoints in range; collisions create self-loops and
            // duplicates on purpose.
            let t = (rng.next_u64() % n as u64) as u32;
            pairs.push((v, t));
        }
    }
    csr_from_pairs(n, &pairs)
}

/// Star: node 0 is a hub of degree n-1 (the vault's hub-note profile, taken
/// to the extreme — maximizes single-node force accumulation).
fn star_graph(n: u32) -> CsrGraph {
    let pairs: Vec<(u32, u32)> = (1..n).map(|v| (0, v)).collect();
    csr_from_pairs(n, &pairs)
}

/// Two dense-ish components + a band of fully isolated nodes (the vault has
/// 49 of those; isolation is the no-attraction worst case for repulsion).
fn islands_graph(n: u32, edge_seed: u64) -> CsrGraph {
    let half = (n / 3).max(1);
    let mut rng = SplitMix64(edge_seed);
    let mut pairs = Vec::new();
    for v in 1..half {
        pairs.push((v, (rng.next_u64() % v as u64) as u32));
    }
    for v in (half + 1)..(2 * half) {
        pairs.push((v, half + (rng.next_u64() % (v - half) as u64) as u32));
    }
    // Nodes in [2·half, n) stay isolated.
    csr_from_pairs(n, &pairs)
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    Path,
    Star,
    ScaleFree,
    RawPairs,
    Islands,
    EdgeFree,
}

fn build_graph(shape: Shape, n: u32, seed: u64) -> CsrGraph {
    match shape {
        Shape::Path => {
            let pairs: Vec<(u32, u32)> = (1..n).map(|v| (v - 1, v)).collect();
            csr_from_pairs(n, &pairs)
        }
        Shape::Star => star_graph(n),
        Shape::ScaleFree => synthetic_scale_free(n, 3, seed),
        Shape::RawPairs => raw_pairs_graph(n, seed, 4),
        Shape::Islands => islands_graph(n, seed),
        Shape::EdgeFree => csr_from_pairs(n, &[]),
    }
}

fn any_shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        Just(Shape::Path),
        Just(Shape::Star),
        Just(Shape::ScaleFree),
        Just(Shape::RawPairs),
        Just(Shape::Islands),
        Just(Shape::EdgeFree),
    ]
}

/// CPU-engine sizes: skew small, tail into the thousands — the original fa2
/// divergence only manifested above the old suite's 1k ceiling, so the tail
/// is the point.
fn cpu_n() -> impl Strategy<Value = u32> {
    prop_oneof![
        1 => Just(1u32),
        1 => Just(2u32),
        5 => 3u32..=192,
        2 => 193u32..=1024,
        1 => 1025u32..=4096,
    ]
}

/// O(n²)-per-step CPU engines (geometric pair scan) get a lower ceiling so
/// high `PROPTEST_CASES` stays tractable.
fn quadratic_cpu_n() -> impl Strategy<Value = u32> {
    prop_oneof![
        1 => Just(1u32),
        5 => 3u32..=128,
        2 => 129u32..=512,
    ]
}

fn gpu_n() -> impl Strategy<Value = u32> {
    prop_oneof![
        1 => Just(1u32),
        3 => 8u32..=512,
        2 => 513u32..=4096,
    ]
}

// ---- seed strategies -----------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum SeedKind {
    /// The renderer convention — sane baseline.
    Ball,
    /// Every node at exactly the same point: 1/d² worst case (the class that
    /// NaN'd the old unit-ring placeholder).
    Coincident,
    /// Tight cluster at ~1e-4 spacing — coincident for f32 force math.
    EpsilonCluster,
    /// All on a line: degenerate octree extents.
    Collinear,
    /// Far from the origin at mixed magnitudes (~1e5): strong-gravity and
    /// far-field paths.
    Huge,
    Zeros,
}

fn build_seed(kind: SeedKind, n: usize, seed: u64) -> Vec<f32> {
    let mut rng = SplitMix64(seed);
    match kind {
        SeedKind::Ball => ball_seed(n, seed),
        SeedKind::Coincident => {
            let x = rng.next_f32() * 100.0 - 50.0;
            let y = rng.next_f32() * 100.0 - 50.0;
            let z = rng.next_f32() * 100.0 - 50.0;
            (0..n).flat_map(|_| [x, y, z]).collect()
        }
        SeedKind::EpsilonCluster => (0..n)
            .flat_map(|_| {
                [
                    rng.next_f32() * 1e-4,
                    rng.next_f32() * 1e-4,
                    rng.next_f32() * 1e-4,
                ]
            })
            .collect(),
        SeedKind::Collinear => (0..n)
            .flat_map(|i| [i as f32 * 0.5, 0.0, 0.0])
            .collect(),
        SeedKind::Huge => (0..n)
            .flat_map(|_| {
                [
                    (rng.next_f32() * 2.0 - 1.0) * 1e5,
                    (rng.next_f32() * 2.0 - 1.0) * 1e5,
                    (rng.next_f32() * 2.0 - 1.0) * 1e5,
                ]
            })
            .collect(),
        SeedKind::Zeros => vec![0.0; 3 * n],
    }
}

fn any_seed_kind() -> impl Strategy<Value = SeedKind> {
    prop_oneof![
        3 => Just(SeedKind::Ball),
        1 => Just(SeedKind::Coincident),
        1 => Just(SeedKind::EpsilonCluster),
        1 => Just(SeedKind::Collinear),
        1 => Just(SeedKind::Huge),
        1 => Just(SeedKind::Zeros),
    ]
}

// ---- settings strategies ---------------------------------------------------------

/// fa2 params. `max_displacement` stays > 0 (the production default): with
/// the cap disabled an adversarial scaling/gravity combination may
/// legitimately exceed any kinematic bound mid-flight; the cap-off regime is
/// covered at default settings by tests/layout_stability.rs.
fn fa2_params() -> impl Strategy<Value = serde_json::Value> {
    (
        0.0f32..=20.0,          // gravity
        any::<bool>(),          // strong_gravity
        0.01f32..=20.0,         // scaling_ratio
        0.0f32..=2.0,           // edge_weight_influence
        0.01f32..=10.0,         // jitter_tolerance
        any::<bool>(),          // lin_log_mode
        0.01f32..=2.0,          // time_step
        1.0f32..=50.0,          // max_displacement
    )
        .prop_map(|(g, sg, sc, ew, jt, ll, dt, cap)| {
            serde_json::json!({
                "gravity": g,
                "strong_gravity": sg,
                "scaling_ratio": sc,
                "edge_weight_influence": ew,
                "jitter_tolerance": jt,
                "lin_log_mode": ll,
                "time_step": dt,
                "max_displacement": cap,
            })
        })
}

/// Geometric lens-resolved params. `max_step` stays > 0: the clamp is the
/// engine's documented non-finite safety (and is always on in production —
/// the lens defaults it to 10).
fn geometric_params() -> impl Strategy<Value = serde_json::Value> {
    (
        0.0f32..=200.0,         // edge_stiffness
        0.0f32..=1.0,           // angle_stiffness
        0.0f32..=200.0,         // exclusion_strength
        0.0f32..=2.0,           // affinity_strength
        0.0f32..=0.1,           // gravity
        0.01f32..=1.0,          // time_step
        0.0f32..=1.0,           // damping
        0.5f32..=50.0,          // max_step
    )
        .prop_map(|(es, asf, ex, af, g, dt, d, ms)| {
            serde_json::json!({
                "edge_stiffness": es,
                "angle_stiffness": asf,
                "exclusion_strength": ex,
                "affinity_strength": af,
                "gravity": g,
                "time_step": dt,
                "damping": d,
                "max_step": ms,
            })
        })
}

fn cpu_spring_params() -> impl Strategy<Value = serde_json::Value> {
    (0.001f32..=0.5).prop_map(|dt| serde_json::json!({ "time_step": dt }))
}

// ---- the property ---------------------------------------------------------------

/// Init + step the engine over fuzzed inputs and hold the fuzz invariants.
/// `step_cap` is the engine's per-step displacement bound (its cap × any dt
/// multiplier), used for the kinematic ceiling; generous 2× slack — the
/// property targets explosions (orders of magnitude), not tight kinematics.
fn check_engine(
    engine: &mut dyn LayoutEngine,
    ctx: &mut EngineCtx,
    graph: &CsrGraph,
    seed: &[f32],
    steps: usize,
    step_cap: f32,
) -> Result<(), TestCaseError> {
    let n = graph.n_nodes as usize;
    engine
        .init(ctx, &CsrShard::whole(graph), seed)
        .map_err(|e| TestCaseError::fail(format!("init failed on well-formed input: {e}")))?;

    let seed_max = seed
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let ceiling = seed_max + (steps as f32 + 1.0) * step_cap * 2.0;

    for step in 1..=steps {
        let out = engine.step(ctx).positions;
        if out.len() != 3 * n {
            return Err(TestCaseError::fail(format!(
                "step {step}: output length {} != 3·n ({})",
                out.len(),
                3 * n
            )));
        }
        for (i, v) in out.iter().enumerate() {
            if !v.is_finite() {
                return Err(TestCaseError::fail(format!(
                    "step {step}: pos[{i}] = {v} non-finite"
                )));
            }
            if v.abs() > ceiling {
                return Err(TestCaseError::fail(format!(
                    "step {step}: pos[{i}] = {v} breaches kinematic ceiling {ceiling} \
                     (seed_max {seed_max}, cap {step_cap}) — divergence"
                )));
            }
        }
    }
    Ok(())
}

// ---- CPU engines: full fuzz volume ---------------------------------------------------

proptest! {
    /// cpu-spring: cheapest engine, biggest graphs.
    #[test]
    fn fuzz_cpu_spring(
        shape in any_shape(),
        n in cpu_n(),
        seed_kind in any_seed_kind(),
        params in cpu_spring_params(),
        graph_seed in any::<u64>(),
        steps in 5usize..=30,
    ) {
        let graph = build_graph(shape, n, graph_seed);
        let seed = build_seed(seed_kind, graph.n_nodes as usize, graph_seed ^ 0xA5);
        let mut engine = CpuSpringEngine::new();
        engine.set_params(&params).map_err(TestCaseError::fail)?;
        // Spring displacement is unclamped but dt ≤ 0.5 bounds it via the
        // spring force on the seed extent; use the seed scale as the cap.
        let cap = 1.0 + seed.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        check_engine(&mut engine, &mut EngineCtx::cpu_only(), &graph, &seed, steps, cap)?;
    }

    /// geometric (CPU): O(n²) pair scan per step — smaller n.
    #[test]
    fn fuzz_geometric_cpu(
        shape in any_shape(),
        n in quadratic_cpu_n(),
        seed_kind in any_seed_kind(),
        params in geometric_params(),
        graph_seed in any::<u64>(),
        steps in 5usize..=20,
    ) {
        let graph = build_graph(shape, n, graph_seed);
        let seed = build_seed(seed_kind, graph.n_nodes as usize, graph_seed ^ 0xA5);
        let cap = params["max_step"].as_f64().unwrap() as f32;
        let mut engine = GeometricEngine::new();
        engine.set_params(&params).map_err(TestCaseError::fail)?;
        check_engine(&mut engine, &mut EngineCtx::cpu_only(), &graph, &seed, steps, cap)?;
    }

    /// sgd-stress (CPU): BFS pivot init is the heavy part — moderate n.
    #[test]
    fn fuzz_sgd_stress_cpu(
        shape in any_shape(),
        n in quadratic_cpu_n(),
        seed_kind in any_seed_kind(),
        graph_seed in any::<u64>(),
        steps in 5usize..=20,
    ) {
        let graph = build_graph(shape, n, graph_seed);
        let seed = build_seed(seed_kind, graph.n_nodes as usize, graph_seed ^ 0xA5);
        let mut engine = SgdStressEngine::new();
        // SGD moves nodes toward graph-distance targets; bound by the larger
        // of the seed extent and the graph's diameter scale.
        let cap = 100.0 + seed.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        check_engine(&mut engine, &mut EngineCtx::cpu_only(), &graph, &seed, steps, cap)?;
    }
}

// ---- GPU engines: clamped case count -----------------------------------------------

macro_rules! gpu_fuzz {
    ($test_name:ident, $engine:ty, $label:literal) => {
        proptest! {
            #![proptest_config(ProptestConfig {
                cases: gpu_cases(),
                failure_persistence: None,
                ..ProptestConfig::default()
            })]
            #[test]
            fn $test_name(
                shape in any_shape(),
                n in gpu_n(),
                seed_kind in any_seed_kind(),
                params in fa2_params(),
                graph_seed in any::<u64>(),
                steps in 5usize..=25,
            ) {
                let _g = GPU_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
                let Some(mut ctx) = common::gpu_ctx_or_skip($label) else {
                    // No adapter on this host (hard-fails under
                    // GPU_PAGERANK_REQUIRE_ADAPTER, like the other GPU suites).
                    return Ok(());
                };
                let graph = build_graph(shape, n, graph_seed);
                let seed = build_seed(seed_kind, graph.n_nodes as usize, graph_seed ^ 0xA5);
                let cap = params["max_displacement"].as_f64().unwrap() as f32
                    * params["time_step"].as_f64().unwrap().max(1.0) as f32;
                let mut engine = <$engine>::new();
                engine.set_params(&params).map_err(TestCaseError::fail)?;
                check_engine(&mut engine, &mut ctx, &graph, &seed, steps, cap)?;
            }
        }
    };
}

gpu_fuzz!(fuzz_fa2_bh_gpu, Fa2BhEngine, "fa2-bh");
gpu_fuzz!(fuzz_fa2_brute_gpu, Fa2BruteEngine, "fa2-brute");
