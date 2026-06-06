//! Cross-oracle validation: `gpu_pagerank` (wgpu/Metal) vs the public CPU
//! reference in `graph-metrics` (`compute_pagerank`), the implementation the
//! lavender notebooks consumed before the cuGraph cutover.
//!
//! `graph_metrics::compute_pagerank` is *directed* and `f64`; this GPU kernel
//! is *undirected/symmetric* and `f32`. To compare apples-to-apples we feed
//! graph-metrics a **symmetrized** edge set (both (u→v) and (v→u)), which makes
//! its directed iteration identical to the undirected pull form. Node i in the
//! CSR maps to VaultGraph id "i" (IndexMap insertion order ⇒ idx[i] == i), so
//! ranks line up index-for-index.
//!
//! Skips cleanly on hosts without a GPU adapter.

use std::collections::{BTreeMap, BTreeSet};

use graph_compute::analytics::gpu_pagerank;
use graph_compute::sim::CsrGraph;
use vault_data::{VaultEdge, VaultGraph, VaultNode};

mod common;

/// Deterministic connected simple graph on `n` nodes, no self-loops, no
/// dangling: a base ring (guarantees connectivity + degree ≥ 2) plus
/// pseudo-random chords and one hub (node 0 linked to many) so ranks are
/// genuinely non-uniform — the case where an ordering bug would actually show.
fn build_edges(n: u32) -> BTreeSet<(u32, u32)> {
    let mut edges = BTreeSet::new();
    let add = |a: u32, b: u32, set: &mut BTreeSet<(u32, u32)>| {
        if a != b {
            set.insert((a.min(b), a.max(b)));
        }
    };
    for i in 0..n {
        add(i, (i + 1) % n, &mut edges); // ring
        add(i, (i.wrapping_mul(37).wrapping_add(11)) % n, &mut edges); // chord
    }
    // Hub: node 0 reaches every 5th node, giving it a high degree.
    let mut j = 5;
    while j < n {
        add(0, j, &mut edges);
        j += 5;
    }
    edges
}

/// Symmetric CSR from the canonical (u<v) edge set.
fn csr_from_edges(n: u32, edges: &BTreeSet<(u32, u32)>) -> CsrGraph {
    let mut adj: BTreeMap<u32, Vec<u32>> = (0..n).map(|i| (i, Vec::new())).collect();
    for &(u, v) in edges {
        adj.get_mut(&u).unwrap().push(v);
        adj.get_mut(&v).unwrap().push(u);
    }
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for i in 0..n {
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(&adj[&i]);
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// VaultGraph with the same nodes (id "i") and symmetrized directed edges.
fn vault_from_edges(n: u32, edges: &BTreeSet<(u32, u32)>) -> VaultGraph {
    let mut vg = VaultGraph::new();
    for i in 0..n {
        vg.add_node(VaultNode {
            id: i.to_string(),
            ..Default::default()
        });
    }
    for &(u, v) in edges {
        vg.add_edge(VaultEdge {
            source: u.to_string(),
            target: v.to_string(),
        });
        vg.add_edge(VaultEdge {
            source: v.to_string(),
            target: u.to_string(),
        });
    }
    vg
}

#[test]
fn gpu_pagerank_matches_graph_metrics_oracle() {
    let Some(ctx) = common::gpu_ctx_or_skip("gpu_pagerank_cross_oracle") else {
        return;
    };

    let n: u32 = 300;
    let edges = build_edges(n);
    let csr = csr_from_edges(n, &edges);
    let mut vg = vault_from_edges(n, &edges);

    // Same damping + iteration count on both sides; 100 iters is well past
    // convergence for n=300.
    let damping = 0.85;
    let iters = 100;
    graph_metrics::compute_pagerank(&mut vg, damping as f64, iters as usize);
    let gpu = gpu_pagerank(&ctx, &csr, damping, iters).expect("gpu pagerank");

    assert_eq!(gpu.len(), n as usize);

    // Per-node agreement (f32 GPU vs f64 CPU). Ranks sit around 1/n ≈ 3.3e-3;
    // the hub is much larger. Converged, both forms identical → tol 1e-3.
    let cpu: Vec<f64> = (0..n)
        .map(|i| vg.nodes.get(&i.to_string()).unwrap().metrics.pagerank)
        .collect();
    let mut max_abs = 0.0f64;
    for i in 0..n as usize {
        let d = (gpu[i] as f64 - cpu[i]).abs();
        max_abs = max_abs.max(d);
    }
    assert!(
        max_abs < 1e-3,
        "max per-node |gpu - graph_metrics| = {max_abs} exceeds 1e-3"
    );

    // Ordering is identical (the property the notebook diagnostic actually
    // depends on). Compare full argsort by rank, descending.
    let argsort = |xs: &[f64]| {
        let mut idx: Vec<usize> = (0..xs.len()).collect();
        idx.sort_by(|&a, &b| xs[b].partial_cmp(&xs[a]).unwrap());
        idx
    };
    let gpu_f64: Vec<f64> = gpu.iter().map(|&x| x as f64).collect();
    let gpu_order = argsort(&gpu_f64);
    let cpu_order = argsort(&cpu);
    // Hub (node 0) must be the top-ranked node in both.
    assert_eq!(gpu_order[0], 0, "GPU did not rank the hub first");
    assert_eq!(cpu_order[0], 0, "CPU did not rank the hub first");
    // Top-20 ranking matches exactly.
    assert_eq!(
        &gpu_order[..20],
        &cpu_order[..20],
        "top-20 PageRank ordering diverges between GPU and graph-metrics"
    );

    let mass: f32 = gpu.iter().sum();
    assert!((mass - 1.0).abs() < 1e-2, "gpu rank mass {mass} != 1");
}
