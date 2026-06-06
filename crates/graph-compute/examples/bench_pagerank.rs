//! Criterion benches for the PageRank kernels: GPU (wgpu — Metal locally,
//! lavapipe in software CI) vs the CPU reference, swept across node counts.
//! Records per-(backend, n) wall-clock so perf regressions land in
//! `target/criterion/` and the `--bench` JSON the Hydra report ingests.
//!
//! Run: `cargo run --release -p graph-compute --example bench_pagerank -- --bench`
//!      `just bench` style. Perf numbers are only meaningful on a real GPU
//!      builder (the aarch64-darwin Metal machines) — software lavapipe is
//!      correctness-only, not a perf baseline.
//!
//! Lives as an `example` (not a `[[bench]]` target) so it survives crane's
//! mkDummySrc deps-only build phase, matching graph-layouts/bench_static_layouts.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use graph_compute::analytics::{cpu_pagerank, gpu_pagerank};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

/// Undirected k-nearest ring: n nodes, degree 2k, symmetric, no dangling.
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

fn bench_pagerank(c: &mut Criterion) {
    let k = 4u32; // degree 8
    let iters = 30u32;
    let damping = 0.85f32;

    // Default sweep stays modest so software-Vulkan CI completes; set
    // BENCH_INCLUDE_HUGE=1 on a real-GPU builder for the millions-scale point.
    let mut sizes = vec![10_000u32, 100_000, 500_000];
    if std::env::var("BENCH_INCLUDE_HUGE").is_ok() {
        sizes.push(2_000_000);
    }

    let ctx = EngineCtx::try_new_gpu();
    let have_gpu = ctx.gpu.is_some();

    let mut group = c.benchmark_group("pagerank");
    for &n in &sizes {
        let g = k_ring(n, k);
        group.throughput(Throughput::Elements(g.neighbors.len() as u64));

        if have_gpu {
            group.bench_with_input(BenchmarkId::new("gpu", n), &g, |b, g| {
                b.iter(|| gpu_pagerank(&ctx, g, damping, iters).expect("gpu pagerank"));
            });
        }
        // CPU is O(n·iters); cap it so the bench doesn't run for minutes.
        if n <= 100_000 {
            group.bench_with_input(BenchmarkId::new("cpu", n), &g, |b, g| {
                b.iter(|| cpu_pagerank(g, damping, iters));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_pagerank);
criterion_main!(benches);
