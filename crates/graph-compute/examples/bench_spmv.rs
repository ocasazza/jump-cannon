//! Criterion bench: thread-per-row `gpu_spmv` vs the hub-aware (workgroup-per-row)
//! `gpu_spmv_hybrid` on a synthetic **power-law** graph — a few very-high-degree
//! hub rows plus many low-degree rows, the case the hybrid kernel targets.
//!
//! The hybrid kernel produces the same result; the question this bench answers is
//! whether spreading each hub row across a whole workgroup beats letting one
//! thread serialize it. Perf is informational — small graphs may not show the
//! benefit (kernel-launch / barrier overhead can dominate), and software-Vulkan
//! CI is correctness-only, not a perf baseline. Numbers are only meaningful on a
//! real GPU builder (the aarch64-darwin Metal machines).
//!
//! Run: `cargo run --release -p graph-compute --example bench_spmv -- --bench`
//!
//! Lives as an `example` (not a `[[bench]]` target) so it survives crane's
//! mkDummySrc deps-only build phase — matching bench_pagerank.rs.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use graph_compute::analytics::{gpu_spmv, gpu_spmv_hybrid, WeightedCsr};
use graph_compute::EngineCtx;

/// Synthetic power-law graph: `n` nodes, `hubs` rows each adjacent to every node
/// (degree n), the rest with small (0..=3) degree. Deterministic from `seed`.
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
            for c in 0..n {
                neighbors.push(c);
                weights.push(((next() % 2000) as f32) / 1000.0 - 1.0);
            }
        } else {
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

fn bench_spmv(c: &mut Criterion) {
    let ctx = EngineCtx::try_new_gpu();
    let Some(_) = ctx.gpu.as_ref() else {
        eprintln!("bench_spmv: no GPU adapter — skipping (perf bench is GPU-only)");
        return;
    };

    // A few hubs (each adjacent to all n) + many short rows. Sweep node count so
    // the hub rows grow with n — that is where load-balancing should help.
    let hubs = 8u32;
    let mut sizes = vec![2_000u32, 10_000, 50_000];
    if std::env::var("BENCH_INCLUDE_HUGE").is_ok() {
        sizes.push(200_000);
    }

    let mut group = c.benchmark_group("spmv_powerlaw");
    for &n in &sizes {
        let (a, x) = power_law(n, hubs, 7);
        group.throughput(Throughput::Elements(a.neighbors.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("baseline_thread_per_row", n),
            &(),
            |b, _| {
                b.iter(|| gpu_spmv(&ctx, &a, &x).expect("gpu spmv"));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("hybrid_workgroup_per_row", n),
            &(),
            |b, _| {
                b.iter(|| gpu_spmv_hybrid(&ctx, &a, &x).expect("gpu spmv hybrid"));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_spmv);
criterion_main!(benches);
