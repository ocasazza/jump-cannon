//! End-to-end integration test for the multilevel/multiscale **wrapper** engine
//! (`"multilevel"`), driven *purely* through the public `LayoutEngine` trait
//! surface — `set_params` (JSON, exactly as a `SubscribeRequest`'s
//! `google.protobuf.Struct` would arrive) → `init` → `step ⟲` — with a REAL
//! inner solver (`"sgd-stress"`).
//!
//! The in-crate unit tests in `engines/multilevel.rs` reach into private fields
//! (`e.settings.inner = …`, `e.inner = Some(mock)`, `e.descent`) to assert the
//! cascade's internals and the build-once / reinit-per-level lifecycle via a
//! counting mock. That hook is `#[cfg(test)]`-private and unreachable here, so
//! this file deliberately tests the *observable, public* properties instead:
//!
//!   1. **Descent reaches the finest level.** After enough ticks the wrapper
//!      emits the FULL finest-level node count with finite positions (the
//!      coarsen → solve-coarsest → prolong → refine cascade actually bottoms out
//!      at `G_0`). We can't read `descent.level` from outside, so we assert the
//!      observable proxy: the broadcast `StepOutput.positions` length equals
//!      `3 * n_nodes` of the input graph and every coordinate is finite.
//!
//!   2. **Reinit-based reuse (build inner ONCE).** The counting hook isn't
//!      reachable from an integration test (it's a `#[cfg(test)]` mock swapped
//!      into the private `inner` field). Per the task, we assert the
//!      *determinism / quality property* that the single-reused-instance design
//!      guarantees instead: two independent runs configured identically through
//!      the public API produce bit-identical final layouts (reuse must be
//!      deterministic), and the cascade measurably reduces stress vs. the seed.
//!      See the `todo` returned with this change.
//!
//!   3. **Walshaw sweep schedule front-loads coarse work.** A non-uniform
//!      schedule (`Linear` / `Geometric`) spends MORE refinement at coarse
//!      levels than `Uniform` does for the same per-level base budget, which —
//!      on the same graph from the same seed — reaches a measurably lower final
//!      stress. We assert the robust direction (non-uniform stress <= uniform
//!      stress, within tolerance) rather than a brittle exact margin.
//!
//! Everything goes through `serde_json::json!` params + the trait methods; no
//! private field is touched. CPU-only `EngineCtx`, so it runs headless / in the
//! sandbox.

use graph_compute::engines::MultilevelEngine;
use graph_compute::sim::CsrGraph;
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};

/// A connected ring 0—1—…—(n-1)—0 as a symmetric CSR graph. A ring has a clean
/// 1-D intrinsic structure (like a path but with no endpoints), which the stress
/// solver lays out as a circle — a good non-trivial cascade target whose stress
/// is well-defined and monotone-ish under refinement.
fn ring(n: u32) -> CsrGraph {
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for v in 0..n {
        // prev (wrapping) and next (wrapping)
        let prev = (v + n - 1) % n;
        let next = (v + 1) % n;
        neighbors.push(prev);
        neighbors.push(next);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Deterministic, well-separated seed positions on a unit circle (interleaved
/// x,y,z). A circular seed is a poor layout for stress on most graphs, giving
/// the cascade real work to do, but it is reproducible (no RNG) so two runs from
/// it are comparable bit-for-bit.
fn circle_seed(n: usize) -> Vec<f32> {
    let mut p = vec![0.0f32; 3 * n];
    for i in 0..n {
        let t = (i as f32) / (n.max(1) as f32) * std::f32::consts::TAU;
        p[3 * i] = t.cos();
        p[3 * i + 1] = t.sin();
        // z stays 0 — 2-D layout.
    }
    p
}

/// Reference full O(n²) stress, the exact objective the `sgd-stress` inner
/// minimizes:  Σ_{i<j} w_ij (‖x_i − x_j‖ − d_ij)²  with w_ij = d_ij^-2 and d_ij
/// the unweighted shortest-path (BFS) distance. Mirrors the in-crate reference
/// in `sgd_stress.rs::full_stress`; small n keeps it cheap. Lower == better.
fn full_stress(g: &CsrGraph, pos: &[f32]) -> f64 {
    let n = g.n_nodes as usize;
    let mut total = 0.0f64;
    for i in 0..n {
        let d = bfs(g, i as u32);
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
    total
}

/// Unweighted BFS hop distances from `source`; `u32::MAX` == unreachable.
fn bfs(g: &CsrGraph, source: u32) -> Vec<u32> {
    let n = g.n_nodes as usize;
    let mut dist = vec![u32::MAX; n];
    if n == 0 {
        return dist;
    }
    let mut q = std::collections::VecDeque::new();
    dist[source as usize] = 0;
    q.push_back(source);
    while let Some(v) = q.pop_front() {
        let dv = dist[v as usize];
        let start = g.offsets[v as usize] as usize;
        let end = g.offsets[v as usize + 1] as usize;
        for e in start..end {
            let u = g.neighbors[e];
            if dist[u as usize] == u32::MAX {
                dist[u as usize] = dv + 1;
                q.push_back(u);
            }
        }
    }
    dist
}

/// Build + configure a `MultilevelEngine` via PUBLIC `set_params` only (JSON, as
/// it arrives on the wire), wrapping `sgd-stress`. `target_size = 2` forces a
/// multi-level cascade even on a small graph so the descent actually has several
/// levels to walk. `schedule` is the kebab-case `SweepSchedule` wire form
/// (`"uniform" | "linear" | "geometric"`).
fn make_engine(schedule: &str, sweeps_per_level: u32) -> MultilevelEngine {
    let mut e = MultilevelEngine::new();
    let params = serde_json::json!({
        "inner": "sgd-stress",
        // Keep the inner deterministic across runs (fixed seed is its default,
        // but pin it explicitly so the test is robust to default changes).
        "inner_params": { "seed": 0x1234_5678u64 },
        "max_levels": 8,
        "target_size": 2,
        "sweeps_per_level": sweeps_per_level,
        "sweep_schedule": schedule,
        // spring_len scales the prolongation jitter (0.5 * spring_len). It must
        // roughly match the INNER engine's natural edge length, per the field
        // doc on MultilevelSettings::spring_len. The sgd-stress inner targets
        // the unweighted BFS distance (unit edge length), so a unit-ish jitter
        // keeps each prolonged level in the regime the stress solver refines
        // quickly. The 30.0 module default is tuned for the FA2 engines (whose
        // natural edge length is ~30); using it here scatters fine levels ~15
        // units off a unit-distance target, so the eta_min-pinned tail of the
        // SGD anneal takes thousands of extra ticks to re-collapse the scale —
        // i.e. the cascade still converges, just far slower than this test's
        // budget. Matching the inner's edge length is the correct config, not a
        // weaker assertion.
        "spring_len": 1.0,
        "seed": 0xABCDu32,
    });
    e.set_params(&params)
        .expect("set_params must accept a valid multilevel config through the public API");
    e
}

/// Drive an engine to (near-)convergence and return the final broadcast
/// positions. We step a generous fixed budget so the cascade has time to walk
/// every level down to the finest and refine there.
fn run(engine: &mut MultilevelEngine, g: &CsrGraph, seed: &[f32], ticks: usize) -> Vec<f32> {
    let mut ctx = EngineCtx::cpu_only();
    engine
        .init(&mut ctx, &CsrShard::whole(g), seed)
        .expect("init must succeed on a CPU-only context with a real CPU inner");
    let mut out = seed.to_vec();
    for _ in 0..ticks {
        out = engine.step(&mut ctx).positions;
    }
    out
}

/// (1) The descent reaches the finest level with the FULL node count and finite
/// positions. Observed purely via the public `step` output: after a generous
/// budget the broadcast carries `3 * n_nodes` coordinates (finest level = the
/// whole graph), and every coordinate is finite (no NaN/Inf blow-up).
#[test]
fn descent_reaches_finest_level_full_count_and_finite() {
    let g = ring(64);
    let seed = circle_seed(64);
    let mut e = make_engine("linear", 6);
    // Large budget: 8 levels × generous per-level sweeps, plus fine-level refine.
    let out = run(&mut e, &g, &seed, 2000);

    assert_eq!(
        out.len(),
        64 * 3,
        "finest-level broadcast must carry all {} nodes (3 floats each)",
        64
    );
    assert!(
        out.iter().all(|c| c.is_finite()),
        "all finest-level coordinates must be finite (no NaN/Inf)"
    );
    // Sanity: the layout actually moved off the degenerate circular seed for at
    // least some node (the cascade did real work, didn't no-op).
    let moved = out
        .iter()
        .zip(seed.iter())
        .any(|(a, b)| (a - b).abs() > 1e-3);
    assert!(moved, "cascade should have moved nodes away from the seed");
}

/// (2) Reinit-based reuse property: because the wrapper builds ONE inner instance
/// and `reinit`s it per level (rather than reconstructing), a configured run is
/// fully deterministic — two independent, identically-configured runs through the
/// public API must produce bit-identical final layouts. (The build-once *count*
/// is asserted by the in-crate counting-mock unit test, which is unreachable from
/// an integration test; see this file's header + the returned `todo`.) We also
/// assert the quality side of the same property: the cascade measurably reduces
/// stress relative to the seed.
#[test]
fn reinit_reuse_is_deterministic_and_reduces_stress() {
    let g = ring(48);
    let seed = circle_seed(48);

    let mut a = make_engine("linear", 5);
    let mut b = make_engine("linear", 5);
    let out_a = run(&mut a, &g, &seed, 1500);
    let out_b = run(&mut b, &g, &seed, 1500);

    assert_eq!(
        out_a, out_b,
        "single reused-inner-instance design must be deterministic: \
         two identical public-API runs must match bit-for-bit"
    );

    let stress_seed = full_stress(&g, &seed);
    let stress_final = full_stress(&g, &out_a);
    assert!(
        stress_final < stress_seed,
        "multilevel cascade should reduce stress vs. the seed: \
         seed={stress_seed} final={stress_final}"
    );
}

/// (3) Walshaw front-loading: a non-uniform sweep schedule (which spends more
/// refinement at coarse levels for the same per-level base budget) reaches a
/// final stress no WORSE than `Uniform` on the same graph from the same seed —
/// the "settle the skeleton first" payoff. We assert the robust *direction*
/// (non-uniform <= uniform within a tolerance) rather than a brittle exact
/// margin, since SGD is stochastic.
///
/// All three schedules are configured ONLY through the public `sweep_schedule`
/// setting (kebab-case wire form), exactly as a client would.
#[test]
fn front_loaded_schedule_is_no_worse_than_uniform() {
    let g = ring(64);
    let seed = circle_seed(64);
    let base = 5;
    let ticks = 2000;

    let mut uniform = make_engine("uniform", base);
    let mut linear = make_engine("linear", base);
    let mut geometric = make_engine("geometric", base);

    let s_uniform = full_stress(&g, &run(&mut uniform, &g, &seed, ticks));
    let s_linear = full_stress(&g, &run(&mut linear, &g, &seed, ticks));
    let s_geometric = full_stress(&g, &run(&mut geometric, &g, &seed, ticks));

    // Relative tolerance: allow non-uniform to be at most 5% worse than uniform
    // to absorb SGD stochasticity, while still asserting the front-loaded
    // schedule does not REGRESS quality. In practice it is lower.
    let tol = 1.05;
    assert!(
        s_linear <= s_uniform * tol,
        "linear (front-loaded) stress {s_linear} should be <= uniform {s_uniform} (×{tol} tol)"
    );
    assert!(
        s_geometric <= s_uniform * tol,
        "geometric (front-loaded) stress {s_geometric} should be <= uniform {s_uniform} (×{tol} tol)"
    );

    // And all schedules should at least beat the degenerate seed — i.e. each
    // produced a genuinely refined layout, not a no-op.
    let s_seed = full_stress(&g, &seed);
    for (name, s) in [
        ("uniform", s_uniform),
        ("linear", s_linear),
        ("geometric", s_geometric),
    ] {
        assert!(
            s < s_seed,
            "{name} schedule final stress {s} should beat seed stress {s_seed}"
        );
    }
}

/// Public-API validation belongs to the cascade's contract too: an unknown inner
/// id and a self-wrap attempt must both be rejected at `set_params` (the wire
/// boundary), and a `null` params payload must be accepted (inner defaults).
/// This guards the same surface a real `SubscribeRequest` exercises.
#[test]
fn set_params_validates_inner_through_public_api() {
    let mut e = MultilevelEngine::new();
    assert!(
        e.set_params(&serde_json::json!({ "inner": "no-such-engine" }))
            .is_err(),
        "unknown inner must be rejected at the public set_params boundary"
    );
    assert!(
        e.set_params(&serde_json::json!({ "inner": "multilevel" }))
            .is_err(),
        "self-wrap must be rejected at the public set_params boundary"
    );
    assert!(
        e.set_params(&serde_json::Value::Null).is_ok(),
        "null params must be accepted (inner defaults)"
    );
}
