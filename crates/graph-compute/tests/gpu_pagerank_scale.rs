//! Millions-scale stability test for `gpu_pagerank` on the real adapter
//! (Metal on this Mac; Vulkan/DX12 elsewhere). Validates that the kernel
//! survives a graph far larger than the unit-test toys without NaN/Inf and
//! still conserves probability mass — the property that breaks first when a
//! buffer-size, workgroup-dispatch, or precision bug bites at scale.
//!
//! Skips cleanly (passes) on hosts without a GPU adapter so CI runners that
//! lack one don't fail. Run explicitly with:
//!     cargo test -p graph-compute --test gpu_pagerank_scale -- --nocapture

use std::time::Instant;

use graph_compute::analytics::gpu_pagerank;
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

/// Undirected `k`-nearest-ring on `n` nodes: node i links to i±1..=±k (mod n).
/// Symmetric, every node has degree exactly `2k` (no dangling), and the
/// stationary distribution is uniform (1/n) — so correctness at scale is
/// trivially checkable while still pushing `n·2k` edges through the kernel.
fn k_ring(n: u32, k: u32) -> CsrGraph {
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::with_capacity((n * 2 * k) as usize);
    for i in 0..n {
        offsets.push(neighbors.len() as u32);
        for d in 1..=k {
            neighbors.push((i + n - d) % n);
            neighbors.push((i + d) % n);
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
fn gpu_pagerank_millions_scale_stable() {
    let ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_none() {
        eprintln!("Skipping gpu_pagerank scale test (no GPU adapter)");
        return;
    }

    // 2M nodes × degree 8 (k=4) = 16M directed edges. neighbors ≈ 64 MB,
    // offsets ≈ 8 MB, rank buffers ≈ 8 MB each — all under wgpu's default
    // 128 MiB max_storage_buffer_binding_size.
    let n: u32 = 2_000_000;
    let k: u32 = 4;
    let build = Instant::now();
    let g = k_ring(n, k);
    let n_edges = g.neighbors.len();
    eprintln!("built {n} nodes / {n_edges} edges in {:?}", build.elapsed());

    let t = Instant::now();
    let ranks = gpu_pagerank(&ctx, &g, 0.85, 30).expect("gpu pagerank at scale");
    eprintln!("gpu_pagerank({n} nodes, 30 iters) in {:?}", t.elapsed());

    assert_eq!(ranks.len(), n as usize);

    // No NaN/Inf anywhere — the first thing a scale bug corrupts.
    assert!(
        ranks.iter().all(|r| r.is_finite()),
        "non-finite rank present"
    );

    // Mass is conserved (symmetric, no dangling). Accumulate in f64 — a naive
    // f32 sum of 2M values ~5e-7 loses ~2% to rounding (eps(1.0)≈1.2e-7 vs the
    // tiny increments), which would be a test artifact, not a kernel error.
    let sum: f64 = ranks.iter().map(|&r| r as f64).sum();
    assert!((sum - 1.0).abs() < 1e-3, "rank mass {sum} != 1");

    // Uniform graph ⇒ every rank ≈ 1/n. Check the spread is tiny.
    let inv_n = 1.0 / n as f32;
    let max_dev = ranks
        .iter()
        .map(|r| (r - inv_n).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_dev < inv_n * 1e-2,
        "k-ring ranks not uniform: max deviation {max_dev} vs 1/n {inv_n}"
    );
}
