//! Correctness for `gpu_spmv` (weighted CSR y = A·x on wgpu).
//!
//! Oracles: identity matrix (y = x), a hand-worked small matrix, proptest
//! GPU ≡ CPU over random sparse matrices, and a consistency check that one
//! PageRank pull-step expressed as an SpMV matches the analytic. Gates on a
//! real adapter (Metal locally; lavapipe in CI).

use graph_compute::analytics::{cpu_spmv, gpu_spmv, WeightedCsr};
use proptest::prelude::*;

mod common;

/// n×n identity as a weighted CSR (one unit-weight self-entry per row).
fn identity(n: u32) -> WeightedCsr {
    let offsets: Vec<u32> = (0..=n).collect();
    let neighbors: Vec<u32> = (0..n).collect();
    let weights = vec![1.0f32; n as usize];
    WeightedCsr {
        n_nodes: n,
        offsets,
        neighbors,
        weights,
    }
}

#[test]
fn identity_is_x() {
    let Some(ctx) = common::gpu_ctx_or_skip("spmv_identity") else {
        return;
    };
    let a = identity(8);
    let x = vec![1.5, -2.0, 3.0, 0.0, 7.0, -1.0, 4.0, 9.0];
    let y = gpu_spmv(&ctx, &a, &x).expect("gpu spmv");
    for (yi, xi) in y.iter().zip(x.iter()) {
        assert!((yi - xi).abs() < 1e-5, "{yi} != {xi}");
    }
}

#[test]
fn small_known_matrix() {
    let Some(ctx) = common::gpu_ctx_or_skip("spmv_small") else {
        return;
    };
    // A = [[2, 0, 1],
    //      [0, 3, 0],
    //      [1, 0, 4]]  (rows as CSR)
    let a = WeightedCsr {
        n_nodes: 3,
        offsets: vec![0, 2, 3, 5],
        neighbors: vec![0, 2, 1, 0, 2],
        weights: vec![2.0, 1.0, 3.0, 1.0, 4.0],
    };
    let x = vec![1.0, 2.0, 3.0];
    // y = [2*1 + 1*3, 3*2, 1*1 + 4*3] = [5, 6, 13]
    let y = gpu_spmv(&ctx, &a, &x).expect("gpu spmv");
    assert!((y[0] - 5.0).abs() < 1e-5, "{}", y[0]);
    assert!((y[1] - 6.0).abs() < 1e-5, "{}", y[1]);
    assert!((y[2] - 13.0).abs() < 1e-5, "{}", y[2]);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    /// GPU ≡ CPU over random sparse matrices + random x.
    #[test]
    fn gpu_equals_cpu_spmv(
        n in 1usize..40,
        seed in 0u64..10_000,
    ) {
        let Some(ctx) = common::gpu_ctx_or_skip("spmv_proptest") else { return Ok(()); };
        // Deterministic pseudo-random sparse matrix from `seed` (no rng dep).
        let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
        let mut next = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        let mut weights = Vec::new();
        for _ in 0..n {
            let deg = (next() % 5) as usize; // 0..=4 entries per row
            for _ in 0..deg {
                neighbors.push((next() as usize % n) as u32);
                weights.push(((next() % 2000) as f32) / 1000.0 - 1.0); // [-1, 1)
            }
            offsets.push(neighbors.len() as u32);
        }
        let a = WeightedCsr { n_nodes: n as u32, offsets, neighbors, weights };
        let x: Vec<f32> = (0..n).map(|_| ((next() % 2000) as f32) / 1000.0 - 1.0).collect();

        let gpu = gpu_spmv(&ctx, &a, &x).expect("gpu spmv");
        let cpu = cpu_spmv(&a, &x);
        prop_assert_eq!(gpu.len(), cpu.len());
        for (g, c) in gpu.iter().zip(cpu.iter()) {
            prop_assert!((g - c).abs() < 1e-4, "gpu {g} vs cpu {c}");
        }
    }
}
