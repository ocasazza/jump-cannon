//! GPU-per-partition distributed PageRank: the multi-GPU scale-out path
//! (partitioned GPU SpMV + halo exchange) must match single-process
//! `cpu_pagerank` for any partition count. Gates on a real adapter (Metal
//! locally; lavapipe in CI).

use graph_compute::analytics::{cpu_pagerank, distributed_pagerank_gpu};
use graph_compute::sim::CsrGraph;

mod common;

fn chorded_ring(n: u32) -> CsrGraph {
    use std::collections::BTreeSet;
    let mut edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for i in 0..n {
        edges.insert((i.min((i + 1) % n), i.max((i + 1) % n)));
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

/// `chorded_ring(core)` plus `n_isolated` appended degree-0 (dangling) nodes.
fn ring_with_isolated(core: u32, n_isolated: u32) -> CsrGraph {
    let base = chorded_ring(core);
    let mut offsets = base.offsets.clone();
    let last = *offsets.last().unwrap();
    for _ in 0..n_isolated {
        offsets.push(last);
    }
    CsrGraph {
        n_nodes: core + n_isolated,
        offsets,
        neighbors: base.neighbors,
    }
}

#[test]
fn gpu_distributed_matches_single_process() {
    let Some(ctx) = common::gpu_ctx_or_skip("gpu_distributed_pagerank") else {
        return;
    };
    let g = chorded_ring(120);
    let single = cpu_pagerank(&g, 0.85, 80);
    for p in [1u32, 2, 4, 7] {
        let dist = distributed_pagerank_gpu(&ctx, &g, p, 0.85, 80).expect("gpu distributed");
        assert_eq!(dist.len(), single.len());
        let max_dev = dist
            .iter()
            .zip(single.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_dev < 1e-4,
            "p={p}: GPU distributed vs single max dev {max_dev}"
        );
        let mass: f64 = dist.iter().map(|&r| r as f64).sum();
        assert!((mass - 1.0).abs() < 1e-3, "p={p}: mass {mass}");
    }
}

/// GPU distributed PageRank must also match `cpu_pagerank` on graphs WITH
/// dangling (degree-0 / isolated) nodes — the global dangling-mass
/// redistribution runs identically on the GPU-per-partition path.
#[test]
fn gpu_distributed_matches_single_process_with_dangling() {
    let Some(ctx) = common::gpu_ctx_or_skip("gpu_distributed_pagerank_dangling") else {
        return;
    };
    for g in [ring_with_isolated(60, 5), ring_with_isolated(30, 20)] {
        let n_dangling = (0..g.n_nodes as usize)
            .filter(|&v| g.offsets[v + 1] == g.offsets[v])
            .count();
        assert!(n_dangling > 0, "test graph has no dangling nodes");

        let single = cpu_pagerank(&g, 0.85, 80);
        for p in [1u32, 2, 3] {
            let dist = distributed_pagerank_gpu(&ctx, &g, p, 0.85, 80).expect("gpu distributed");
            assert_eq!(dist.len(), single.len());
            let max_dev = dist
                .iter()
                .zip(single.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                max_dev < 1e-4,
                "n={} p={p}: GPU distributed-with-dangling vs single max dev {max_dev}",
                g.n_nodes
            );
            let mass: f64 = dist.iter().map(|&r| r as f64).sum();
            assert!((mass - 1.0).abs() < 1e-3, "p={p}: mass {mass}");
        }
    }
}
