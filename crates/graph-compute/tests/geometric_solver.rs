//! Geometric constraint engine — validation, regression & performance harness.
//!
//! The geometric engine minimises a potential energy (edge springs + angle
//! constraints + class exclusion + gravity). This file is the "is the solver
//! actually working?" framework for it, in the spirit of how molecular / FEP
//! engines are validated: every check is built on ONE observable —
//! [`GeometricEngine::observe`], which reports the decomposed potential energy
//! and the residual force `‖∇E‖`. At a *solved* (equilibrium) layout the
//! residual → 0 and the potential sits at a local minimum.
//!
//! Three complementary layers:
//!
//!   1. **CANARY (solved cases).** A library of "known problems" whose
//!      equilibrium is known *analytically* — a single spring → rest length; a
//!      spring under gravity → the closed-form balance `d* = 2kL/(2k+gm)`; three
//!      equal springs → an equilateral triangle; a 4-cycle with a 90° angle → a
//!      square; K4 with equal springs → a regular tetrahedron (3D). Chosen so
//!      the set collectively pins down every force term (edge, gravity, angle)
//!      in 2D and 3D. The engine MUST relax to each known solution within
//!      tolerance — the loud, fast "the solver is broken" alarm.
//!   2. **REGRESSION (golden master).** A fixed scenario run for a fixed number
//!      of steps from a fixed seed; robust scalars of the final state (energy,
//!      residual, radius of gyration) are compared against a committed golden
//!      file. Drift beyond tolerance fails. Regenerate with
//!      `UPDATE_GEOMETRIC_GOLDEN=1` (a first run with no golden writes one).
//!   3. **PERFORMANCE.** Throughput (steps/sec) and steps-to-convergence on
//!      fixed graphs, asserted against *generous* budgets so a real algorithmic
//!      or complexity regression trips the test without it being timing-flaky.

use graph_compute::engines::geometric::{CoordinationSource, GeometricEngine, GeometricSettings};
use graph_compute::engines::{
    CsrShard, EngineCtx, GeometricGpuEngine, GraphAttributes, LayoutEngine,
};
use graph_compute::sim::CsrGraph;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A relaxation scenario: a graph, the engine settings, and a seed layout.
struct Scenario {
    name: &'static str,
    graph: CsrGraph,
    settings: GeometricSettings,
    seed: Vec<f32>,
}

/// One sample along a relaxation trajectory (taken via `observe`, no mutation).
#[derive(Clone, Copy, Debug)]
struct Sample {
    step: usize,
    potential: f32,
    kinetic: f32,
    max_residual: f32,
}

impl Sample {
    /// Total mechanical energy — the quantity a damped system must shed.
    fn total(&self) -> f32 {
        self.potential + self.kinetic
    }
}

struct RelaxResult {
    final_positions: Vec<f32>,
    trajectory: Vec<Sample>,
    /// First sampled step at which `max_residual < residual_tol`, if reached.
    converged_at: Option<usize>,
    wall: Duration,
}

/// Relax `scn` for up to `max_steps`, sampling the observable every
/// `sample_every` steps. Stops early the first time the residual drops below
/// `residual_tol` (pass `0.0` to disable early-stop and always run the budget).
/// Drives the engine through its real param path (`set_params` with serialized
/// settings) so the harness exercises the same entry point the wire uses.
fn relax(scn: &Scenario, max_steps: usize, residual_tol: f32, sample_every: usize) -> RelaxResult {
    let mut engine = GeometricEngine::new();
    engine
        .set_params(&serde_json::to_value(&scn.settings).expect("serialize settings"))
        .expect("set_params");
    let mut ctx = EngineCtx::cpu_only();
    let shard = CsrShard::whole(&scn.graph);
    engine.init(&mut ctx, &shard, &scn.seed).expect("init");

    let sample_every = sample_every.max(1);
    let mut trajectory = Vec::new();
    let mut converged_at = None;
    let mut final_positions = scn.seed.clone();

    let t0 = Instant::now();
    for step in 0..max_steps {
        final_positions = engine.step(&mut ctx).positions;
        let is_last = step + 1 == max_steps;
        if step % sample_every == 0 || is_last {
            let o = engine.observe().expect("observe after init");
            trajectory.push(Sample {
                step,
                potential: o.potential,
                kinetic: o.kinetic,
                max_residual: o.max_residual,
            });
            if converged_at.is_none() && residual_tol > 0.0 && o.max_residual < residual_tol {
                converged_at = Some(step);
                break;
            }
        }
    }
    RelaxResult {
        final_positions,
        trajectory,
        converged_at,
        wall: t0.elapsed(),
    }
}

/// Settings with every non-spring force off: pure edge-length springs, no
/// gravity. Used by the analytical canaries so their equilibrium is *exact*
/// (gravity would pull the structure toward the origin and shift it). Damping is
/// raised (lower value = more friction) so the canaries settle briskly instead
/// of ringing for thousands of steps.
fn springs_only(rest_len: f32) -> GeometricSettings {
    GeometricSettings {
        edge_rest_len: rest_len,
        edge_stiffness: 0.3,
        angle_stiffness: 0.0,
        exclusion_strength: 0.0,
        affinity_strength: 0.0,
        gravity: 0.0,
        damping: 0.6,
        ..GeometricSettings::default()
    }
}

fn dist(pos: &[f32], i: usize, j: usize) -> f32 {
    let dx = pos[3 * j] - pos[3 * i];
    let dy = pos[3 * j + 1] - pos[3 * i + 1];
    let dz = pos[3 * j + 2] - pos[3 * i + 2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Radius of gyration about the centroid — a rotation/translation-invariant
/// scalar that captures the overall scale of a layout. Robust golden quantity.
fn radius_of_gyration(pos: &[f32]) -> f32 {
    let n = pos.len() / 3;
    if n == 0 {
        return 0.0;
    }
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    let mut cz = 0.0f64;
    for i in 0..n {
        cx += pos[3 * i] as f64;
        cy += pos[3 * i + 1] as f64;
        cz += pos[3 * i + 2] as f64;
    }
    cx /= n as f64;
    cy /= n as f64;
    cz /= n as f64;
    let mut s = 0.0f64;
    for i in 0..n {
        let dx = pos[3 * i] as f64 - cx;
        let dy = pos[3 * i + 1] as f64 - cy;
        let dz = pos[3 * i + 2] as f64 - cz;
        s += dx * dx + dy * dy + dz * dz;
    }
    (s / n as f64).sqrt() as f32
}

/// A deterministic, reproducible seed layout (no RNG — a SplitMix64-style
/// integer hash mapped to `[-spread, spread]`), so golden runs are stable across
/// machines and invocations.
fn deterministic_seed(n: usize, spread: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; 3 * n];
    for i in 0..3 * n {
        // SplitMix64 finaliser on the index → uniform-ish u64 → f32 in [-1,1].
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let unit = (z >> 40) as f32 / (1u64 << 24) as f32; // [0,1)
        out[i] = (unit * 2.0 - 1.0) * spread;
    }
    out
}

// --- small graph builders --------------------------------------------------

/// Two nodes joined by a single edge.
fn single_edge() -> CsrGraph {
    CsrGraph {
        n_nodes: 2,
        offsets: vec![0, 1, 2],
        neighbors: vec![1, 0],
    }
}

/// A 3-cycle (triangle): 0-1, 1-2, 2-0.
fn triangle() -> CsrGraph {
    CsrGraph {
        n_nodes: 3,
        offsets: vec![0, 2, 4, 6],
        neighbors: vec![1, 2, 0, 2, 0, 1],
    }
}

/// An `n`-cycle (ring): each node joined to its two ring neighbours, CSR with
/// ascending neighbour lists. `cycle(4)` is the square's 4-cycle.
fn cycle(n: usize) -> CsrGraph {
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for v in 0..n {
        let mut nb = [((v + n - 1) % n) as u32, ((v + 1) % n) as u32];
        nb.sort_unstable();
        neighbors.extend_from_slice(&nb);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n as u32,
        offsets,
        neighbors,
    }
}

/// The complete graph on 4 vertices (all 6 edges) — the tetrahedron's topology.
fn k4() -> CsrGraph {
    CsrGraph {
        n_nodes: 4,
        offsets: vec![0, 3, 6, 9, 12],
        neighbors: vec![1, 2, 3, 0, 2, 3, 0, 1, 3, 0, 1, 2],
    }
}

/// Assert `actual ≈ expected` within absolute `tol`, with a descriptive message.
fn approx(what: &str, actual: f32, expected: f32, tol: f32) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} != expected {expected} (Δ={:.2e} > tol {:.2e})",
        (actual - expected).abs(),
        tol
    );
}

/// A `w × h` 4-neighbour grid graph (node `r*w + c`), CSR with ascending
/// neighbour lists. Exercises springs + angle + exclusion at a useful scale.
fn grid(w: usize, h: usize) -> CsrGraph {
    let n = w * h;
    let mut offsets = Vec::with_capacity(n + 1);
    let mut neighbors = Vec::new();
    offsets.push(0u32);
    for r in 0..h {
        for c in 0..w {
            // Ascending node-id order: up, left, right, down.
            if r > 0 {
                neighbors.push(((r - 1) * w + c) as u32);
            }
            if c > 0 {
                neighbors.push((r * w + c - 1) as u32);
            }
            if c + 1 < w {
                neighbors.push((r * w + c + 1) as u32);
            }
            if r + 1 < h {
                neighbors.push(((r + 1) * w + c) as u32);
            }
            offsets.push(neighbors.len() as u32);
        }
    }
    CsrGraph {
        n_nodes: n as u32,
        offsets,
        neighbors,
    }
}

// ---------------------------------------------------------------------------
// 1. CANARY — a library of "known problems" with closed-form solutions
// ---------------------------------------------------------------------------
//
// FEP is validated against a *set* of solved systems, not one. Likewise: each
// case below has an *analytically known* equilibrium, chosen so the set
// collectively pins down every force term against a closed-form answer — edge
// springs, the gravity balance (quantitatively), the angle constraint, and a 3D
// case. If any fails to relax to its known solution within tolerance the solver
// is broken; together they are the loud, fast canary.

/// A problem whose relaxed geometry is known in closed form.
struct SolvedCase {
    name: &'static str,
    /// What this case pins down — surfaced in the pass log and failure message.
    validates: &'static str,
    graph: CsrGraph,
    settings: GeometricSettings,
    seed: Vec<f32>,
    max_steps: usize,
    residual_tol: f32,
    /// Assert the relaxed layout matches the known solution.
    check: Box<dyn Fn(&[f32])>,
}

/// The known-problem library. Order is irrelevant; each entry is independent.
fn solved_cases() -> Vec<SolvedCase> {
    let mut cases = Vec::new();

    // 1. Single spring (edge term). Seeded 3× too long; the only equilibrium is
    //    the two nodes exactly `l` apart.
    let l = 2.0;
    cases.push(SolvedCase {
        name: "single-spring",
        validates: "edge spring → rest length",
        graph: single_edge(),
        settings: springs_only(l),
        seed: vec![0.0, 0.0, 0.0, 3.0 * l, 0.0, 0.0],
        max_steps: 5_000,
        residual_tol: 1e-3,
        check: Box::new(move |p| approx("spring length", dist(p, 0, 1), l, 5e-3)),
    });

    // 2. Spring + gravity (gravity term, *quantitative*). Gravity pulls both
    //    nodes to the origin, compressing the spring to a closed-form
    //    separation: balancing k(2x−L) + g·m·x = 0 about the origin gives
    //    d* = 2x = 2kL / (2k + g·m), with the centroid at the origin.
    let (l, k, g, m) = (3.0f32, 0.3f32, 0.05f32, 1.0f32);
    let d_star = 2.0 * k * l / (2.0 * k + g * m);
    let mut grav = springs_only(l);
    grav.edge_stiffness = k;
    grav.gravity = g; // re-enable gravity for this case
    cases.push(SolvedCase {
        name: "spring-gravity",
        validates: "spring/gravity balance d*=2kL/(2k+gm)",
        graph: single_edge(),
        settings: grav,
        seed: vec![-2.0 * l, 0.5, 0.0, 2.0 * l, -0.5, 0.0],
        max_steps: 12_000,
        residual_tol: 1e-3,
        check: Box::new(move |p| {
            approx("gravity-balanced separation", dist(p, 0, 1), d_star, 5e-3);
            let c = ((p[0] + p[3]) / 2.0)
                .hypot((p[1] + p[4]) / 2.0)
                .hypot((p[2] + p[5]) / 2.0);
            assert!(c < 5e-3, "centroid should sit at the origin, |c|={c:.2e}");
        }),
    });

    // 3. Equilateral triangle (edge term, 2D). Three equal springs on a 3-cycle:
    //    the unique zero-energy state (up to rigid motion) is equilateral.
    let l = 1.5;
    cases.push(SolvedCase {
        name: "equilateral-triangle",
        validates: "3 equal springs → equilateral",
        graph: triangle(),
        settings: springs_only(l),
        seed: vec![0.0, 0.0, 0.0, l * 0.5, 0.0, 0.0, 0.2, 0.3, 0.0],
        max_steps: 8_000,
        residual_tol: 2e-3,
        check: Box::new(move |p| {
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                approx(&format!("triangle side {a}-{b}"), dist(p, a, b), l, 5e-3);
            }
        }),
    });

    // 4. Square (angle term, 2D). A 4-cycle under springs *alone* is a floppy
    //    rhombus (sides L, angle free). The 90° coordination angle pins it to a
    //    square: sides L, diagonals L√2. This is the case that fails if the
    //    angle term regresses.
    let l = 1.0;
    let mut square = springs_only(l);
    square.coordination_source = CoordinationSource::Uniform { bucket: 0 };
    square.coordination_angles = vec![90.0];
    square.angle_stiffness = 0.15;
    cases.push(SolvedCase {
        name: "square-90deg",
        validates: "edge + 90° angle → square",
        graph: cycle(4),
        settings: square,
        seed: vec![
            1.1, 0.0, 0.0, // 0
            0.1, 0.9, 0.0, // 1
            -1.0, 0.2, 0.0, // 2
            0.0, -1.2, 0.0, // 3
        ],
        max_steps: 30_000,
        residual_tol: 4e-3,
        check: Box::new(move |p| {
            for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
                approx(&format!("square side {a}-{b}"), dist(p, a, b), l, 2e-2);
            }
            let diag = l * std::f32::consts::SQRT_2;
            approx("square diagonal 0-2", dist(p, 0, 2), diag, 3e-2);
            approx("square diagonal 1-3", dist(p, 1, 3), diag, 3e-2);
        }),
    });

    // 5. Regular tetrahedron (edge term, 3D). K4 with 6 equal springs has the
    //    regular tetrahedron as its unique zero-energy 3D embedding — every one
    //    of the 6 pairwise distances equals L.
    let l = 2.0;
    cases.push(SolvedCase {
        name: "regular-tetrahedron",
        validates: "K4 equal springs → regular tetrahedron (3D)",
        graph: k4(),
        settings: springs_only(l),
        seed: deterministic_seed(4, 1.5),
        max_steps: 10_000,
        residual_tol: 2e-3,
        check: Box::new(move |p| {
            for (a, b) in [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
                approx(&format!("tetrahedron edge {a}-{b}"), dist(p, a, b), l, 1e-2);
            }
        }),
    });

    cases
}

#[test]
fn canary_solves_known_problems() {
    for case in solved_cases() {
        let scn = Scenario {
            name: case.name,
            graph: case.graph,
            settings: case.settings,
            seed: case.seed,
        };
        let r = relax(&scn, case.max_steps, case.residual_tol, 1);
        let at = r.converged_at.unwrap_or_else(|| {
            let last = r
                .trajectory
                .last()
                .map(|s| s.max_residual)
                .unwrap_or(f32::NAN);
            panic!(
                "[{}] ({}) did not reach equilibrium: residual {:.3e} > tol {:.3e} after {} steps",
                case.name, case.validates, last, case.residual_tol, case.max_steps
            );
        });
        (case.check)(&r.final_positions);
        eprintln!("solved [{}] in {at} steps — {}", case.name, case.validates);
    }
}

// ---------------------------------------------------------------------------
// 1b. CANARY (GPU) — the same known problems must solve on `geometric-gpu`
// ---------------------------------------------------------------------------
//
// The solved-case `check` closures assert closed-form *geometry* (distances),
// which is invariant to the rigid rotation/translation a different backend may
// settle into — so they are the natural CPU↔GPU equivalence gate: the GPU engine
// must relax each known problem to the same analytical answer the CPU engine
// does. Skips cleanly (loudly) when no wgpu adapter is present (CI / sandbox);
// run it on a GPU host (e.g. `cargo test -- --nocapture`, sandbox off).
//
// The WGSL kernel (`geometric_barnes_hut.wgsl`) now implements every CPU force
// term — edge springs + class exclusion/affinity + gravity + the angle
// (coordination) constraint — so the WHOLE library runs on GPU, including the
// square (which the angle term pins down). That makes the square the GPU canary
// for the angle pass: if the WGSL angle gradient regresses, `square-90deg` fails
// here while the spring/gravity cases still pass.

/// Run an arbitrary engine for a fixed number of steps and return the final
/// positions. Generic over the `LayoutEngine` trait so CPU and GPU engines share
/// it. (The GPU engine has no `observe()`, so convergence isn't sampled here — a
/// generous fixed budget is used and the closed-form `check` is the oracle.)
fn run_steps(
    engine: &mut dyn LayoutEngine,
    ctx: &mut EngineCtx,
    graph: &CsrGraph,
    settings: &GeometricSettings,
    seed: &[f32],
    steps: usize,
) -> Vec<f32> {
    engine
        .set_params(&serde_json::to_value(settings).expect("serialize settings"))
        .expect("set_params");
    let shard = CsrShard::whole(graph);
    engine.init(ctx, &shard, seed).expect("init");
    let mut pos = seed.to_vec();
    for _ in 0..steps {
        pos = engine.step(ctx).positions;
    }
    pos
}

#[test]
fn canary_gpu_solves_known_problems() {
    let mut ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_none() {
        eprintln!(
            "SKIP canary_gpu_solves_known_problems: no wgpu adapter \
             (run sandbox-off on a GPU host)"
        );
        return;
    }
    let backend = ctx.gpu.as_ref().map(|g| g.adapter_info.backend);
    eprintln!("canary_gpu: adapter backend = {backend:?}");

    // Budget generously (the CPU equivalents converge in ~20 steps and a settled
    // system stays put); kept modest because each GPU step reads positions back.
    const GPU_STEPS: usize = 2_000;
    let mut ran = 0usize;
    for case in solved_cases() {
        let mut engine = GeometricGpuEngine::new();
        let pos = run_steps(
            &mut engine,
            &mut ctx,
            &case.graph,
            &case.settings,
            &case.seed,
            GPU_STEPS,
        );
        (case.check)(&pos);
        eprintln!("gpu solved [{}] — {}", case.name, case.validates);
        ran += 1;
    }
    assert!(
        ran > 0,
        "expected at least one GPU-supported solved case to run"
    );
}

/// A star: hub node 0 joined to `leaves` leaf nodes (undirected). The hub has
/// degree `leaves`; every leaf has degree 1 — i.e. degrees differ, so a
/// degree-based coordination source resolves to a *non-uniform* vector.
fn star(leaves: usize) -> CsrGraph {
    let n = leaves + 1;
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    // hub (node 0): neighbours 1..=leaves
    for l in 1..=leaves {
        neighbors.push(l as u32);
    }
    offsets.push(neighbors.len() as u32);
    // each leaf: single neighbour 0
    for _ in 0..leaves {
        neighbors.push(0);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n as u32,
        offsets,
        neighbors,
    }
}

/// Read back the per-node coordination ids the GPU engine uploaded. The GPU
/// engine keeps no observable, so this drives it through a real wgpu device and
/// maps the `node_coord` storage buffer it built in `init` — exactly what the
/// WGSL angle pass indexes. `None` if no wgpu adapter is present.
fn gpu_resolved_coordination(
    graph: &CsrGraph,
    settings: &GeometricSettings,
    attrs: Option<&GraphAttributes>,
) -> Option<Vec<u32>> {
    let mut ctx = EngineCtx::try_new_gpu();
    ctx.gpu.as_ref()?;
    let n = graph.n_nodes as usize;
    let mut engine = GeometricGpuEngine::new();
    engine
        .set_params(&serde_json::to_value(settings).expect("serialize settings"))
        .expect("set_params");
    let shard = match attrs {
        Some(a) => CsrShard::whole_with_attributes(graph, a),
        None => CsrShard::whole(graph),
    };
    let seed = deterministic_seed(n, 2.0);
    engine.init(&mut ctx, &shard, &seed).expect("gpu init");
    Some(engine.debug_node_coordination())
}

/// Regression for the "GPU ignored structural sources" bug: with
/// [`CoordinationSource::Degree`] on a non-uniform graph (a star, where the hub's
/// degree differs from every leaf's), the resolved per-node coordination MUST be
/// the node degrees — the old GPU init read injected attributes only and silently
/// produced all-zeros. The CPU resolver assertion always runs; the GPU side is
/// gated on a wgpu adapter (prints SKIP otherwise), since it is the path that was
/// wrong.
#[test]
fn degree_coordination_resolves_same_on_cpu_and_gpu() {
    let graph = star(5); // hub degree 5, five leaves degree 1
    let expected_degrees: Vec<u32> = vec![5, 1, 1, 1, 1, 1];
    let settings = GeometricSettings {
        coordination_source: CoordinationSource::Degree,
        ..GeometricSettings::default()
    };

    // CPU resolver — the single shared source of truth. Always runs.
    let cpu = GeometricEngine::resolve(&settings, &graph, None).expect("cpu resolve");
    assert_eq!(
        cpu.coordination, expected_degrees,
        "CPU degree coordination must equal node degrees (non-uniform)"
    );
    // Sanity: this graph really is non-uniform, so an all-zeros default would be
    // detectably wrong (the exact failure the old GPU init exhibited).
    assert!(
        cpu.coordination.iter().any(|&c| c != cpu.coordination[0]),
        "test graph must have differing degrees to be meaningful"
    );

    // GPU side: assert the engine uploaded the SAME coordination vector, not the
    // old vec![0; n] default.
    match gpu_resolved_coordination(&graph, &settings, None) {
        Some(gpu) => {
            assert_eq!(
                gpu, cpu.coordination,
                "GPU degree coordination diverged from CPU (the structural-source bug)"
            );
            eprintln!("gpu degree coordination matches cpu: {gpu:?}");
        }
        None => eprintln!(
            "SKIP gpu half of degree_coordination_resolves_same_on_cpu_and_gpu: no wgpu adapter"
        ),
    }
}

#[test]
fn canary_energy_descends_to_minimum() {
    // A damped solver is a descent process: total mechanical energy (potential +
    // kinetic) must be shed monotonically, and the potential must end far below
    // where it started, near its floor. Run the full budget (no early stop) and
    // sample every step.
    let rest = 1.5;
    let scn = Scenario {
        name: "triangle-energy",
        graph: triangle(),
        seed: vec![0.0, 0.0, 0.0, rest * 0.5, 0.0, 0.0, 0.2, 0.3, 0.0],
        settings: springs_only(rest),
    };

    let r = relax(&scn, 4_000, 0.0, 1);
    let first = *r.trajectory.first().unwrap();
    let last = *r.trajectory.last().unwrap();

    assert!(
        last.potential < first.potential * 0.05,
        "potential should fall to near its floor: {:.4} -> {:.4}",
        first.potential,
        last.potential
    );
    assert!(
        last.max_residual < first.max_residual,
        "residual force should shrink: {:.4} -> {:.4}",
        first.max_residual,
        last.max_residual
    );

    // Total mechanical energy is non-increasing on coarse checkpoints. A small
    // relative slack absorbs the explicit integrator's per-step jitter without
    // hiding a real divergence.
    let stride = 50;
    let mut prev = f32::INFINITY;
    for s in r.trajectory.iter().step_by(stride) {
        assert!(
            s.total() <= prev * 1.01 + 1e-6,
            "total energy rose at step {}: {:.5} after {:.5}",
            s.step,
            s.total(),
            prev
        );
        prev = s.total();
    }
}

// ---------------------------------------------------------------------------
// 1b. THERMOSTAT — Langevin (Brownian) dynamics canaries
// ---------------------------------------------------------------------------
//
// The canaries above pin the *zero-temperature* minimizer. Self-assembly needs
// the engine to instead sample a *thermal ensemble* (Brownian motion). The
// closed-form check for that is **equipartition**: a free particle's steady
// state has `⟨½ m v²⟩ = ½ kT` per degree of freedom, independent of the time
// step — so a gas of N free particles at temperature `kT` must hold mean
// per-particle kinetic energy `≈ 1.5 kT`. This is the statistical analogue of
// the spring canary and the keystone for everything in
// `docs/self-assembly-plan.md`.

/// `n` isolated nodes (no edges). The free-gas substrate for the thermostat
/// canaries: with every force off, the only dynamics is the OU thermostat, so
/// the steady state is *exactly* the Maxwell–Boltzmann velocity distribution.
fn no_edges(n: usize) -> CsrGraph {
    CsrGraph {
        n_nodes: n as u32,
        offsets: vec![0u32; n + 1],
        neighbors: Vec::new(),
    }
}

/// All forces off; the engine reduces to the bare integrator. Used to isolate
/// the thermostat (free gas) and so the steady state is analytic.
fn free_gas(temperature: f32, damping: f32) -> GeometricSettings {
    GeometricSettings {
        edge_stiffness: 0.0,
        angle_stiffness: 0.0,
        exclusion_strength: 0.0,
        affinity_strength: 0.0,
        gravity: 0.0,
        damping,
        time_step: 1.0,
        max_step: 0.0, // uncapped: a displacement clamp would bias the KE estimate
        temperature,
        ..GeometricSettings::default()
    }
}

#[test]
fn thermostat_free_gas_obeys_equipartition() {
    // A free gas at temperature kT must equilibrate to ⟨KE⟩/particle ≈ 1.5 kT
    // (3 translational DOF × ½ kT), regardless of dt. `observe().kinetic` is the
    // total Σ ½ m v², so dividing by N gives the per-particle mean.
    let kt = 1.0f32;
    let n = 1024;
    let scn = Scenario {
        name: "free-gas-equipartition",
        graph: no_edges(n),
        seed: vec![0.0f32; 3 * n], // start at rest; the thermostat fills in motion
        settings: free_gas(kt, 0.9),
    };

    // Burn in to steady state (velocity autocorrelation time ~ 1/(1−damping) ≈ 10
    // steps), then time-average the per-particle KE to beat down sampling noise.
    let r = relax(&scn, 4_000, 0.0, 10);
    let burn_in = 1_000usize;
    let tail: Vec<f32> = r
        .trajectory
        .iter()
        .filter(|s| s.step >= burn_in)
        .map(|s| s.kinetic / n as f32)
        .collect();
    assert!(!tail.is_empty(), "no post-burn-in samples");
    let mean_ke = tail.iter().sum::<f32>() / tail.len() as f32;

    let expected = 1.5 * kt;
    // ~1024 particles × ~300 time samples is a large ensemble; 8% absorbs the
    // residual sampling + discrete-OU bias without hiding a real miscalibration.
    assert!(
        (mean_ke - expected).abs() < 0.08 * expected,
        "equipartition: mean per-particle KE {mean_ke:.4} should be ≈ 1.5 kT = {expected:.4}"
    );
}

#[test]
fn thermostat_temperature_scales_kinetic_energy() {
    // Equipartition is linear in kT: doubling the temperature must double the
    // steady-state kinetic energy. Tests the *calibration*, not just presence.
    let measure = |kt: f32| -> f32 {
        let n = 1024;
        let scn = Scenario {
            name: "free-gas-scaling",
            graph: no_edges(n),
            seed: vec![0.0f32; 3 * n],
            settings: free_gas(kt, 0.9),
        };
        let r = relax(&scn, 3_000, 0.0, 10);
        let tail: Vec<f32> = r
            .trajectory
            .iter()
            .filter(|s| s.step >= 800)
            .map(|s| s.kinetic / n as f32)
            .collect();
        tail.iter().sum::<f32>() / tail.len() as f32
    };
    let lo = measure(0.5);
    let hi = measure(2.0);
    let ratio = hi / lo;
    // kT goes 0.5 → 2.0 (×4), so KE must scale ×4.
    assert!(
        (ratio - 4.0).abs() < 0.4,
        "KE should scale linearly with kT: ratio {ratio:.3} (expected ≈ 4.0)"
    );
}

#[test]
fn thermostat_off_is_a_pure_minimizer() {
    // temperature == 0 must inject ZERO noise: a free gas seeded at rest with no
    // forces stays at rest (KE stays 0). This is the guard that the historical
    // zero-temperature behaviour — and the golden-master regression — are
    // byte-identical with the thermostat compiled in.
    let n = 64;
    let scn = Scenario {
        name: "free-gas-cold",
        graph: no_edges(n),
        seed: vec![0.0f32; 3 * n],
        settings: free_gas(0.0, 0.9),
    };
    let r = relax(&scn, 200, 0.0, 50);
    for s in &r.trajectory {
        assert_eq!(
            s.kinetic, 0.0,
            "cold free gas must stay at rest (no noise at T=0); KE={} at step {}",
            s.kinetic, s.step
        );
    }
}

// ---------------------------------------------------------------------------
// 2. REGRESSION — golden master of a fixed, force-rich scenario
// ---------------------------------------------------------------------------

/// Robust, low-dimensional summary of a relaxed layout. Rotation/translation
/// invariant where it can be, so the golden compares *physics*, not a basis.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct Golden {
    n: usize,
    steps: usize,
    potential: f32,
    max_residual: f32,
    radius_of_gyration: f32,
    /// Edge-length coefficient of variation (drawing-aesthetic uniformity) via
    /// the shared `graph_layouts::metrics`. `#[serde(default)]` so a pre-existing
    /// golden without this field still parses (regenerate to populate it).
    #[serde(default)]
    edge_length_cv: f32,
}

/// Unique undirected edges `(a, b)` with `a < b` from a CSR graph.
fn unique_edges(g: &CsrGraph) -> Vec<(u32, u32)> {
    let mut e = Vec::new();
    for v in 0..g.n_nodes as usize {
        let (a, b) = (g.offsets[v] as usize, g.offsets[v + 1] as usize);
        for &u in &g.neighbors[a..b] {
            if (v as u32) < u {
                e.push((v as u32, u));
            }
        }
    }
    e
}

/// The regression fixture: a 5×5 grid relaxed under the *default* force set
/// (springs + degree-coordination angle + exclusion + gravity), from a fixed
/// deterministic seed, for a fixed step count. Captures the whole engine, not
/// just one term.
fn regression_scenario() -> (Scenario, usize) {
    const STEPS: usize = 600;
    let graph = grid(5, 5);
    let n = (graph.n_nodes) as usize;
    let settings = GeometricSettings {
        coordination_source: CoordinationSource::Degree,
        ..GeometricSettings::default()
    };
    (
        Scenario {
            name: "grid5x5-default",
            graph,
            settings,
            seed: deterministic_seed(n, 4.0),
        },
        STEPS,
    )
}

#[test]
fn regression_golden_master() {
    let (scn, steps) = regression_scenario();
    let r = relax(&scn, steps, 0.0, steps); // fixed budget, no early stop
    let last = r.trajectory.last().unwrap();
    let actual = Golden {
        n: (scn.graph.n_nodes) as usize,
        steps,
        potential: last.potential,
        max_residual: last.max_residual,
        radius_of_gyration: radius_of_gyration(&r.final_positions),
        edge_length_cv: graph_layouts::metrics::edge_length_cv(
            &r.final_positions,
            &unique_edges(&scn.graph),
        ),
    };

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/geometric_grid5x5.json");
    let update = std::env::var("UPDATE_GEOMETRIC_GOLDEN").is_ok();

    if update || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create golden dir");
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap())
            .expect("write golden");
        eprintln!(
            "geometric regression: wrote golden {} -> {actual:?}",
            path.display()
        );
        return;
    }

    let golden: Golden =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read golden"))
            .expect("parse golden");

    assert_eq!(golden.n, actual.n, "node count changed");
    assert_eq!(golden.steps, actual.steps, "step budget changed");
    // f32 reductions over ~600 deterministic steps reproduce closely across
    // platforms; a small relative tolerance absorbs last-ULP differences while
    // still catching a genuine behavioural drift.
    approx_eq("potential", actual.potential, golden.potential, 1e-3, 1e-3);
    approx_eq(
        "max_residual",
        actual.max_residual,
        golden.max_residual,
        1e-3,
        1e-3,
    );
    approx_eq(
        "radius_of_gyration",
        actual.radius_of_gyration,
        golden.radius_of_gyration,
        1e-3,
        1e-3,
    );
    approx_eq(
        "edge_length_cv",
        actual.edge_length_cv,
        golden.edge_length_cv,
        1e-3,
        1e-3,
    );
}

/// Assert `actual ≈ expected` within `max(abs_tol, rel_tol·|expected|)`, with a
/// message that prints the regenerate hint on failure.
fn approx_eq(what: &str, actual: f32, expected: f32, rel_tol: f32, abs_tol: f32) {
    let tol = abs_tol.max(rel_tol * expected.abs());
    assert!(
        (actual - expected).abs() <= tol,
        "regression in {what}: {actual} vs golden {expected} (Δ={:.3e} > tol {:.3e}). \
         If this change is intended, regenerate with UPDATE_GEOMETRIC_GOLDEN=1.",
        (actual - expected).abs(),
        tol
    );
}

// ---------------------------------------------------------------------------
// 3. PERFORMANCE — throughput + convergence budgets (generous, non-flaky)
// ---------------------------------------------------------------------------

#[test]
fn performance_throughput_and_convergence() {
    // Throughput on a medium grid. Exclusion is O(n²), so keep n modest; 144
    // nodes is enough to be representative without making the floor flaky.
    let graph = grid(12, 12);
    let n = (graph.n_nodes) as usize;
    let edges = graph.neighbors.len() / 2;
    let scn = Scenario {
        name: "grid12x12-perf",
        graph,
        settings: GeometricSettings {
            coordination_source: CoordinationSource::Degree,
            ..GeometricSettings::default()
        },
        seed: deterministic_seed(n, 6.0),
    };

    // Warm up (let the allocator/caches settle), then time a fixed run.
    let _ = relax(&scn, 20, 0.0, 1000);
    const TIMED_STEPS: usize = 300;
    let timed = relax(&scn, TIMED_STEPS, 0.0, TIMED_STEPS);
    let sps = TIMED_STEPS as f64 / timed.wall.as_secs_f64();
    eprintln!(
        "geometric perf [{}]: {sps:.0} steps/sec on {n} nodes / {edges} edges \
         ({:.2} ms/step)",
        scn.name,
        timed.wall.as_secs_f64() * 1e3 / TIMED_STEPS as f64
    );
    assert!(
        sps > 50.0,
        "throughput regression: {sps:.0} steps/sec (<50). Likely an algorithmic \
         or complexity regression in the force kernels."
    );

    // Steps-to-converge budget on the triangle canary. A wall-clock floor can't
    // see an algorithm that still converges but takes 10× the iterations; this
    // can. Generous ceiling so integrator-tuning noise doesn't trip it.
    let rest = 1.5;
    let tri = Scenario {
        name: "triangle-budget",
        graph: triangle(),
        seed: vec![0.0, 0.0, 0.0, rest * 0.5, 0.0, 0.0, 0.2, 0.3, 0.0],
        settings: springs_only(rest),
    };
    let rr = relax(&tri, 8_000, 2e-3, 1);
    let at = rr
        .converged_at
        .expect("triangle must converge within budget");
    eprintln!("geometric perf [{}]: converged in {at} steps", tri.name);
    assert!(
        at < 4_000,
        "convergence-iteration regression: {at} steps to converge (>4000)"
    );
}
