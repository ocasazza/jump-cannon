//! Correctness for `distributed_pagerank_gpu` — the GPU-per-partition scale-out
//! (domain decomposition + BSP halo exchange, each partition's gather a weighted
//! `gpu_spmv`). The committed lib tests cover the CPU `distributed_pagerank`;
//! this fills the gap for the GPU path, which the unit tests can't (no adapter
//! in the pure unit profile).
//!
//! Contract: distributed-on-GPU over any partition count P equals the
//! single-process `cpu_pagerank` (synchronous BSP ⇒ identical Jacobi iteration),
//! and equals the CPU `distributed_pagerank`. Gates on a real adapter (Metal
//! locally; lavapipe in CI).

use graph_compute::analytics::{cpu_pagerank, distributed_pagerank, distributed_pagerank_gpu};
use graph_compute::sim::CsrGraph;

mod common;

fn ring(n: u32) -> CsrGraph {
    let mut offsets = Vec::with_capacity((n + 1) as usize);
    let mut neighbors = Vec::new();
    for i in 0..n {
        offsets.push(neighbors.len() as u32);
        neighbors.push((i + n - 1) % n);
        neighbors.push((i + 1) % n);
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Connected, no-dangling: a ring + deterministic chords.
fn chorded_ring(n: u32) -> CsrGraph {
    use std::collections::BTreeSet;
    let mut edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for i in 0..n {
        let b = (i + 1) % n;
        edges.insert((i.min(b), i.max(b)));
        let c = i.wrapping_mul(31).wrapping_add(7) % n;
        if i != c {
            edges.insert((i.min(c), i.max(c)));
        }
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
    for (u, v) in edges {
        adj[u as usize].push(v);
        adj[v as usize].push(u);
    }
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for a in &adj {
        neighbors.extend_from_slice(a);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

#[test]
fn gpu_distributed_matches_single_and_cpu_distributed() {
    let Some(ctx) = common::gpu_ctx_or_skip("gpu_distributed") else {
        return;
    };
    for g in [ring(60), chorded_ring(120)] {
        let single = cpu_pagerank(&g, 0.85, 80);
        for p in [1u32, 2, 3, 5, 8] {
            let gpu_dist =
                distributed_pagerank_gpu(&ctx, &g, p, 0.85, 80).expect("gpu distributed");
            assert_eq!(gpu_dist.len(), single.len());

            // (a) GPU-distributed == single-process cpu_pagerank.
            let dev_single = gpu_dist
                .iter()
                .zip(single.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                dev_single < 1e-4,
                "n={} p={p}: gpu-distributed vs single max dev {dev_single}",
                g.n_nodes
            );

            // (b) GPU-distributed == CPU-distributed (same decomposition).
            let cpu_dist = distributed_pagerank(&g, p, 0.85, 80);
            let dev_cpu = gpu_dist
                .iter()
                .zip(cpu_dist.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                dev_cpu < 1e-4,
                "n={} p={p}: gpu vs cpu distributed dev {dev_cpu}",
                g.n_nodes
            );

            // (c) Mass conserved across partitions.
            let mass: f64 = gpu_dist.iter().map(|&r| r as f64).sum();
            assert!((mass - 1.0).abs() < 1e-3, "p={p}: mass {mass}");
        }
    }
}

#[test]
fn gpu_distributed_requires_adapter_errs_without_gpu() {
    // Sanity: the CPU-only ctx path returns Err (callers fall back).
    let ctx = graph_compute::EngineCtx::cpu_only();
    let g = ring(8);
    assert!(distributed_pagerank_gpu(&ctx, &g, 2, 0.85, 10).is_err());
}
