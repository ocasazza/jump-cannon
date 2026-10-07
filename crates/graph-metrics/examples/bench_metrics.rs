//! Criterion benches for the reload-path metrics (`compute_all` and each
//! metric) on a vault-shaped graph: preferential-attachment links, 30% of
//! notes with no outgoing links.
//!
//! Run: `cargo run --release -p graph-metrics --example bench_metrics -- --bench`

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use rand::{rngs::StdRng, Rng, SeedableRng};
use vault_data::{VaultEdge, VaultGraph, VaultNode};

fn vault_graph(n: usize, seed: u64) -> VaultGraph {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut g = VaultGraph::new();
    for i in 0..n {
        g.add_node(VaultNode { id: format!("note-{i:06}"), ..Default::default() });
    }
    // Endpoint list: sampling uniformly from it is degree-proportional.
    let mut endpoints: Vec<usize> = vec![0];
    for i in 1..n {
        if rng.gen_bool(0.3) {
            endpoints.push(i);
            continue;
        }
        for _ in 0..3 {
            let t = endpoints[rng.gen_range(0..endpoints.len())];
            if t != i {
                g.add_edge(VaultEdge::new(format!("note-{i:06}"), format!("note-{t:06}")));
                endpoints.push(t);
                endpoints.push(i);
            }
        }
    }
    g
}

fn bench_metrics(c: &mut Criterion) {
    let mut group = c.benchmark_group("metrics");
    group.sample_size(10);
    for n in [1_000usize, 10_000, 30_000] {
        let g = vault_graph(n, 42);
        let cases: [(&str, fn(&mut VaultGraph)); 7] = [
            ("degree", graph_metrics::compute_degree),
            ("pagerank", |g| graph_metrics::compute_pagerank(g, 0.85, 50)),
            ("betweenness", |g| graph_metrics::compute_betweenness(g, 500)),
            ("kcore", graph_metrics::compute_kcore),
            ("wcc", graph_metrics::compute_wcc),
            ("louvain", |g| graph_metrics::compute_louvain(g, 10)),
            ("all", graph_metrics::compute_all),
        ];
        for (name, f) in cases {
            group.bench_with_input(BenchmarkId::new(name, n), &g, |b, g| {
                b.iter_batched(|| g.clone(), |mut g| f(&mut g), BatchSize::LargeInput)
            });
        }
    }
    group.finish();
}

/// BENCH_PPROF=1 switches to pprof-rs sampling, as in the other perf examples.
fn criterion_config() -> Criterion {
    if std::env::var_os("BENCH_PPROF").is_some() {
        Criterion::default().with_profiler(pprof::criterion::PProfProfiler::new(
            100,
            pprof::criterion::Output::Protobuf,
        ))
    } else {
        Criterion::default()
    }
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_metrics
}
criterion_main!(benches);
