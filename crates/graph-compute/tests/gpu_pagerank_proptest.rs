//! Property-based correctness oracles for `gpu_pagerank`.
//!
//! Per the GPU-numerical-testing literature (ISSTA 2025 bug taxonomy; SC24 FP
//! non-associativity), GPU vs CPU must be compared by **tolerance + invariants**,
//! not exact match. Three oracles:
//!   1. Closed-form star graph — an analytic PageRank we can check against.
//!   2. GPU ≡ CPU over proptest-generated symmetric, no-dangling graphs.
//!   3. Structural invariants on every generated case: finite, positive,
//!      mass-conserving (Σ = 1).
//!
//! All gate on a real adapter (Metal locally; lavapipe software-Vulkan in the
//! Linux CI sandbox) and skip cleanly when none is present.

use std::sync::OnceLock;

use proptest::prelude::*;

use graph_compute::analytics::{cpu_pagerank, gpu_pagerank};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

mod common;

const DAMPING: f32 = 0.85;
const ITERS: u32 = 100;
const TOL: f32 = 1e-3;

/// One GPU context shared across all proptest cases (creating a wgpu device per
/// case would dominate runtime). `None` ⇒ no adapter ⇒ tests skip.
fn ctx() -> &'static Option<EngineCtx> {
    static CTX: OnceLock<Option<EngineCtx>> = OnceLock::new();
    CTX.get_or_init(|| common::gpu_ctx_or_skip("gpu_pagerank_proptest"))
}

/// Symmetric CSR from canonical undirected edges (u<v), with a ring backbone
/// guaranteeing connectivity + degree ≥ 2 (no dangling).
fn symmetric_csr(n: u32, chords: &[(u32, u32)]) -> CsrGraph {
    use std::collections::BTreeSet;
    let mut edges: BTreeSet<(u32, u32)> = BTreeSet::new();
    for i in 0..n {
        let a = i;
        let b = (i + 1) % n;
        edges.insert((a.min(b), a.max(b)));
    }
    for &(u, v) in chords {
        let (a, b) = (u % n, v % n);
        if a != b {
            edges.insert((a.min(b), a.max(b)));
        }
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
    for (u, v) in edges {
        adj[u as usize].push(v);
        adj[v as usize].push(u);
    }
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for a in &adj {
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(a);
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Undirected star: node 0 (center) linked to `leaves` leaves. Closed-form
/// stationary PageRank for the pull/undirected form with teleport (1-d)/n:
///     r_center = (1 + d·L) / (n·(1 + d))
///     r_leaf   = (1-d)/n + (d/L)·r_center
fn star_csr(leaves: u32) -> CsrGraph {
    let n = leaves + 1;
    let mut offsets = vec![0u32];
    let mut neighbors = Vec::new();
    // center (node 0): all leaves
    for l in 1..=leaves {
        neighbors.push(l);
    }
    offsets.push(neighbors.len() as u32);
    // each leaf: only the center
    for _ in 1..=leaves {
        neighbors.push(0);
        offsets.push(neighbors.len() as u32);
    }
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

proptest! {
    // No failure-persistence file: integration tests have no crate src root to
    // write regression seeds into (proptest warns otherwise), and CI is
    // ephemeral anyway.
    #![proptest_config(ProptestConfig {
        cases: 24,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Oracle 2 + 3: GPU ≡ CPU and invariants over random symmetric graphs.
    #[test]
    fn gpu_equals_cpu_and_conserves_mass(
        n in 4u32..48,
        chords in proptest::collection::vec((0u32..48, 0u32..48), 0..96),
    ) {
        let Some(ctx) = ctx() else { return Ok(()); };
        let g = symmetric_csr(n, &chords);
        let gpu = gpu_pagerank(ctx, &g, DAMPING, ITERS).expect("gpu pagerank");
        let cpu = cpu_pagerank(&g, DAMPING, ITERS);

        prop_assert_eq!(gpu.len(), n as usize);
        for (i, (a, b)) in gpu.iter().zip(cpu.iter()).enumerate() {
            prop_assert!(a.is_finite() && *a > 0.0, "rank {i} not finite/positive: {a}");
            prop_assert!((a - b).abs() < TOL, "node {i}: gpu {a} vs cpu {b}");
        }
        let mass: f64 = gpu.iter().map(|&r| r as f64).sum();
        prop_assert!((mass - 1.0).abs() < 1e-2, "mass {mass} != 1");
    }

    /// Oracle 1: star graph matches the closed form.
    #[test]
    fn star_matches_closed_form(leaves in 2u32..40) {
        let Some(ctx) = ctx() else { return Ok(()); };
        let n = (leaves + 1) as f32;
        let d = DAMPING;
        let r_center = (1.0 + d * leaves as f32) / (n * (1.0 + d));
        let r_leaf = (1.0 - d) / n + (d / leaves as f32) * r_center;

        let g = star_csr(leaves);
        let gpu = gpu_pagerank(ctx, &g, d, 200).expect("gpu pagerank");

        prop_assert!(
            (gpu[0] - r_center).abs() < TOL,
            "center: gpu {} vs closed-form {r_center}", gpu[0]
        );
        for (i, &r) in gpu.iter().enumerate().skip(1) {
            prop_assert!(
                (r - r_leaf).abs() < TOL,
                "leaf {i}: gpu {r} vs closed-form {r_leaf}"
            );
        }
    }
}
