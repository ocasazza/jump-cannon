//! Scaling benchmark for the GPU force-directed layout engine.
//!
//! Measures per-step wall time across node counts, repulsion backends, and force models.
//! Generates deterministic preferential-attachment graphs (Barabasi-Albert style) with
//! a small LCG seeded from N for reproducibility.
//!
//! Run:
//! ```text
//! cargo run -p graph-layouts --release --example bench_gpu_force -- --nodes 100000,1000000 --steps 20
//! ```

use graph_layouts::{Edge, ForceModel, Graph, GpuForceLayout, GpuForceOptions, Node, RepulsionMode};
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

        let graph = generate_barabasi_albert(n, degree);
        let edge_count = graph.edges.len();

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

                // Warm-up: run once with steps_per_call = 1
                let mut graph_warmup = graph.clone();
                let mut warmup_opts = options.clone();
                warmup_opts.steps_per_call = 1;
                layout.set_options(warmup_opts);

                match pollster::block_on(layout.run(&mut graph_warmup)) {
                    Ok(_) => {}
                    Err(e) => {
                        println!("{}", e);
                        return;
                    }
                }

                // Timed run with original step count
                let mut graph_timed = graph.clone();
                layout.set_options(options);

                let start = Instant::now();
                match pollster::block_on(layout.run(&mut graph_timed)) {
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

        // Drop the graph to free host memory (Graph is string-keyed and dominates at 1e6 nodes)
        drop(graph);
    }
}

/// Generate a deterministic Barabasi-Albert preferential-attachment graph.
/// Each new node attaches D/2 edges to existing nodes, sampled proportionally to degree.
/// Uses a simple LCG seeded from N for reproducibility.
fn generate_barabasi_albert(n: u32, d: u32) -> Graph {
    let mut g = Graph::new();
    let mut rng = Lcg::new(n as u64);

    let mut degree: Vec<u32> = Vec::new();
    let mut edges_list: Vec<usize> = Vec::new();

    for i in 0..n {
        g.add_node(Node::new(format!("n{}", i)));
        degree.push(0);

        let num_edges = (d / 2).max(1);

        for _ in 0..num_edges {
            // Preferential attachment needs a seed edge to sample from:
            // the second node attaches to the first unconditionally.
            if edges_list.is_empty() {
                if i == 0 {
                    continue;
                }
                edges_list.push(0);
                edges_list.push(0);
            }

            let idx = rng.next() as usize % edges_list.len();
            let target = edges_list[idx];

            if target != i as usize {
                let edge_id = format!("e{}", g.edges.len());
                g.add_edge(Edge::new(edge_id, format!("n{}", i), format!("n{}", target)));

                degree[i as usize] += 1;
                degree[target] += 1;

                edges_list.push(i as usize);
                edges_list.push(target);
            }
        }
    }

    g
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
