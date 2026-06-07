//! Criterion perf **matrix** for the GPU graph kernels: PageRank and weighted
//! SpMV throughput swept across a {average degree} × {structure} grid — the
//! sparse↔dense axis the headline `bench_pagerank` doesn't cover (it pins one
//! degree-8 k-ring). This is the regression signal for *degree sensitivity*:
//! the kernels are a CSR gather, so wall-clock should track edge count, and any
//! drift in Melem/s at a fixed (degree, structure) cell across merges is a perf
//! regression.
//!
//! Two axes:
//!   * **degree** — regular k-ring at degree 2/8/32/128 (sparse → dense). Same n,
//!     so the only thing changing is edges-per-node; throughput (Melem/s) should
//!     stay roughly flat if the gather is bandwidth-bound and not degree-bound.
//!   * **structure** — fixed average degree, three neighbour layouts: a ring
//!     (spatially local), a uniform-random graph (scattered gathers that thrash
//!     cache), and dense blocks (locally complete). Same edge budget, very
//!     different memory-access pattern → isolates structure sensitivity.
//!
//! Throughput is always reported in graph **edges** (`Throughput::Elements` over
//! `neighbors.len()`) so Melem/s is comparable across every cell.
//!
//! Run: `cargo run --release -p graph-compute --example bench_scaling -- --bench`
//!      Set `BENCH_INCLUDE_HUGE=1` on a real-GPU builder for the millions-scale
//!      point. Software lavapipe is correctness-only — not a perf baseline.
//!
//! Lives as an `example` (not a `[[bench]]` target) so it survives crane's
//! mkDummySrc deps-only build phase, matching bench_pagerank.rs / bench_spmv.rs.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use graph_compute::analytics::{gpu_pagerank, gpu_spmv, WeightedCsr};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

/// Small deterministic xorshift so the random structure is reproducible across
/// runs (a perf baseline must bench the *same* graph every merge).
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// Build a symmetric CSR from an undirected edge set given as per-node neighbour
/// lists. Dedups and sorts each row; guarantees no dangling node by folding in a
/// degree-2 ring backbone first (every kernel here rejects degree-0 rows).
fn csr_from_adj(n: u32, mut adj: Vec<Vec<u32>>) -> CsrGraph {
    // Ring backbone → connectivity + no dangling, regardless of the extra edges.
    for i in 0..n as usize {
        adj[i].push(((i as u32 + n - 1) % n) as u32);
        adj[i].push(((i as u32 + 1) % n) as u32);
    }
    let mut offsets = Vec::with_capacity(n as usize + 1);
    let mut neighbors = Vec::new();
    for row in adj.iter_mut() {
        row.sort_unstable();
        row.dedup();
        offsets.push(neighbors.len() as u32);
        neighbors.extend_from_slice(row);
    }
    offsets.push(neighbors.len() as u32);
    CsrGraph {
        n_nodes: n,
        offsets,
        neighbors,
    }
}

/// Regular k-nearest ring: every node adjacent to its ±k neighbours → degree 2k,
/// symmetric, spatially local (neighbour indices are close to v).
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

/// Uniform-random undirected graph: `avg_deg` random neighbours per node,
/// symmetrized → scattered gathers (neighbour indices uncorrelated with v, the
/// cache-hostile case). Deterministic from `seed`.
fn random_graph(n: u32, avg_deg: u32, seed: u64) -> CsrGraph {
    let mut rng = Rng::new(seed);
    let mut adj = vec![Vec::new(); n as usize];
    let half = (avg_deg / 2).max(1);
    for v in 0..n {
        for _ in 0..half {
            let u = (rng.next() % n as u64) as u32;
            if u != v {
                adj[v as usize].push(u);
                adj[u as usize].push(v); // symmetrize
            }
        }
    }
    csr_from_adj(n, adj)
}

/// Locally-dense graph: nodes partitioned into fully-connected blocks of
/// `block` (degree ≈ block-1 inside a block), blocks otherwise disjoint — high
/// degree with perfectly local neighbours, contrasting random_graph's scatter.
fn dense_blocks(n: u32, block: u32) -> CsrGraph {
    let mut adj = vec![Vec::new(); n as usize];
    let mut start = 0u32;
    while start < n {
        let end = (start + block).min(n);
        for a in start..end {
            for b in start..end {
                if a != b {
                    adj[a as usize].push(b);
                }
            }
        }
        start = end;
    }
    csr_from_adj(n, adj)
}

/// Reinterpret an unweighted CSR as a weighted one (all weights 1.0) so the same
/// fixtures drive the SpMV bench.
fn weighted(g: &CsrGraph) -> (WeightedCsr, Vec<f32>) {
    let x = vec![1.0f32; g.n_nodes as usize];
    (
        WeightedCsr {
            n_nodes: g.n_nodes,
            offsets: g.offsets.clone(),
            neighbors: g.neighbors.clone(),
            weights: vec![1.0f32; g.neighbors.len()],
        },
        x,
    )
}

fn bench_scaling(c: &mut Criterion) {
    let ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_none() {
        eprintln!("bench_scaling: no GPU adapter — skipping (perf matrix is GPU-only)");
        return;
    }
    let iters = 30u32;
    let damping = 0.85f32;

    // Keep the default size modest so a CI builder finishes; one big point on a
    // real GPU when explicitly asked. The degree axis is the headline here.
    let n = if std::env::var("BENCH_INCLUDE_HUGE").is_ok() {
        1_000_000u32
    } else {
        100_000u32
    };

    // ---- Axis 1: degree sweep (sparse → dense) on a regular k-ring ----------
    // k = 1/4/16/64 → degree 2/8/32/128. Same n, only edges-per-node changes.
    {
        let mut group = c.benchmark_group("scaling_degree_pagerank");
        for k in [1u32, 4, 16, 64] {
            let g = k_ring(n, k);
            let deg = 2 * k;
            group.throughput(Throughput::Elements(g.neighbors.len() as u64));
            group.bench_with_input(BenchmarkId::new("gpu", deg), &g, |b, g| {
                b.iter(|| gpu_pagerank(&ctx, g, damping, iters).expect("gpu pagerank"));
            });
        }
        group.finish();
    }
    {
        let mut group = c.benchmark_group("scaling_degree_spmv");
        for k in [1u32, 4, 16, 64] {
            let (a, x) = weighted(&k_ring(n, k));
            let deg = 2 * k;
            group.throughput(Throughput::Elements(a.neighbors.len() as u64));
            group.bench_with_input(BenchmarkId::new("gpu", deg), &(a, x), |b, (a, x)| {
                b.iter(|| gpu_spmv(&ctx, a, x).expect("gpu spmv"));
            });
        }
        group.finish();
    }

    // ---- Axis 2: structure at fixed ~degree-8 (local vs scattered vs dense) --
    // Same edge budget, three memory-access patterns. Isolates structure cost.
    {
        let structures: [(&str, CsrGraph); 3] = [
            ("ring_local", k_ring(n, 4)),                   // degree 8, local
            ("random_scattered", random_graph(n, 8, 1234)), // ~degree 8, scattered
            ("dense_blocks", dense_blocks(n, 9)),           // degree 8, locally complete
        ];
        let mut group = c.benchmark_group("scaling_structure_pagerank");
        for (name, g) in &structures {
            group.throughput(Throughput::Elements(g.neighbors.len() as u64));
            group.bench_with_input(BenchmarkId::new("gpu", name), g, |b, g| {
                b.iter(|| gpu_pagerank(&ctx, g, damping, iters).expect("gpu pagerank"));
            });
        }
        group.finish();
    }
}

criterion_group!(benches, bench_scaling);
criterion_main!(benches);
