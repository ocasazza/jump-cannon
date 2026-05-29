//! GPU layout-engine correctness tests (`fa2-brute`, `fa2-bh`).
//!
//! These need a real wgpu adapter (Metal/Vulkan/DX12). Under the default
//! command sandbox no adapter is visible, so each test SKIPS (returns early and
//! passes) — keeping the in-sandbox suite green. Run them for real with the
//! sandbox disabled or in a GPU CI lane:
//!
//! ```text
//! cargo test -p graph-compute --test gpu_engines
//! ```
//!
//! The headline check is [`barnes_hut_matches_brute_force_single_step`]: with the
//! Barnes-Hut acceptance criterion `theta = 0` the octree visits every leaf, so
//! its repulsion must equal the brute-force O(n²) engine up to floating-point
//! summation order — the property that proves the tree traversal is correct.

use graph_compute::engines::{Fa2BhEngine, Fa2BruteEngine};
use graph_compute::sim::CsrGraph;
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};

/// Deterministic, well-spread seed layout (golden-angle / sunflower spiral) so
/// no two nodes coincide — coincident nodes make repulsion degenerate.
fn seed(n: usize) -> Vec<f32> {
    let golden_angle = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let mut p = Vec::with_capacity(3 * n);
    for i in 0..n {
        let r = (i as f32 + 1.0).sqrt();
        let a = i as f32 * golden_angle;
        p.push(r * a.cos());
        p.push(r * a.sin());
        p.push(0.0);
    }
    p
}

/// Bring up a GPU context, or print a skip note and return `None` (so the test
/// passes trivially on sandboxed/headless hosts with no adapter).
fn gpu_or_skip(test: &str) -> Option<EngineCtx> {
    let ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_none() {
        eprintln!("SKIP {test}: no wgpu adapter (sandboxed/headless host)");
        return None;
    }
    Some(ctx)
}

/// Init an engine on the whole graph and run `steps` ticks, returning the final
/// interleaved x,y,z positions.
fn run(
    engine: &mut dyn LayoutEngine,
    ctx: &mut EngineCtx,
    graph: &CsrGraph,
    seed: &[f32],
    steps: usize,
) -> Vec<f32> {
    engine
        .init(ctx, &CsrShard::whole(graph), seed)
        .expect("engine init on GPU");
    let mut out = seed.to_vec();
    for _ in 0..steps {
        out = engine.step(ctx).positions;
    }
    out
}

#[test]
fn gpu_engines_run_and_move() {
    let Some(mut ctx) = gpu_or_skip("gpu_engines_run_and_move") else {
        return;
    };
    let graph = CsrGraph::path(32);
    let s = seed(32);

    for (name, mut engine) in [
        (
            "fa2-brute",
            Box::new(Fa2BruteEngine::new()) as Box<dyn LayoutEngine>,
        ),
        ("fa2-bh", Box::new(Fa2BhEngine::new()) as Box<dyn LayoutEngine>),
    ] {
        let out = run(engine.as_mut(), &mut ctx, &graph, &s, 30);
        assert_eq!(out.len(), s.len(), "{name}: position count preserved");
        assert!(
            out.iter().all(|x| x.is_finite()),
            "{name}: all positions finite after 30 steps"
        );
        let moved: f32 = out.iter().zip(&s).map(|(a, b)| (a - b).abs()).sum();
        assert!(
            moved > 1e-3,
            "{name}: layout should evolve from the seed (total movement = {moved})"
        );
    }
}

// This MUST pass. It currently FAILS on a real GPU (max diff ≈ 36 on a seed of
// radius ~5): at theta=0 the Barnes-Hut engine's repulsion is effectively absent
// (the layout is gravity-dominated, collapsing toward the origin) whereas brute
// force correctly pushes nodes outward. fa2-bh's octree repulsion is broken — it
// was authored without a GPU and never functionally tested. Left FAILING on
// purpose until the octree repulsion is fixed; do not #[ignore] it.
#[test]
fn barnes_hut_matches_brute_force_single_step() {
    let Some(mut ctx) = gpu_or_skip("barnes_hut_matches_brute_force_single_step") else {
        return;
    };
    let graph = CsrGraph::path(24);
    let s = seed(24);

    // Brute force: exact O(n²) repulsion.
    let brute = run(&mut Fa2BruteEngine::new(), &mut ctx, &graph, &s, 1);

    // Barnes-Hut with theta = 0 ⇒ every leaf visited ⇒ no approximation.
    let mut bh = Fa2BhEngine::new();
    bh.set_params(&serde_json::json!({ "theta": 0.0 }))
        .expect("set theta=0");
    let bh_pos = run(&mut bh, &mut ctx, &graph, &s, 1);

    assert_eq!(brute.len(), bh_pos.len());
    let max_diff = brute
        .iter()
        .zip(&bh_pos)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_diff < 5e-2,
        "Barnes-Hut (theta=0) must match brute force up to float summation order; \
         max per-coordinate diff = {max_diff}"
    );
}
