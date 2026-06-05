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

use graph_compute::engines::geometric::{
    AssemblyObservables, CoordinationSource, DirectorSource, GeometricEngine, GeometricSettings,
};
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

/// A stateful SplitMix64 stream (the test seed helper is stateless per-index;
/// a random walk needs a running stream). Same generator as the engine's
/// thermostat, so the canaries stay in one RNG family.
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniformly-distributed unit vector (Marsaglia's method) for the random-walk
/// chain seed.
fn random_unit(state: &mut u64) -> [f32; 3] {
    loop {
        let u = next_u64(state) as f64 / u64::MAX as f64 * 2.0 - 1.0;
        let v = next_u64(state) as f64 / u64::MAX as f64 * 2.0 - 1.0;
        let s = u * u + v * v;
        if s < 1.0 && s > 1e-9 {
            let f = 2.0 * (1.0 - s).sqrt();
            return [(u * f) as f32, (v * f) as f32, (1.0 - 2.0 * s) as f32];
        }
    }
}

/// A freely-jointed random-walk seed: `n` beads, each one `step` away from the
/// previous in a uniformly random direction. This *is* an ideal-chain ensemble
/// sample, so the chain canary doesn't have to wait out the (∝N²) Rouse time to
/// reach equilibrium — it starts at a typical configuration and the thermostat
/// just fluctuates around it.
fn random_walk_seed(n: usize, step: f32, seed: u64) -> Vec<f32> {
    let mut pos = vec![0.0f32; 3 * n];
    let mut rng = seed;
    let (mut x, mut y, mut z) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        pos[3 * i] = x;
        pos[3 * i + 1] = y;
        pos[3 * i + 2] = z;
        let d = random_unit(&mut rng);
        x += step * d[0];
        y += step * d[1];
        z += step * d[2];
    }
    pos
}

/// Time- and ensemble-averaged squared radius of gyration of a thermalized
/// linear chain of `n` beads: average over `chains` independent random-walk
/// seeds, each relaxed and sampled after a burn-in.
fn mean_rg2_chain(n: usize, settings: &GeometricSettings, chains: u64) -> f32 {
    let g = CsrGraph::path(n as u32);
    let (steps, burn_in, every) = (2_000usize, 400usize, 8usize);
    let mut acc = 0.0f64;
    let mut count = 0u64;
    for c in 0..chains {
        let seed = random_walk_seed(n, settings.edge_rest_len, 0xC0FFEE ^ (c + 1) * 2_654_435_761);
        // Each chain gets an INDEPENDENT thermostat stream — otherwise the runs
        // share noise, the ensemble average barely shrinks the variance, and a
        // single chain's fluctuation can skew the fitted exponent.
        let mut settings = settings.clone();
        settings.rng_seed ^= (c + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&settings).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        e.init(&mut ctx, &CsrShard::whole(&g), &seed).unwrap();
        for step in 0..steps {
            let p = e.step(&mut ctx).positions;
            if step >= burn_in && step % every == 0 {
                let rg = radius_of_gyration(&p);
                acc += (rg * rg) as f64;
                count += 1;
            }
        }
    }
    (acc / count as f64) as f32
}

#[test]
fn thermostat_ideal_chain_scales_linearly_with_length() {
    // An ideal (freely-jointed, no excluded-volume) polymer at temperature obeys
    // ⟨R_g²⟩ ∝ N — the Flory exponent ν = 1/2 (R_g ∝ N^ν, R_g² ∝ N^{2ν} = N¹).
    // This validates the thermostat AND the bond springs *together* against a
    // textbook scaling law. It's a *ratio* test, so any N-independent prefactor
    // bias in the integrator's configurational sampling cancels out — only the
    // exponent is asserted. Excluded volume is OFF (it would swell the chain to
    // the self-avoiding ν ≈ 0.588, a different exponent).
    let chain = GeometricSettings {
        edge_rest_len: 1.0,
        edge_stiffness: 1.0,
        angle_stiffness: 0.0,
        exclusion_strength: 0.0, // ideal chain: NO excluded volume
        affinity_strength: 0.0,
        gravity: 0.0,
        damping: 0.9,
        time_step: 0.5,
        max_step: 0.0,
        temperature: 0.5,
        ..GeometricSettings::default()
    };

    let rg2_16 = mean_rg2_chain(16, &chain, 4);
    let rg2_32 = mean_rg2_chain(32, &chain, 4);
    let rg2_64 = mean_rg2_chain(64, &chain, 4);

    // Each doubling of N should roughly double ⟨R_g²⟩.
    let r1 = rg2_32 / rg2_16;
    let r2 = rg2_64 / rg2_32;
    // Fit the exponent: R_g² ∝ N^p ⇒ p = log2(rg2_64 / rg2_16) / log2(64/16).
    let p = (rg2_64 / rg2_16).log2() / 4.0f32.log2();

    eprintln!(
        "ideal chain: R_g²(16)={rg2_16:.3} R_g²(32)={rg2_32:.3} R_g²(64)={rg2_64:.3} \
         | ratios {r1:.2}, {r2:.2} | exponent p={p:.3} (ideal=1.0)"
    );
    // Generous band: sampling noise + finite-N corrections (the exact form is
    // ∝ (N − 1/N)) keep this from being exactly 1.0, but a self-avoiding (~1.18)
    // or collapsed/rod regime would fall well outside.
    assert!(
        (p - 1.0).abs() < 0.22,
        "ideal-chain R_g² should scale ~ N¹ (got exponent {p:.3})"
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
// 1c. ATTRACTIVE WELL — tunable-range cohesion (Cooke–Deserno) canaries
// ---------------------------------------------------------------------------
//
// The exclusion term alone only *prevents* overlap; nothing makes two unbonded
// monomers stick. Phase W adds a soft attractive well (WCA repulsion to contact
// σ, then a cosine² tail of depth ε out to σ+w_c). It is a *clean* potential, so
// the minimum of repulsion+well sits exactly at contact σ — two particles
// starting *outside* σ but within the well must condense to a bound pair at ≈σ.
// And because the well lowers the system's energy, a multi-particle droplet must
// compactify *monotonically* as the well deepens. These are the closed-form
// (separation → σ) and ordinal (deeper ε ⇒ smaller R_g) canaries for cohesion.

/// Settings isolating the attractive well on a free (unbonded) pair/cluster:
/// no edges, no gravity, no angle/affinity — just WCA exclusion + the cohesion
/// well, with a fixed radius so σ = 2·radius is known.
fn cohesion_only(radius: f32, well_depth: f32, well_width: f32) -> GeometricSettings {
    GeometricSettings {
        edge_stiffness: 0.0,
        angle_stiffness: 0.0,
        affinity_strength: 0.0,
        gravity: 0.0,
        exclusion_strength: 1.0,
        class_radius: vec![radius],
        default_radius: radius,
        well_depth,
        well_width,
        damping: 0.6,
        time_step: 1.0,
        max_step: 0.5,
        temperature: 0.0, // deterministic minimizer: the pair must settle, not jiggle
        ..GeometricSettings::default()
    }
}

#[test]
fn well_off_by_default_is_noncohesive() {
    // Guard: with well_depth = 0 (the default) two particles seeded apart feel NO
    // attraction — they stay put (only exclusion acts, and they're already past
    // σ). This is the byte-identical-default guarantee for the new term.
    let radius = 0.5f32;
    let sigma = 2.0 * radius;
    let start = sigma + 0.8; // inside what *would* be the well, but well is OFF
    let scn = Scenario {
        name: "well-off",
        graph: no_edges(2),
        seed: vec![0.0, 0.0, 0.0, start, 0.0, 0.0],
        settings: cohesion_only(radius, 0.0, 1.0),
    };
    let r = relax(&scn, 1_000, 0.0, 50);
    let d = dist(&r.final_positions, 0, 1);
    approx("non-cohesive separation unchanged", d, start, 1e-3);
}

#[test]
fn well_binds_a_pair_to_contact() {
    // Two particles seeded *outside* contact σ but within the attractive well
    // (σ < d₀ < σ + w_c) must attract and relax to a bound pair at the well's
    // energy minimum, which (repulsion + cohesion) sits exactly at contact σ.
    let radius = 0.5f32;
    let sigma = 2.0 * radius; // = 1.0
    let well_width = 1.5f32;
    let start = sigma + 0.9; // inside the well (0.9 < w_c) but well beyond σ
    let scn = Scenario {
        name: "well-bind",
        graph: no_edges(2),
        seed: vec![0.0, 0.0, 0.0, start, 0.0, 0.0],
        settings: cohesion_only(radius, 1.0, well_width),
    };
    let r = relax(&scn, 4_000, 0.0, 50);
    let d = dist(&r.final_positions, 0, 1);
    // It must have moved *inward* from the seed (real attraction)…
    assert!(
        d < start - 0.3,
        "pair should be drawn inward by the well: {start} -> {d}"
    );
    // …and settle at the energy minimum = contact σ.
    approx("bound-pair separation", d, sigma, 3e-2);
}

#[test]
fn well_does_not_reach_beyond_its_width() {
    // The well has finite range: a pair seeded *past* σ + w_c feels nothing and
    // stays put. Confirms the tail truly vanishes at σ + w_c (no spurious
    // long-range pull leaking past the cutoff).
    let radius = 0.5f32;
    let sigma = 2.0 * radius;
    let well_width = 0.8f32;
    let start = sigma + well_width + 0.5; // strictly outside the well
    let scn = Scenario {
        name: "well-out-of-range",
        graph: no_edges(2),
        seed: vec![0.0, 0.0, 0.0, start, 0.0, 0.0],
        settings: cohesion_only(radius, 1.0, well_width),
    };
    let r = relax(&scn, 1_000, 0.0, 50);
    let d = dist(&r.final_positions, 0, 1);
    approx("out-of-range separation unchanged", d, start, 1e-3);
}

#[test]
fn well_energy_matches_negative_gradient() {
    // The cohesion well is advertised as a *clean* potential: −∇(cohesion) must
    // equal the force the integrator applies. Verify numerically — central finite
    // difference of EnergyBreakdown.cohesion vs the residual along the pair axis,
    // at a separation inside the well. (Exclusion off so cohesion is the only
    // pairwise term; the residual is then purely the well force.)
    let radius = 0.5f32;
    let sigma = 2.0 * radius;
    let d0 = sigma + 0.4; // inside the well
    let mut s = cohesion_only(radius, 1.0, 1.5);
    s.exclusion_strength = 0.0; // isolate cohesion (d0 > σ so exclusion is 0 anyway)

    let energy_at = |sep: f32| -> f32 {
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        let g = no_edges(2);
        let pos = vec![0.0, 0.0, 0.0, sep, 0.0, 0.0];
        e.init(&mut ctx, &CsrShard::whole(&g), &pos).unwrap();
        e.observe().unwrap().energy.cohesion
    };

    // Residual force on node 0 along +x at d0 (observe reports ‖F‖; the well is
    // attractive so node 0 is pulled toward +x, node 1 toward −x).
    let mut e = GeometricEngine::new();
    e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
    let mut ctx = EngineCtx::cpu_only();
    let g = no_edges(2);
    let pos = vec![0.0, 0.0, 0.0, d0, 0.0, 0.0];
    e.init(&mut ctx, &CsrShard::whole(&g), &pos).unwrap();
    let f_mag = e.observe().unwrap().max_residual;
    assert!(f_mag > 1e-3, "well should exert a force at d0; got {f_mag}");

    // dE/dd via central difference; the magnitude of the pair force is |dE/dd|.
    let h = 1e-3f32;
    let de_dd = (energy_at(d0 + h) - energy_at(d0 - h)) / (2.0 * h);
    approx("|−∇E| == |F| for the well", f_mag, de_dd.abs(), 5e-3);
}

#[test]
fn deeper_well_binds_a_pair_faster_monotonically() {
    // Monotonicity canary (closed-form, deterministic): the well force scales
    // linearly with ε, so from a *fixed* starting separation inside the well a
    // deeper well drives the pair to contact in strictly *fewer* steps. This is
    // the unambiguous "deeper ⇒ tighter/faster binding" check the plan asks for
    // — no thermal sampling, no kinetic-trap confound (a many-body droplet at
    // intermediate ε can sit in a metastable open shell, so binding *speed* is
    // the robust monotone order parameter, not a finite-cluster R_g).
    let radius = 0.5f32;
    let sigma = 2.0 * radius; // = 1.0
    let well_width = 1.5f32;
    let start = sigma + 1.0; // inside the well, same for every ε

    // Steps until the pair first reaches (near) contact, T=0 minimizer.
    let steps_to_bind = |eps: f32| -> usize {
        let mut s = cohesion_only(radius, eps, well_width); // temperature stays 0
        // Overdamped + small step + UNCAPPED: drift speed ∝ force ∝ ε, so binding
        // time falls cleanly with ε. A displacement clamp would saturate the deep
        // wells and erase the monotone gradient, so disable it here.
        s.damping = 0.2;
        s.time_step = 0.2;
        s.max_step = 0.0;
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        let g = no_edges(2);
        let seed = vec![0.0, 0.0, 0.0, start, 0.0, 0.0];
        e.init(&mut ctx, &CsrShard::whole(&g), &seed).unwrap();
        let target = sigma + 0.05; // "bound" once within 5% of contact
        for step in 0..20_000 {
            let pos = e.step(&mut ctx).positions;
            if dist(&pos, 0, 1) <= target {
                return step;
            }
        }
        usize::MAX // never bound within budget
    };

    let t05 = steps_to_bind(0.5);
    let t15 = steps_to_bind(1.5);
    let t30 = steps_to_bind(3.0);
    eprintln!("well binding time: ε=0.5 → {t05}, 1.5 → {t15}, 3.0 → {t30} steps");

    assert!(
        t05 < usize::MAX && t15 < usize::MAX && t30 < usize::MAX,
        "every well depth must bind the pair within budget"
    );
    // Strictly monotone: deeper well ⇒ faster binding.
    assert!(t15 < t05, "ε 0.5→1.5 should bind faster: {t05} -> {t15}");
    assert!(t30 < t15, "ε 1.5→3.0 should bind faster: {t15} -> {t30}");
}

#[test]
fn well_condenses_a_loose_cloud() {
    // The multi-particle condensation signature: a deterministic minimizer turns
    // a loose cubic cloud (neighbours spaced *beyond* contact but *within* the
    // well) into a tight droplet, whereas with the well OFF only exclusion acts —
    // particles past σ feel nothing and the cloud stays loose. So R_g(well) must
    // be clearly below R_g(no well). T=0 (pure minimizer) keeps it reproducible
    // and free of any thermal-evaporation confound.
    let radius = 0.5f32;
    let n = 27usize; // 3×3×3 loose cubic cloud
    let spacing = 1.4f32; // > σ = 1.0 (loose) but neighbours within σ + w_c
    let mut seed = vec![0.0f32; 3 * n];
    let mut k = 0usize;
    for ix in 0..3 {
        for iy in 0..3 {
            for iz in 0..3 {
                seed[3 * k] = (ix as f32 - 1.0) * spacing;
                seed[3 * k + 1] = (iy as f32 - 1.0) * spacing;
                seed[3 * k + 2] = (iz as f32 - 1.0) * spacing;
                k += 1;
            }
        }
    }

    let relax_rg = |eps: f32| -> f32 {
        let s = cohesion_only(radius, eps, 1.5); // temperature 0: pure minimizer
        let scn = Scenario {
            name: "cloud",
            graph: no_edges(n),
            seed: seed.clone(),
            settings: s,
        };
        let r = relax(&scn, 4_000, 0.0, 4_000);
        radius_of_gyration(&r.final_positions)
    };

    let rg_off = relax_rg(0.0); // no well: cloud stays loose
    let rg_on = relax_rg(2.0); // well on: cloud condenses
    eprintln!("cloud R_g: well off → {rg_off:.3}, well on → {rg_on:.3}");
    assert!(
        rg_on < rg_off - 0.2,
        "the attractive well must condense the loose cloud: off={rg_off:.3} on={rg_on:.3}"
    );
}

// ---------------------------------------------------------------------------
// 1d. DIRECTOR / PATCHY — per-node orientation + orientation-dependent well
// ---------------------------------------------------------------------------
//
// Phase A adds a per-node unit director, integrated under rotational Brownian
// motion, and makes the cohesion well orientation-dependent (patchy): the well's
// depth for a pair scales with `1 + anisotropy_strength·(nᵢ·nⱼ)`, so ALIGNED
// directors attract more. The orientational ground state is therefore a
// mutually-aligned (nematic) aggregate — the bilayer-sheet precursor. The
// closed-form-ish detector is the **nematic order parameter** S = largest
// eigenvalue of Q = ⟨nn⟩ − I/3 (S→0 isotropic, S→1 perfectly aligned). The
// canary: starting from a disordered (random) director field, the well +
// anisotropy + rotational thermostat must drive S clearly upward.

/// Nematic order parameter S = (3/2)·λ_max(Q), where Q = ⟨n⊗n⟩ − I/3 is the
/// traceless orientation tensor; computed in-test (the Phase-O observable, local
/// for now). `directors` is interleaved x,y,z. The 3/2 normalisation gives the
/// textbook range S ∈ [0,1]: 0 isotropic, 1 perfectly aligned (S is head-tail
/// symmetric — it does not distinguish n from −n, the physically correct nematic
/// measure). (λ_max alone tops out at 2/3 for a perfectly aligned field.)
fn nematic_s(directors: &[f32]) -> f32 {
    let n = directors.len() / 3;
    if n == 0 {
        return 0.0;
    }
    // Build symmetric Q = (1/N) Σ nn - I/3 (six unique entries).
    let (mut qxx, mut qyy, mut qzz) = (0.0f64, 0.0f64, 0.0f64);
    let (mut qxy, mut qxz, mut qyz) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y, z) = (
            directors[3 * i] as f64,
            directors[3 * i + 1] as f64,
            directors[3 * i + 2] as f64,
        );
        qxx += x * x;
        qyy += y * y;
        qzz += z * z;
        qxy += x * y;
        qxz += x * z;
        qyz += y * z;
    }
    let inv = 1.0 / n as f64;
    let third = 1.0 / 3.0;
    let (qxx, qyy, qzz) = (qxx * inv - third, qyy * inv - third, qzz * inv - third);
    let (qxy, qxz, qyz) = (qxy * inv, qxz * inv, qyz * inv);

    // Largest eigenvalue of the symmetric 3×3 via the closed-form trigonometric
    // method (Smith 1961). Q is traceless, but the formula holds generally.
    let p1 = qxy * qxy + qxz * qxz + qyz * qyz;
    if p1 < 1e-18 {
        // Diagonal already: largest diagonal entry is the largest eigenvalue.
        return (1.5 * qxx.max(qyy).max(qzz)) as f32;
    }
    let q = (qxx + qyy + qzz) / 3.0; // = 0 (traceless), kept for generality
    let p2 = (qxx - q).powi(2) + (qyy - q).powi(2) + (qzz - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    // B = (1/p)(Q - qI); det(B)/2 = cos(3φ).
    let (bxx, byy, bzz) = ((qxx - q) / p, (qyy - q) / p, (qzz - q) / p);
    let (bxy, bxz, byz) = (qxy / p, qxz / p, qyz / p);
    let det_b = bxx * (byy * bzz - byz * byz) - bxy * (bxy * bzz - byz * bxz)
        + bxz * (bxy * byz - byy * bxz);
    let r = (det_b / 2.0).clamp(-1.0, 1.0);
    let phi = r.acos() / 3.0;
    // Largest eigenvalue = q + 2p·cos(phi); S = (3/2)·λ_max.
    (1.5 * (q + 2.0 * p * phi.cos())) as f32
}

#[test]
fn nematic_s_detects_known_order() {
    // Sanity for the in-test observable: a perfectly aligned field → S = 1, an
    // isotropic field (axes) → S ≈ 0. Guards against a broken eigensolver giving
    // a false "ordering" signal in the patchy canary below.
    let aligned: Vec<f32> = (0..50).flat_map(|_| [0.0f32, 0.0, 1.0]).collect();
    let s_aligned = nematic_s(&aligned);
    assert!(
        (s_aligned - 1.0).abs() < 1e-3,
        "aligned field should give S≈1, got {s_aligned}"
    );
    // Equal thirds along x, y, z ⇒ Q = 0 ⇒ S = 0.
    let mut iso = Vec::new();
    for axis in 0..3 {
        for _ in 0..20 {
            let mut v = [0.0f32; 3];
            v[axis] = 1.0;
            iso.extend_from_slice(&v);
        }
    }
    let s_iso = nematic_s(&iso);
    assert!(s_iso < 0.05, "isotropic field should give S≈0, got {s_iso}");
}

#[test]
fn patchy_well_drives_nematic_alignment() {
    // A population of patchy particles in a small box: the attractive well + the
    // orientation anisotropy + the rotational thermostat must take a DISORDERED
    // (random-director) seed to a clearly ALIGNED aggregate — the nematic order
    // parameter S rises from ~0 toward a high value.
    let radius = 0.5f32;
    let n = 64usize;

    // Pack into a 4×4×4 grid spaced *within* the well so every neighbour cohering
    // pair exerts an aligning torque (the spatial overlap the patchy term needs).
    let spacing = 1.2f32; // > σ = 1.0 but inside σ + w_c
    let mut seed = vec![0.0f32; 3 * n];
    let mut k = 0usize;
    for ix in 0..4 {
        for iy in 0..4 {
            for iz in 0..4 {
                seed[3 * k] = (ix as f32 - 1.5) * spacing;
                seed[3 * k + 1] = (iy as f32 - 1.5) * spacing;
                seed[3 * k + 2] = (iz as f32 - 1.5) * spacing;
                k += 1;
            }
        }
    }

    let settings = GeometricSettings {
        // Free (unbonded) particles — alignment must come from the patchy well,
        // not from bonds.
        edge_stiffness: 0.0,
        angle_stiffness: 0.0,
        affinity_strength: 0.0,
        gravity: 0.0,
        exclusion_strength: 1.0,
        class_radius: vec![radius],
        default_radius: radius,
        // Patchy well ON: deep + wide enough to bind, strongly anisotropic so the
        // aligned configuration is clearly favoured.
        well_depth: 2.0,
        well_width: 1.5,
        anisotropy_strength: 2.0,
        // Modest rotational diffusion: enough to let directors escape the random
        // seed and explore, but well below the aligning torque so the nematic
        // ground state wins (the order/disorder balance is the coupling/noise
        // ratio — too much noise melts the alignment).
        rotational_diffusion: 0.15,
        director_source: DirectorSource::Random, // DISORDERED seed
        // Low temperature: enough to let positions/directors settle out of the
        // random seed, cold enough that the nematic aggregate does not re-melt.
        temperature: 0.1,
        rng_seed: 0xA11C_E000_1234_5678,
        damping: 0.6,
        time_step: 0.4,
        max_step: 0.3,
        ..GeometricSettings::default()
    };

    let mut e = GeometricEngine::new();
    e.set_params(&serde_json::to_value(&settings).unwrap()).unwrap();
    let mut ctx = EngineCtx::cpu_only();
    e.init(&mut ctx, &CsrShard::whole(&no_edges(n)), &seed)
        .expect("init");

    // The random seed must actually be disordered (else the test is vacuous).
    let s_start = nematic_s(e.directors().expect("directors present"));
    assert!(
        s_start < 0.35,
        "random director seed should start near-isotropic, got S={s_start:.3}"
    );

    // Relax; time-average S over the tail to read the ordered steady state past
    // the thermal jitter.
    let steps = 4_000usize;
    let burn_in = 2_000usize;
    let mut s_tail = Vec::new();
    for step in 0..steps {
        let _ = e.step(&mut ctx);
        if step >= burn_in && step % 25 == 0 {
            s_tail.push(nematic_s(e.directors().unwrap()));
        }
    }
    let s_end = s_tail.iter().sum::<f32>() / s_tail.len() as f32;
    eprintln!(
        "patchy alignment: S {s_start:.3} (disordered seed) -> {s_end:.3} (aligned aggregate)"
    );

    // The aligned aggregate must be clearly more ordered than the disordered seed.
    // The steady-state S sits near 0.97 here; require a large, unambiguous rise so
    // a regression that breaks the orientation coupling (or the patchy well) trips
    // this loudly rather than passing on a marginal fluctuation.
    assert!(
        s_end > s_start + 0.4 && s_end > 0.7,
        "patchy well should drive nematic alignment: S {s_start:.3} -> {s_end:.3}"
    );
}

#[test]
fn directors_are_static_at_zero_temperature() {
    // Determinism guard: at temperature 0 the rotational thermostat injects ZERO
    // noise, so the director field is frozen at its seed even with the patchy well
    // and anisotropy fully ON. (The aligning torque alone could still rotate
    // directors, but a *pure minimizer* at T=0 must reach a static configuration —
    // here the seed is already a uniform-z fixed point of the aligning field, so
    // nothing moves.) This is the rotational analogue of the T=0 pure-minimizer
    // guarantee for positions.
    let radius = 0.5f32;
    let n = 16usize;
    let mut seed = vec![0.0f32; 3 * n];
    for i in 0..n {
        seed[3 * i] = (i as f32) * 0.9; // a loose line, some pairs within the well
    }
    let mut s = cohesion_only(radius, 2.0, 1.5);
    s.temperature = 0.0; // no rotational noise
    s.anisotropy_strength = 1.0;
    s.director_source = DirectorSource::AlignedZ; // a fixed point of the torque
    let mut e = GeometricEngine::new();
    e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
    let mut ctx = EngineCtx::cpu_only();
    e.init(&mut ctx, &CsrShard::whole(&no_edges(n)), &seed)
        .unwrap();
    let before = e.directors().unwrap().to_vec();
    for _ in 0..200 {
        let _ = e.step(&mut ctx);
    }
    let after = e.directors().unwrap().to_vec();
    assert_eq!(
        before, after,
        "directors must be static at temperature == 0 (no rotational noise)"
    );
}

#[test]
fn directors_do_not_affect_forces_without_anisotropy() {
    // Backward-compat guarantee: with anisotropy 0 (the default) the director
    // field — however it is seeded or however it rotates under the thermostat —
    // has NO effect on the dynamics. A random director field and a uniform one
    // must produce *byte-identical* positions, because the well's orientation
    // factor is identically 1. This is what keeps the golden master untouched.
    let radius = 0.5f32;
    let n = 27usize;
    let spacing = 1.2f32;
    let mut seed = vec![0.0f32; 3 * n];
    let mut k = 0usize;
    for ix in 0..3 {
        for iy in 0..3 {
            for iz in 0..3 {
                seed[3 * k] = (ix as f32 - 1.0) * spacing;
                seed[3 * k + 1] = (iy as f32 - 1.0) * spacing;
                seed[3 * k + 2] = (iz as f32 - 1.0) * spacing;
                k += 1;
            }
        }
    }

    let run = |director_source: DirectorSource| -> Vec<f32> {
        let mut s = cohesion_only(radius, 2.0, 1.5);
        s.temperature = 0.0; // deterministic positions
        s.anisotropy_strength = 0.0; // patchy OFF ⇒ orientation must be irrelevant
        s.director_source = director_source;
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        e.init(&mut ctx, &CsrShard::whole(&no_edges(n)), &seed)
            .unwrap();
        let mut pos = seed.clone();
        for _ in 0..300 {
            pos = e.step(&mut ctx).positions;
        }
        pos
    };

    let random_dirs = run(DirectorSource::Random);
    let aligned_dirs = run(DirectorSource::AlignedZ);
    assert_eq!(
        random_dirs, aligned_dirs,
        "with anisotropy == 0 the director field must not affect positions"
    );
}

// ---------------------------------------------------------------------------
// PHASE O — self-assembly order parameters (observe_assembly)
// ---------------------------------------------------------------------------
//
// These exercise the ENGINE's observable (GeometricEngine::observe_assembly) on
// synthetic point clouds whose answers are known by construction: a perfectly
// aligned director lattice → S≈1, random directors → S≈0; one dense blob → a
// single cluster, a scattered gas → many singletons; a hollow sphere → "closed",
// a flat disk → "open". They pin every Phase-O field against ground truth so a
// later regression in the eigensolver / union-find / closure heuristic trips
// loudly. Directors are supplied via injected GraphAttributes so the test
// controls them exactly (the dynamics are not run — these are pure observables).

/// Build an inited engine over `n` unbonded nodes at the given `positions`, with
/// the given per-node `directors` (interleaved x,y,z) injected verbatim, and a
/// uniform contact radius. No dynamics are stepped; the engine exists only so
/// `observe_assembly` can read the configuration.
fn assembly_engine(
    positions: &[f32],
    directors: &[f32],
    radius: f32,
) -> GeometricEngine {
    let n = positions.len() / 3;
    let settings = GeometricSettings {
        edge_stiffness: 0.0,
        angle_stiffness: 0.0,
        exclusion_strength: 0.0,
        affinity_strength: 0.0,
        gravity: 0.0,
        well_depth: 0.0,
        temperature: 0.0,
        class_radius: vec![radius],
        default_radius: radius,
        director_source: DirectorSource::Injected,
        ..GeometricSettings::default()
    };
    let attrs = GraphAttributes {
        node_director: Some(directors.to_vec()),
        ..Default::default()
    };
    let mut e = GeometricEngine::new();
    e.set_params(&serde_json::to_value(&settings).unwrap()).unwrap();
    let mut ctx = EngineCtx::cpu_only();
    let g = no_edges(n);
    e.init(&mut ctx, &CsrShard::whole_with_attributes(&g, &attrs), positions)
        .expect("init assembly engine");
    e
}

#[test]
fn assembly_nematic_s_spans_aligned_to_random() {
    // Aligned lattice of directors → S ≈ 1; an isotropic (axis-balanced) field →
    // S ≈ 0. This is the same eigensolver the in-test `nematic_s` checks, but read
    // through the ENGINE's observe_assembly so the wiring is validated end to end.
    let n = 64usize;
    let pos = vec![0.0f32; 3 * n]; // positions irrelevant to S
    let aligned: Vec<f32> = (0..n).flat_map(|_| [0.0f32, 0.0, 1.0]).collect();
    let s_aligned = assembly_engine(&pos, &aligned, 0.5)
        .observe_assembly()
        .unwrap()
        .nematic_s;
    assert!(
        (s_aligned - 1.0).abs() < 1e-3,
        "aligned directors should give S≈1, got {s_aligned}"
    );

    // Equal thirds along x/y/z ⇒ Q = 0 ⇒ S = 0.
    let mut iso = Vec::new();
    for axis in 0..3 {
        for _ in 0..(n / 3) {
            let mut v = [0.0f32; 3];
            v[axis] = 1.0;
            iso.extend_from_slice(&v);
        }
    }
    let n_iso = iso.len() / 3;
    let pos_iso = vec![0.0f32; 3 * n_iso];
    let s_iso = assembly_engine(&pos_iso, &iso, 0.5)
        .observe_assembly()
        .unwrap()
        .nematic_s;
    assert!(s_iso < 0.05, "isotropic directors should give S≈0, got {s_iso}");
}

#[test]
fn assembly_cluster_count_singletons_vs_one_blob() {
    // A scattered gas (spacing ≫ contact cutoff) → n singleton clusters; a tight
    // blob (spacing ≪ cutoff) → exactly one cluster spanning all nodes.
    let radius = 0.5f32; // σ = 1.0; default contact cutoff = 1.2·σ = 1.2
    let n = 27usize;

    // Gas: 3×3×3 grid spaced far apart (5.0 ≫ 1.2) ⇒ every node isolated.
    let mut gas = vec![0.0f32; 3 * n];
    let mut k = 0usize;
    for ix in 0..3 {
        for iy in 0..3 {
            for iz in 0..3 {
                gas[3 * k] = ix as f32 * 5.0;
                gas[3 * k + 1] = iy as f32 * 5.0;
                gas[3 * k + 2] = iz as f32 * 5.0;
                k += 1;
            }
        }
    }
    let dirs = [0.0f32, 0.0, 1.0].repeat(n);
    let gas_obs = assembly_engine(&gas, &dirs, radius)
        .observe_assembly()
        .unwrap();
    assert_eq!(
        gas_obs.cluster_count, n,
        "a dispersed gas should be all singletons, got {} clusters",
        gas_obs.cluster_count
    );
    assert_eq!(gas_obs.largest_cluster, 1, "no two gas nodes are in contact");

    // Blob: same grid spaced 1.0 (< 1.2 cutoff) ⇒ a single connected cluster.
    let mut blob = vec![0.0f32; 3 * n];
    let mut k = 0usize;
    for ix in 0..3 {
        for iy in 0..3 {
            for iz in 0..3 {
                blob[3 * k] = ix as f32 * 1.0;
                blob[3 * k + 1] = iy as f32 * 1.0;
                blob[3 * k + 2] = iz as f32 * 1.0;
                k += 1;
            }
        }
    }
    let blob_obs = assembly_engine(&blob, &dirs, radius)
        .observe_assembly()
        .unwrap();
    assert_eq!(
        blob_obs.cluster_count, 1,
        "a tight blob should be one cluster, got {}",
        blob_obs.cluster_count
    );
    assert_eq!(blob_obs.largest_cluster, n, "the blob should contain every node");
    approx("blob largest-cluster frac", blob_obs.largest_cluster_frac, 1.0, 1e-6);
}

#[test]
fn assembly_closure_distinguishes_shell_from_disk() {
    // A hollow sphere of points wrapping its centroid reads as CLOSED (solid-angle
    // coverage → 1); a flat disk leaves both polar caps empty and reads as OPEN
    // (coverage ≈ ½). This is the mesh-free open-sheet-vs-closed-vesicle screen.

    // Hollow sphere: a Fibonacci-sphere of points all at distance R from origin.
    let n_sphere = 200usize;
    let r = 3.0f32;
    let mut sphere = vec![0.0f32; 3 * n_sphere];
    let golden = std::f32::consts::PI * (3.0 - 5.0f32.sqrt()); // golden angle
    for i in 0..n_sphere {
        let y = 1.0 - (i as f32 / (n_sphere - 1) as f32) * 2.0; // [1, -1]
        let rad = (1.0 - y * y).max(0.0).sqrt();
        let theta = golden * i as f32;
        sphere[3 * i] = r * theta.cos() * rad;
        sphere[3 * i + 1] = r * y;
        sphere[3 * i + 2] = r * theta.sin() * rad;
    }
    // Make every point a contact neighbour of the next so they form ONE cluster
    // (the closure metric runs on the largest cluster). Use a big radius so the
    // shell is connected; the closure metric itself is radius-independent.
    let dirs = [0.0f32, 0.0, 1.0].repeat(n_sphere);
    let shell_obs = assembly_engine(&sphere, &dirs, 5.0)
        .observe_assembly()
        .unwrap();
    assert_eq!(
        shell_obs.largest_cluster, n_sphere,
        "the shell must be one cluster for the closure metric to see all of it"
    );
    assert!(
        shell_obs.is_closed(),
        "a hollow sphere should read as CLOSED (closure={:.3})",
        shell_obs.closure
    );
    assert!(
        shell_obs.closure > 0.85,
        "hollow sphere closure should be near 1, got {:.3}",
        shell_obs.closure
    );

    // Flat disk in the z=0 plane: points only point outward in-plane, so the two
    // polar caps (±z) of the centroid's view sphere are never covered ⇒ ~½ at most.
    let n_disk = 200usize;
    let mut disk = vec![0.0f32; 3 * n_disk];
    for i in 0..n_disk {
        // A filled disk via the sunflower (Vogel) spiral.
        let rr = 3.0 * (i as f32 / n_disk as f32).sqrt();
        let theta = golden * i as f32;
        disk[3 * i] = rr * theta.cos();
        disk[3 * i + 1] = rr * theta.sin();
        // z stays 0 → flat.
    }
    let dirs_d = [0.0f32, 0.0, 1.0].repeat(n_disk);
    let disk_obs = assembly_engine(&disk, &dirs_d, 5.0)
        .observe_assembly()
        .unwrap();
    assert_eq!(disk_obs.largest_cluster, n_disk, "the disk must be one cluster");
    assert!(
        !disk_obs.is_closed(),
        "a flat disk should read as OPEN (closure={:.3})",
        disk_obs.closure
    );
    assert!(
        disk_obs.closure < 0.6,
        "flat-disk closure should be well below a shell's, got {:.3}",
        disk_obs.closure
    );
    // And the gap between the two must be unambiguous, not a marginal split.
    assert!(
        shell_obs.closure - disk_obs.closure > 0.3,
        "closure must clearly separate shell ({:.3}) from disk ({:.3})",
        shell_obs.closure,
        disk_obs.closure
    );
}

#[test]
fn assembly_observables_empty_graph_is_zeroed() {
    // Guard the degenerate path: zero nodes ⇒ all-zero observables, no panic.
    let e = assembly_engine(&[], &[], 0.5);
    let obs: AssemblyObservables = e.observe_assembly().unwrap();
    assert_eq!(obs.n, 0);
    assert_eq!(obs.cluster_count, 0);
    assert_eq!(obs.largest_cluster, 0);
    assert_eq!(obs.nematic_s, 0.0);
    assert_eq!(obs.closure, 0.0);
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
