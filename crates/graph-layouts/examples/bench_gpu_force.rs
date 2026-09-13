//! Scaling benchmark for the GPU force-directed layout engine.
//!
//! Measures per-step wall time across node counts, repulsion backends, and force models.
//! Generates deterministic preferential-attachment graphs (Barabasi-Albert style) with
//! a small LCG seeded from N for reproducibility. Topology is fed as an index-based CSR
//! edge list straight into `run_csr` — no string-keyed `Graph` is ever built.
//!
//! Run:
//! ```text
//! cargo run -p graph-layouts --release --example bench_gpu_force -- --nodes 100000,1000000 --steps 20
//! ```

use graph_layouts::{CsrInput, ForceModel, GpuForceLayout, GpuForceOptions, RepulsionMode};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut node_counts = vec![100_000, 1_000_000];
    let mut step_count = 20u32;
    let mut degree = 8u32;
    let mut modes = vec!["bh", "ns"];
    let mut models = vec!["spring", "tfdp"];

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--nodes" => {
                if i + 1 < args.len() {
                    node_counts = args[i + 1]
                        .split(',')
                        .filter_map(|s| s.trim().parse::<usize>().ok())
                        .collect();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--steps" => {
                if i + 1 < args.len() {
                    if let Ok(s) = args[i + 1].parse::<u32>() {
                        step_count = s;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--degree" => {
                if i + 1 < args.len() {
                    if let Ok(d) = args[i + 1].parse::<u32>() {
                        degree = d;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--modes" => {
                if i + 1 < args.len() {
                    modes = args[i + 1]
                        .split(',')
                        .map(|s| s.trim())
                        .collect();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--models" => {
                if i + 1 < args.len() {
                    models = args[i + 1]
                        .split(',')
                        .map(|s| s.trim())
                        .collect();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    println!(
        "N\tedges\tmode\tmodel\tms_per_step\ttotal_ms"
    );

    for n in &node_counts {
        let n = *n as u32;

        let (n_nodes, edges) = generate_barabasi_albert(n, degree);
        let edge_count = edges.len() / 2;

        for mode_str in &modes {
            let mode = match *mode_str {
                "exact" => {
                    if n > 20_000 {
                        println!("(skipped N={} mode=exact: too large)", n);
                        continue;
                    }
                    RepulsionMode::Exact
                }
                "bh" => RepulsionMode::BarnesHut,
                "ns" => RepulsionMode::NegativeSampling,
                _ => continue,
            };

            for model_str in &models {
                let model = match *model_str {
                    "spring" => ForceModel::SpringElectrical,
                    "tfdp" => ForceModel::TFdp,
                    _ => continue,
                };

                let mut options = GpuForceOptions::default();
                options.steps_per_call = step_count;
                options.energy_threshold = 0.0;
                options.repulsion_samples = 8;
                options.tfdp_k = 3.0;
                options.repulsion_mode = mode;
                options.force_model = model;

                let mut layout = GpuForceLayout::new(options.clone());
                let input = CsrInput {
                    n_nodes,
                    edges: &edges,
                    positions: None,
                };
                let mut out: Vec<f32> = Vec::new();

                // Warm-up: run once with steps_per_call = 1 so pipeline
                // compilation and buffer allocation don't skew the timed run.
                let mut warmup_opts = options.clone();
                warmup_opts.steps_per_call = 1;
                layout.set_options(warmup_opts);

                match pollster::block_on(layout.run_csr(&input, &mut out)) {
                    Ok(_) => {}
                    Err(e) => {
                        println!("{}", e);
                        return;
                    }
                }

                // Timed run with the original step count. Same topology, so
                // the layout reuses its GPU state and continues the sim.
                layout.set_options(options);

                let start = Instant::now();
                match pollster::block_on(layout.run_csr(&input, &mut out)) {
                    Ok(_) => {}
                    Err(e) => {
                        println!("{}", e);
                        return;
                    }
                }
                let elapsed = start.elapsed();

                let total_ms = elapsed.as_secs_f64() * 1000.0;
                let ms_per_step = total_ms / (step_count as f64);

                println!(
                    "{}\t{}\t{}\t{}\t{:.3}\t{:.3}",
                    n, edge_count, mode_str, model_str, ms_per_step, total_ms
                );
            }
        }
    }
}

/// Generate a deterministic Barabasi-Albert preferential-attachment graph as an
/// index-based edge list `[s0, t0, s1, t1, ...]`. Each new node attaches D/2
/// edges to existing nodes, sampled proportionally to degree. Uses a simple LCG
/// seeded from N for reproducibility. Returns `(n_nodes, edges)`.
fn generate_barabasi_albert(n: u32, d: u32) -> (u32, Vec<u32>) {
    let mut rng = Lcg::new(n as u64);

    // Endpoint multiset used for preferential sampling (each existing edge
    // contributes both of its endpoints).
    let mut endpoints: Vec<usize> = Vec::new();
    // Flat undirected edge list, two entries per edge.
    let mut edges: Vec<u32> = Vec::new();

    for i in 0..n {
        let num_edges = (d / 2).max(1);

        for _ in 0..num_edges {
            // Preferential attachment needs a seed edge to sample from:
            // the second node attaches to the first unconditionally.
            if endpoints.is_empty() {
                if i == 0 {
                    continue;
                }
                endpoints.push(0);
                endpoints.push(0);
            }

            let idx = rng.next() as usize % endpoints.len();
            let target = endpoints[idx];

            if target != i as usize {
                edges.push(i);
                edges.push(target as u32);
                endpoints.push(i as usize);
                endpoints.push(target);
            }
        }
    }

    (n, edges)
}

/// Minimal linear congruential generator for reproducible preferential attachment.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
}
