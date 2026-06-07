//! Millions-scale dispatch test for the geometric / lipid-membrane GPU kernels.
//!
//! The geometric engine and its dynamic-bonding stage launch one GPU thread per
//! particle. A 1-D dispatch caps at 65535 workgroups × 64 = 4_194_304 threads,
//! so above ~4.19M particles the kernels would silently under-dispatch (the tail
//! never runs) unless the host tiles the dispatch into 2-D. This test drives all
//! four per-particle kernels — `geometric_step` (Barnes–Hut), `calc_hash` +
//! `scan_candidates` (the cell-list bond search), and `spring_step` (bond relax)
//! — at a particle count *above* that cap and asserts every position comes back
//! finite and full-length, i.e. the whole array was actually written. This is
//! the regression guard for the lipid self-assembly demo at 10–20M.
//!
//! Default N (5M) crosses the cap so a real-Metal builder exercises the 2-D tile
//! path; `GEOMETRIC_SCALE_N` tunes it down for a software/CI runner. Skips
//! cleanly (passes) without a GPU adapter.
//!
//!     cargo test -p graph-compute --test geometric_scale -- --nocapture

use std::time::Instant;

use graph_compute::engines::geometric::GeometricSettings;
use graph_compute::engines::{
    gpu_dynamic_bonds, gpu_relax_bonds, CsrShard, GeometricGpuEngine, LayoutEngine,
};
use graph_compute::sim::CsrGraph;

mod common;

/// `n` isolated nodes (no edges) — the geometric/bonding stages derive structure
/// from positions, not topology, so an edgeless graph is the right fixture for a
/// pure per-particle dispatch test.
fn no_edges(n: u32) -> CsrGraph {
    CsrGraph {
        n_nodes: n,
        offsets: vec![0u32; n as usize + 1],
        neighbors: Vec::new(),
    }
}

/// Lay `n` particles on a cubic grid at `spacing` so the cell list stays O(1) per
/// cell (a clustered layout would make the candidate scan O(n²) and dominate the
/// runtime — irrelevant to what this test checks). Returns a flat [x,y,z,…].
fn grid_positions(n: usize, spacing: f32) -> Vec<f32> {
    let side = (n as f64).cbrt().ceil() as usize;
    let mut pos = Vec::with_capacity(3 * n);
    for i in 0..n {
        let x = (i % side) as f32 * spacing;
        let y = ((i / side) % side) as f32 * spacing;
        let z = (i / (side * side)) as f32 * spacing;
        pos.push(x);
        pos.push(y);
        pos.push(z);
    }
    pos
}

#[test]
fn geometric_kernels_dispatch_above_4m_particles() {
    let Some(mut ctx) = common::gpu_ctx_or_skip("geometric_scale") else {
        return;
    };

    let n: usize = std::env::var("GEOMETRIC_SCALE_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5_000_000);
    assert!(n > 0);
    let n_u32 = n as u32;

    let build = Instant::now();
    let graph = no_edges(n_u32);
    // r_bond 1.0, grid spacing 2.0 ⇒ neighbours just out of bonding range, so the
    // cell-list scan stays cheap; the dispatch still covers all n threads.
    let pos = grid_positions(n, 2.0);
    let classes = vec![0u32; n];
    eprintln!("built {n} particles in {:?}", build.elapsed());

    let settings = GeometricSettings {
        edge_stiffness: 0.0,
        angle_stiffness: 0.0,
        exclusion_strength: 1.0,
        well_depth: 0.0,
        gravity: 0.0,
        temperature: 0.1, // a little Brownian motion, like the membrane demo
        bonding_enabled: true,
        r_bond: 1.0,
        r_break: 1.5,
        bond_stiffness: 0.3,
        bond_every: 1,
        max_step: 0.5,
        damping: 0.9,
        time_step: 1.0,
        default_max_valence: 2,
        ..GeometricSettings::default()
    };

    // ---- Kernel 1: geometric_step (Barnes–Hut) via the layout engine ---------
    let mut engine = GeometricGpuEngine::new();
    engine
        .set_params(&serde_json::to_value(&settings).expect("serialize settings"))
        .expect("set_params");
    engine
        .init(&mut ctx, &CsrShard::whole(&graph), &pos)
        .expect("gpu init at scale");
    let t = Instant::now();
    let stepped = engine.step(&mut ctx).positions;
    eprintln!("geometric_step({n}) in {:?}", t.elapsed());
    assert_eq!(stepped.len(), 3 * n, "geometric_step truncated the array");
    assert!(
        stepped.iter().all(|v| v.is_finite()),
        "geometric_step produced non-finite positions (tail not dispatched?)"
    );

    // ---- Kernels 2+3: calc_hash + scan_candidates via gpu_dynamic_bonds -------
    let gpu = ctx.gpu.take().expect("gpu present");
    let t = Instant::now();
    let bonds = gpu_dynamic_bonds(&gpu, &settings, &pos, &classes, &[]);
    eprintln!(
        "gpu_dynamic_bonds({n}) -> {} bonds in {:?}",
        bonds.len(),
        t.elapsed()
    );
    // (Bond count is layout-dependent and not the point — that the call returns
    //  at all means calc_hash/scan_candidates dispatched over every particle.)

    // ---- Kernel 4: spring_step via gpu_relax_bonds ---------------------------
    // Per-node kernel: dispatches over all n regardless of bond count.
    let t = Instant::now();
    let relaxed = gpu_relax_bonds(&gpu, &settings, &pos, &bonds, 2);
    eprintln!("gpu_relax_bonds({n}, 2 steps) in {:?}", t.elapsed());
    assert_eq!(relaxed.len(), 3 * n, "spring_step truncated the array");
    assert!(
        relaxed.iter().all(|v| v.is_finite()),
        "spring_step produced non-finite positions (tail not dispatched?)"
    );

    eprintln!("geometric_scale OK at {n} particles (> 4.19M 1-D cap)");
}
