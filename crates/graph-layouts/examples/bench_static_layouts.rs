//! Criterion benches sweeping every registered static layout across a
//! parameter matrix of node counts. Records per-(algo, n) wall-clock time
//! so regressions land in `target/criterion/` and `cargo bench` summary.
//!
//! Run: `cargo bench -p graph-layouts --bench static_layouts`
//!      `just bench` for the wrapped form.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use graph_layouts::{
    CircleLayout, CircleSettings, ConcentricLayout, ConcentricSettings, Graph, GridLayout,
    GridSettings, HilbertLayout, HilbertSettings, Node, RandomLayout, RandomSettings,
    SphereLayout, SphereSettings, StaticLayout,
};

/// Build a path-graph: N nodes in a chain, N-1 edges. Cheap to construct
/// at scale and exercises the edge pass for layouts that need it
/// (Concentric uses degrees, the rest ignore edges).
fn build_graph(n: usize) -> Graph {
    let mut g = Graph::new();
    for i in 0..n {
        g.add_node(Node::new(format!("{i:08}")));
    }
    for i in 0..n.saturating_sub(1) {
        let id = format!("e{i:08}");
        let src = format!("{i:08}");
        let tgt = format!("{:08}", i + 1);
        g.add_edge(graph_layouts::Edge::new(id, src, tgt));
    }
    g
}

fn bench_static_layouts(c: &mut Criterion) {
    // Skip 1M by default — it's slow and dominates `cargo bench` runtime.
    // Set BENCH_INCLUDE_1M=1 to include it.
    let include_huge = std::env::var("BENCH_INCLUDE_1M").is_ok();
    let mut sizes = vec![1_000usize, 10_000, 100_000];
    if include_huge {
        sizes.push(1_000_000);
    }

    let mut group = c.benchmark_group("static_layouts");
    for &n in &sizes {
        let g = build_graph(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("random", n), &g, |b, g| {
            let s = RandomSettings::default();
            b.iter(|| RandomLayout::solve(&s, g).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("circle", n), &g, |b, g| {
            let s = CircleSettings::default();
            b.iter(|| CircleLayout::solve(&s, g).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("grid", n), &g, |b, g| {
            let s = GridSettings::default();
            b.iter(|| GridLayout::solve(&s, g).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("sphere", n), &g, |b, g| {
            let s = SphereSettings::default();
            b.iter(|| SphereLayout::solve(&s, g).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("hilbert", n), &g, |b, g| {
            let s = HilbertSettings::default();
            b.iter(|| HilbertLayout::solve(&s, g).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("concentric", n), &g, |b, g| {
            let s = ConcentricSettings::default();
            b.iter(|| ConcentricLayout::solve(&s, g).unwrap());
        });
    }
    group.finish();
}

/// Profiling is opt-in via BENCH_PPROF=1 (set by the jump-cannon-perf wrapper
/// when the chart's tests.profiling.enabled wires PYROSCOPE_URL): pprof-rs
/// samples at 100 Hz and criterion writes one gzipped `profile.pb` per
/// benchmark under $CRITERION_HOME (default target/criterion) for the wrapper
/// to push to Pyroscope. Profiling perturbs wall-clock numbers, so the nightly
/// Pushgateway timings stay profiler-free unless profiling is on.
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
    targets = bench_static_layouts
}
criterion_main!(benches);
