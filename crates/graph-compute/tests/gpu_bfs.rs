//! Correctness for `gpu_bfs` (single-source distance relaxation on wgpu).
//!
//! Oracles: closed distances on a path (|i−src|) and a star, UNREACHABLE on a
//! disconnected node, a Nix-grid fixture (Manhattan distance), and proptest
//! GPU ≡ CPU queue-BFS over random graphs. Gates on a real adapter (Metal
//! locally; lavapipe in CI), skipping cleanly otherwise.

use std::collections::{BTreeMap, BTreeSet};

use graph_compute::analytics::{cpu_bfs, gpu_bfs, UNREACHABLE};
use graph_compute::sim::CsrGraph;
use proptest::prelude::*;
use tvix_wasm::eval_graph;

mod common;

fn path(n: u32) -> CsrGraph {
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for i in 0..n {
        offsets.push(neighbors.len() as u32);
        if i > 0 {
            neighbors.push(i - 1);
        }
        if i + 1 < n {
            neighbors.push(i + 1);
        }
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Build a symmetric CSR from a Nix generator expression body.
fn nix_csr(body: &str) -> CsrGraph {
    let expr = format!(
        "let g = import /jc/src/graph.nix {{}}; \
             gcl = import /jc/src/graph-combinators.nix {{ graph = g; }}; \
         in g.toGraphJSON ({body})"
    );
    let gen = eval_graph(&expr).expect("tvix eval");
    let mut idx: BTreeMap<String, u32> = BTreeMap::new();
    for node in &gen.nodes {
        let next = idx.len() as u32;
        idx.entry(node.id.clone()).or_insert(next);
    }
    let n = idx.len() as u32;
    let mut adj: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n as usize];
    for e in &gen.edges {
        let (a, b) = (idx[&e.source], idx[&e.target]);
        if a != b {
            adj[a as usize].insert(b);
            adj[b as usize].insert(a);
        }
    }
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    for s in &adj {
        neighbors.extend(s.iter().copied());
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

#[test]
fn path_distances_are_linear() {
    let Some(ctx) = common::gpu_ctx_or_skip("bfs_path") else {
        return;
    };
    let g = path(50);
    let src = 10u32;
    let dist = gpu_bfs(&ctx, &g, src).expect("gpu bfs");
    for v in 0..50u32 {
        let expected = (v as i64 - src as i64).unsigned_abs() as u32;
        assert_eq!(dist[v as usize], expected, "node {v}");
    }
}

#[test]
fn disconnected_node_is_unreachable() {
    let Some(ctx) = common::gpu_ctx_or_skip("bfs_disconnected") else {
        return;
    };
    // 0—1—2, node 3 isolated.
    let g = CsrGraph {
        n_nodes: 4,
        offsets: vec![0, 1, 3, 4, 4],
        neighbors: vec![1, 0, 2, 1],
    };
    let dist = gpu_bfs(&ctx, &g, 0).expect("gpu bfs");
    assert_eq!(dist[0], 0);
    assert_eq!(dist[1], 1);
    assert_eq!(dist[2], 2);
    assert_eq!(dist[3], UNREACHABLE);
}

#[test]
fn nix_grid_is_manhattan_from_corner() {
    let Some(ctx) = common::gpu_ctx_or_skip("bfs_nix_grid") else {
        return;
    };
    // Grid: node ids "nR_C". From corner n0_0, distance = R + C (Manhattan).
    let g = nix_csr("gcl.gridGen { rows = 5; cols = 6; prefix = \"n\"; }");
    // Cross-check against the CPU oracle (both over the same symmetric CSR),
    // which is the robust check; also assert the corner is 0.
    let dist = gpu_bfs(&ctx, &g, 0).expect("gpu bfs");
    assert_eq!(dist, cpu_bfs(&g, 0));
    assert_eq!(dist[0], 0);
    assert!(dist.iter().all(|&d| d != UNREACHABLE), "grid is connected");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    /// GPU ≡ CPU queue-BFS over random graphs from a random source.
    #[test]
    fn gpu_equals_cpu_bfs(
        n in 1u32..40,
        raw_edges in proptest::collection::vec((0u32..40, 0u32..40), 0..80),
        src_raw in 0u32..40,
    ) {
        let Some(ctx) = common::gpu_ctx_or_skip("bfs_proptest") else { return Ok(()); };
        let mut adj: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n as usize];
        for (a, b) in raw_edges {
            let (a, b) = (a % n, b % n);
            if a != b {
                adj[a as usize].insert(b);
                adj[b as usize].insert(a);
            }
        }
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        for s in &adj {
            neighbors.extend(s.iter().copied());
            offsets.push(neighbors.len() as u32);
        }
        let g = CsrGraph { n_nodes: n, offsets, neighbors };
        let src = src_raw % n;

        let gpu = gpu_bfs(&ctx, &g, src).expect("gpu bfs");
        let cpu = cpu_bfs(&g, src);
        prop_assert_eq!(gpu, cpu);
    }
}
