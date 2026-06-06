//! Correctness fixtures generated from the **shared Nix graph library**
//! (`graph.nix` + `graph-combinators.nix`, embedded in `tvix-wasm`), evaluated
//! by `tvix-eval`. The canonical shapes — path / cycle / complete / star / grid
//! — are defined once, declaratively, in Nix and reused both by the renderer
//! and here, so the test corpus can't drift from the app's graph model.
//!
//! Each fixture is symmetrized into a `CsrGraph` and checked three ways:
//!   - GPU ≡ CPU oracle (tolerance, not exact — FP non-associativity),
//!   - structural invariants (finite, positive, mass = 1),
//!   - closed-form ranks where the shape has one (cycle/complete → uniform 1/n;
//!     star → the analytic center/leaf split).
//!
//! Gates on a real adapter (Metal locally; lavapipe software-Vulkan in CI).

use std::collections::BTreeMap;

use graph_compute::analytics::{cpu_pagerank, gpu_pagerank};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;
use tvix_wasm::eval_graph;

mod common;

const DAMPING: f32 = 0.85;
const ITERS: u32 = 200;
const TOL: f32 = 1e-3;

/// Standard preamble that makes the embedded library importable, then applies a
/// generator and renders it to the `{ nodes, links }` JSON shape.
fn gen_expr(body: &str) -> String {
    format!(
        "let g = import /jc/src/graph.nix {{}}; \
             gcl = import /jc/src/graph-combinators.nix {{ graph = g; }}; \
         in g.toGraphJSON ({body})"
    )
}

/// Evaluate a Nix generator expression → symmetric CSR + the id-ordered node
/// list (so callers can locate a node, e.g. the star's "n0" center, by id).
fn nix_symmetric_csr(body: &str) -> (CsrGraph, Vec<String>) {
    let g = eval_graph(&gen_expr(body)).expect("tvix eval graph");

    // Stable id → index mapping (BTreeMap = deterministic, id-sorted).
    let mut idx: BTreeMap<String, u32> = BTreeMap::new();
    for n in &g.nodes {
        let next = idx.len() as u32;
        idx.entry(n.id.clone()).or_insert(next);
    }
    let n = idx.len() as u32;

    // Symmetrize: each directed edge contributes both directions; dedup; no
    // self-loops. (The path/cycle/complete/star/grid generators emit directed
    // edges; PageRank here is undirected.)
    use std::collections::BTreeSet;
    let mut edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for e in &g.edges {
        let (a, b) = (idx[&e.source], idx[&e.target]);
        if a != b {
            edges.insert((a.min(b), a.max(b)));
        }
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
    for (a, b) in edges {
        adj[a as usize].push(b);
        adj[b as usize].push(a);
    }
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for a in &adj {
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(a);
    }
    offsets.push(neighbors.len() as u32);

    let mut ids = vec![String::new(); n as usize];
    for (id, &i) in &idx {
        ids[i as usize] = id.clone();
    }
    (
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        },
        ids,
    )
}

fn assert_gpu_eq_cpu_and_valid(ctx: &EngineCtx, g: &CsrGraph) -> Vec<f32> {
    let gpu = gpu_pagerank(ctx, g, DAMPING, ITERS).expect("gpu pagerank");
    let cpu = cpu_pagerank(g, DAMPING, ITERS);
    assert_eq!(gpu.len(), cpu.len());
    for (i, (a, b)) in gpu.iter().zip(cpu.iter()).enumerate() {
        assert!(
            a.is_finite() && *a > 0.0,
            "rank {i} not finite/positive: {a}"
        );
        assert!((a - b).abs() < TOL, "node {i}: gpu {a} vs cpu {b}");
    }
    let mass: f64 = gpu.iter().map(|&r| r as f64).sum();
    assert!((mass - 1.0).abs() < 1e-2, "mass {mass} != 1");
    gpu
}

#[test]
fn nix_cycle_is_uniform() {
    let Some(ctx) = common::gpu_ctx_or_skip("nix_cycle_is_uniform") else {
        return;
    };
    let (g, _) = nix_symmetric_csr("gcl.cycleGen { nodes = 32; prefix = \"n\"; }");
    let gpu = assert_gpu_eq_cpu_and_valid(&ctx, &g);
    // Regular degree-2 ⇒ uniform stationary distribution.
    let inv_n = 1.0 / g.n_nodes as f32;
    for (i, &r) in gpu.iter().enumerate() {
        assert!(
            (r - inv_n).abs() < TOL,
            "cycle node {i}: {r} != 1/n {inv_n}"
        );
    }
}

#[test]
fn nix_complete_is_uniform() {
    let Some(ctx) = common::gpu_ctx_or_skip("nix_complete_is_uniform") else {
        return;
    };
    // completeGen is O(n²) in Nix eval — keep it small.
    let (g, _) = nix_symmetric_csr("gcl.completeGen { nodes = 12; prefix = \"n\"; }");
    let gpu = assert_gpu_eq_cpu_and_valid(&ctx, &g);
    let inv_n = 1.0 / g.n_nodes as f32;
    for (i, &r) in gpu.iter().enumerate() {
        assert!(
            (r - inv_n).abs() < TOL,
            "complete node {i}: {r} != 1/n {inv_n}"
        );
    }
}

#[test]
fn nix_star_matches_closed_form() {
    let Some(ctx) = common::gpu_ctx_or_skip("nix_star_matches_closed_form") else {
        return;
    };
    // starGen nodes = center + spokes; "n0" is the center.
    let total = 21u32;
    let leaves = (total - 1) as f32;
    let (g, ids) = nix_symmetric_csr(&format!(
        "gcl.starGen {{ nodes = {total}; prefix = \"n\"; }}"
    ));
    let gpu = assert_gpu_eq_cpu_and_valid(&ctx, &g);

    let d = DAMPING;
    let n = total as f32;
    let r_center = (1.0 + d * leaves) / (n * (1.0 + d));
    let r_leaf = (1.0 - d) / n + (d / leaves) * r_center;

    let center = ids.iter().position(|s| s == "n0").expect("center n0");
    assert!(
        (gpu[center] - r_center).abs() < TOL,
        "star center: gpu {} vs closed-form {r_center}",
        gpu[center]
    );
    for (i, &r) in gpu.iter().enumerate() {
        if i != center {
            assert!(
                (r - r_leaf).abs() < TOL,
                "star leaf {i}: gpu {r} vs closed-form {r_leaf}"
            );
        }
    }
}

#[test]
fn nix_path_and_grid_gpu_eq_cpu() {
    let Some(ctx) = common::gpu_ctx_or_skip("nix_path_and_grid_gpu_eq_cpu") else {
        return;
    };
    // No simple closed form — parity + invariants are the oracle.
    let (path, _) = nix_symmetric_csr("gcl.pathGen { nodes = 40; prefix = \"n\"; }");
    assert_gpu_eq_cpu_and_valid(&ctx, &path);

    let (grid, _) = nix_symmetric_csr("gcl.gridGen { rows = 6; cols = 7; prefix = \"n\"; }");
    assert_gpu_eq_cpu_and_valid(&ctx, &grid);
}
