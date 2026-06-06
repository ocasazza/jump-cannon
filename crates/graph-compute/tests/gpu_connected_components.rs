//! Correctness for `gpu_connected_components` (min-label propagation on wgpu).
//!
//! Oracles: GPU ≡ CPU union-find over proptest-generated graphs, closed
//! component counts on hand-built shapes (disjoint cliques, a single path), and
//! a Nix-fixture cross-check (a disjoint union of two cycles from the shared
//! graph.nix generators ⇒ exactly 2 components). Gates on a real adapter (Metal
//! locally; lavapipe software-Vulkan in CI), skipping cleanly otherwise.

use std::collections::{BTreeMap, BTreeSet};

use graph_compute::analytics::{cpu_connected_components, gpu_connected_components};
use graph_compute::sim::CsrGraph;
use proptest::prelude::*;
use tvix_wasm::eval_graph;

mod common;

/// Number of distinct component labels.
fn num_components(labels: &[u32]) -> usize {
    labels.iter().copied().collect::<BTreeSet<_>>().len()
}

/// `k` disjoint cliques of `size` nodes each (symmetric, fully-connected blocks).
fn disjoint_cliques(k: u32, size: u32) -> CsrGraph {
    let n = k * size;
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for v in 0..n {
        offsets.push(neighbors.len() as u32);
        let block = v / size;
        for j in (block * size)..((block + 1) * size) {
            if j != v {
                neighbors.push(j);
            }
        }
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Undirected path 0—1—…—(n-1): one component, every node reachable.
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

#[test]
fn disjoint_cliques_have_k_components() {
    let Some(ctx) = common::gpu_ctx_or_skip("cc_disjoint_cliques") else {
        return;
    };
    let g = disjoint_cliques(5, 7); // 35 nodes, 5 components
    let labels = gpu_connected_components(&ctx, &g).expect("gpu cc");
    assert_eq!(num_components(&labels), 5);
    // Each clique's label is its min node index (0, 7, 14, 21, 28).
    for block in 0..5u32 {
        let lbl = labels[(block * 7) as usize];
        assert_eq!(lbl, block * 7, "clique {block} label {lbl}");
        for j in (block * 7)..((block + 1) * 7) {
            assert_eq!(labels[j as usize], block * 7);
        }
    }
}

#[test]
fn path_is_one_component() {
    let Some(ctx) = common::gpu_ctx_or_skip("cc_path") else {
        return;
    };
    let g = path(64);
    let labels = gpu_connected_components(&ctx, &g).expect("gpu cc");
    assert_eq!(num_components(&labels), 1);
    assert!(
        labels.iter().all(|&l| l == 0),
        "all nodes label 0 (the min)"
    );
}

#[test]
fn isolated_nodes_are_singletons() {
    let Some(ctx) = common::gpu_ctx_or_skip("cc_isolated") else {
        return;
    };
    // 3 nodes, no edges → 3 singleton components (no dangling handling needed).
    let g = CsrGraph {
        n_nodes: 3,
        offsets: vec![0, 0, 0, 0],
        neighbors: vec![],
    };
    let labels = gpu_connected_components(&ctx, &g).expect("gpu cc");
    assert_eq!(labels, vec![0, 1, 2]);
}

#[test]
fn nix_two_cycles_is_two_components() {
    let Some(ctx) = common::gpu_ctx_or_skip("cc_nix_two_cycles") else {
        return;
    };
    // Two cycles with disjoint prefixes ("a*" and "b*") ⇒ 2 components.
    let expr = "let g = import /jc/src/graph.nix {}; \
                    gcl = import /jc/src/graph-combinators.nix { graph = g; }; \
                in g.toGraphJSON (g.merge \
                     (gcl.cycleGen { nodes = 10; prefix = \"a\"; }) \
                     (gcl.cycleGen { nodes = 12; prefix = \"b\"; }))";
    let gen = eval_graph(expr).expect("tvix eval");

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
    let g = CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    };

    let labels = gpu_connected_components(&ctx, &g).expect("gpu cc");
    assert_eq!(
        num_components(&labels),
        2,
        "two disjoint cycles → 2 components"
    );
    // Matches the CPU oracle exactly.
    assert_eq!(labels, cpu_connected_components(&g));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    /// GPU ≡ CPU union-find over random graphs (any structure: multiple
    /// components, isolated nodes, cycles).
    #[test]
    fn gpu_equals_cpu_union_find(
        n in 1u32..40,
        raw_edges in proptest::collection::vec((0u32..40, 0u32..40), 0..80),
    ) {
        let Some(ctx) = common::gpu_ctx_or_skip("cc_proptest") else { return Ok(()); };
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

        let gpu = gpu_connected_components(&ctx, &g).expect("gpu cc");
        let cpu = cpu_connected_components(&g);
        prop_assert_eq!(gpu, cpu);
    }
}
