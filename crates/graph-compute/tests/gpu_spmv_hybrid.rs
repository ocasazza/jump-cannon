//! Correctness for `gpu_spmv_hybrid` — the hub-aware (workgroup-per-row,
//! load-balanced) weighted SpMV for power-law graphs.
//!
//! It must produce the SAME result as the thread-per-row `gpu_spmv` baseline
//! (and the `cpu_spmv` oracle), up to f32 summation-order tolerance (~1e-4) —
//! the reduction order differs because long rows are summed cooperatively by a
//! whole workgroup rather than serially by one thread.
//!
//! Cases: a hand-worked small matrix, proptest GPU≡GPU≡CPU over random sparse
//! matrices, and a power-law matrix with a few very-high-degree hub rows — the
//! case this variant targets. Gates on a real adapter (Metal locally; lavapipe
//! in CI).

use graph_compute::analytics::{cpu_spmv, gpu_spmv, gpu_spmv_hybrid, WeightedCsr};
use proptest::prelude::*;

mod common;

/// Build a power-law-ish CSR: `n` nodes, `hubs` of them adjacent to (almost) all
/// nodes, the rest with small degree. Weights/x derived deterministically from a
/// seed so the test is reproducible without an rng dep.
fn power_law(n: u32, hubs: u32, seed: u64) -> (WeightedCsr, Vec<f32>) {
    let n_us = n as usize;
    let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    let mut weights = Vec::new();
    for v in 0..n {
        if v < hubs {
            // Hub row: adjacent to every node (degree n) — the long pole that
            // the thread-per-row baseline serializes on one thread.
            for c in 0..n {
                neighbors.push(c);
                weights.push(((next() % 2000) as f32) / 1000.0 - 1.0);
            }
        } else {
            // Short row: 0..=3 random entries.
            let deg = (next() % 4) as usize;
            for _ in 0..deg {
                neighbors.push((next() as usize % n_us) as u32);
                weights.push(((next() % 2000) as f32) / 1000.0 - 1.0);
            }
        }
        offsets.push(neighbors.len() as u32);
    }
    let x: Vec<f32> = (0..n_us)
        .map(|_| ((next() % 2000) as f32) / 1000.0 - 1.0)
        .collect();
    (
        WeightedCsr {
            n_nodes: n,
            offsets,
            neighbors,
            weights,
        },
        x,
    )
}

#[test]
fn small_known_matrix() {
    let Some(ctx) = common::gpu_ctx_or_skip("spmv_hybrid_small") else {
        return;
    };
    // A = [[2, 0, 1],
    //      [0, 3, 0],
    //      [1, 0, 4]]
    let a = WeightedCsr {
        n_nodes: 3,
        offsets: vec![0, 2, 3, 5],
        neighbors: vec![0, 2, 1, 0, 2],
        weights: vec![2.0, 1.0, 3.0, 1.0, 4.0],
    };
    let x = vec![1.0, 2.0, 3.0];
    // y = [2*1 + 1*3, 3*2, 1*1 + 4*3] = [5, 6, 13]
    let y = gpu_spmv_hybrid(&ctx, &a, &x).expect("gpu spmv hybrid");
    assert!((y[0] - 5.0).abs() < 1e-4, "{}", y[0]);
    assert!((y[1] - 6.0).abs() < 1e-4, "{}", y[1]);
    assert!((y[2] - 13.0).abs() < 1e-4, "{}", y[2]);
}

#[test]
fn power_law_hubs_match_baseline() {
    let Some(ctx) = common::gpu_ctx_or_skip("spmv_hybrid_powerlaw") else {
        return;
    };
    // 200 nodes, 3 hubs each adjacent to all 200 nodes (the case the variant
    // targets) + 197 short rows. Hybrid must match both baseline and CPU.
    let (a, x) = power_law(200, 3, 42);
    let hybrid = gpu_spmv_hybrid(&ctx, &a, &x).expect("hybrid");
    let baseline = gpu_spmv(&ctx, &a, &x).expect("baseline");
    let cpu = cpu_spmv(&a, &x);
    assert_eq!(hybrid.len(), cpu.len());
    for i in 0..hybrid.len() {
        assert!(
            (hybrid[i] - baseline[i]).abs() < 1e-4,
            "row {i}: hybrid {} vs baseline {}",
            hybrid[i],
            baseline[i]
        );
        assert!(
            (hybrid[i] - cpu[i]).abs() < 1e-4,
            "row {i}: hybrid {} vs cpu {}",
            hybrid[i],
            cpu[i]
        );
    }
    // Sanity: the hub rows really are long (degree == n), i.e. we exercised the
    // cooperative-reduction path, not just short rows.
    assert_eq!(a.offsets[1] - a.offsets[0], 200);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    /// hybrid ≡ baseline ≡ CPU over random sparse matrices + random x.
    #[test]
    fn hybrid_equals_baseline_and_cpu(
        n in 1usize..40,
        seed in 0u64..10_000,
    ) {
        let Some(ctx) = common::gpu_ctx_or_skip("spmv_hybrid_proptest") else { return Ok(()); };
        let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
        let mut next = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        let mut weights = Vec::new();
        for _ in 0..n {
            let deg = (next() % 5) as usize; // 0..=4 entries per row
            for _ in 0..deg {
                neighbors.push((next() as usize % n) as u32);
                weights.push(((next() % 2000) as f32) / 1000.0 - 1.0);
            }
            offsets.push(neighbors.len() as u32);
        }
        let a = WeightedCsr { n_nodes: n as u32, offsets, neighbors, weights };
        let x: Vec<f32> = (0..n).map(|_| ((next() % 2000) as f32) / 1000.0 - 1.0).collect();

        let hybrid = gpu_spmv_hybrid(&ctx, &a, &x).expect("hybrid");
        let baseline = gpu_spmv(&ctx, &a, &x).expect("baseline");
        let cpu = cpu_spmv(&a, &x);
        prop_assert_eq!(hybrid.len(), cpu.len());
        for i in 0..hybrid.len() {
            prop_assert!((hybrid[i] - baseline[i]).abs() < 1e-4, "hybrid {} vs baseline {}", hybrid[i], baseline[i]);
            prop_assert!((hybrid[i] - cpu[i]).abs() < 1e-4, "hybrid {} vs cpu {}", hybrid[i], cpu[i]);
        }
    }
}
