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
//!   1. **CANARY (solved cases).** Tiny graphs whose equilibrium is known
//!      *analytically* — a single spring relaxes to its rest length; three equal
//!      springs relax to an equilateral triangle. The engine MUST reach that
//!      geometry and the residual MUST fall below tolerance. These are the loud,
//!      fast "the solver is broken" alarms.
//!   2. **REGRESSION (golden master).** A fixed scenario run for a fixed number
//!      of steps from a fixed seed; robust scalars of the final state (energy,
//!      residual, radius of gyration) are compared against a committed golden
//!      file. Drift beyond tolerance fails. Regenerate with
//!      `UPDATE_GEOMETRIC_GOLDEN=1` (a first run with no golden writes one).
//!   3. **PERFORMANCE.** Throughput (steps/sec) and steps-to-convergence on
//!      fixed graphs, asserted against *generous* budgets so a real algorithmic
//!      or complexity regression trips the test without it being timing-flaky.

use graph_compute::engines::geometric::{CoordinationSource, GeometricEngine, GeometricSettings};
use graph_compute::engines::{CsrShard, EngineCtx, LayoutEngine};
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
// 1. CANARY — solved cases with analytically known equilibria
// ---------------------------------------------------------------------------

#[test]
fn canary_single_spring_relaxes_to_rest_length() {
    // One spring, seeded 3× too long. The only equilibrium is the two nodes
    // exactly `rest_len` apart with zero residual force and zero energy.
    let rest = 2.0;
    let scn = Scenario {
        name: "single-spring",
        graph: single_edge(),
        settings: springs_only(rest),
        seed: vec![0.0, 0.0, 0.0, 3.0 * rest, 0.0, 0.0],
    };

    let r = relax(&scn, 5_000, 1e-3, 1);
    let at = r
        .converged_at
        .expect("single spring must reach residual < 1e-3");

    let d = dist(&r.final_positions, 0, 1);
    assert!(
        (d - rest).abs() < 5e-3,
        "equilibrium length {d} != rest {rest} (Δ={:.2e})",
        (d - rest).abs()
    );
    let pot = r.trajectory.last().unwrap().potential;
    assert!(
        pot < 1e-4,
        "spring potential should vanish at equilibrium, got {pot:.2e}"
    );
    assert!(
        at < 2_000,
        "single spring converged suspiciously slowly at step {at}"
    );
}

#[test]
fn canary_triangle_relaxes_to_equilateral() {
    // Three equal springs on a 3-cycle: the unique zero-energy state (up to
    // rigid motion) is the equilateral triangle with every side == rest_len.
    let rest = 1.5;
    let scn = Scenario {
        name: "triangle",
        graph: triangle(),
        // Seed a deliberately skewed, non-equilateral triangle.
        seed: vec![0.0, 0.0, 0.0, rest * 0.5, 0.0, 0.0, 0.2, 0.3, 0.0],
        settings: springs_only(rest),
    };

    let r = relax(&scn, 8_000, 2e-3, 1);
    r.converged_at.expect("triangle must reach residual < 2e-3");

    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
        let d = dist(&r.final_positions, a, b);
        assert!(
            (d - rest).abs() < 5e-3,
            "side {a}-{b} = {d} should equal rest {rest} (Δ={:.2e})",
            (d - rest).abs()
        );
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
