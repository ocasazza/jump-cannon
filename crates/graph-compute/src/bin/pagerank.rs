//! `graph-pagerank` — one-shot PageRank over a CSR file.
//!
//! The lavender notebooks' GPU diagnostic entrypoint, replacing the NVIDIA-only
//! cuGraph call. Reads a CSR in graph-compute's binary format
//! (`[u32 n_nodes][u32 n_edges][u32×(n+1) offsets][u32×m neighbors]`, the same
//! file `CsrGraph::save_bin` writes and graph-api serves at `/graph/csr.bin`)
//! and emits a JSON array of per-node f32 ranks, in CSR node order.
//!
//! Hardware-agnostic: runs the wgpu kernel (Metal/Vulkan/DX12) when an adapter
//! is present, otherwise the CPU reference — and falls back to CPU automatically
//! if the GPU path rejects the graph (e.g. dangling nodes, not yet handled on
//! the GPU). The notebook just calls it and reads ranks; it does not care which
//! backend ran.
//!
//! Usage:
//!     graph-pagerank <csr.bin> [--damping 0.85] [--iters 100] [--out ranks.json] [--cpu]
//!
//! With no `--out`, ranks go to stdout; the chosen backend + timing go to stderr
//! (so stdout stays a clean JSON array the notebook can `json.loads`).

use std::io::Write;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use graph_compute::analytics::{cpu_pagerank, gpu_pagerank};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

struct Args {
    csr_path: String,
    damping: f32,
    iters: u32,
    out: Option<String>,
    force_cpu: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        csr_path: String::new(),
        damping: 0.85,
        iters: 100,
        out: None,
        force_cpu: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--damping" => {
                a.damping = it
                    .next()
                    .ok_or_else(|| anyhow!("--damping needs a value"))?
                    .parse()
                    .context("parsing --damping")?
            }
            "--iters" => {
                a.iters = it
                    .next()
                    .ok_or_else(|| anyhow!("--iters needs a value"))?
                    .parse()
                    .context("parsing --iters")?
            }
            "--out" => a.out = Some(it.next().ok_or_else(|| anyhow!("--out needs a path"))?),
            "--cpu" => a.force_cpu = true,
            "-h" | "--help" => {
                eprintln!(
                    "graph-pagerank <csr.bin> [--damping 0.85] [--iters 100] [--out ranks.json] [--cpu]"
                );
                std::process::exit(0);
            }
            other if a.csr_path.is_empty() && !other.starts_with('-') => {
                a.csr_path = other.to_string()
            }
            other => bail!("unexpected argument: {other}"),
        }
    }
    if a.csr_path.is_empty() {
        bail!("missing required <csr.bin> path");
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let graph = CsrGraph::load_bin(&args.csr_path)
        .with_context(|| format!("loading CSR from {}", args.csr_path))?;
    eprintln!(
        "graph-pagerank: {} nodes, {} edges, damping={}, iters={}",
        graph.n_nodes,
        graph.neighbors.len(),
        args.damping,
        args.iters
    );

    let t = Instant::now();
    let (ranks, backend) = if args.force_cpu {
        (
            cpu_pagerank(&graph, args.damping, args.iters),
            "cpu (forced)",
        )
    } else {
        let ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_some() {
            match gpu_pagerank(&ctx, &graph, args.damping, args.iters) {
                Ok(r) => (r, "gpu (wgpu)"),
                Err(e) => {
                    eprintln!("graph-pagerank: GPU path unavailable ({e}); using CPU");
                    (
                        cpu_pagerank(&graph, args.damping, args.iters),
                        "cpu (fallback)",
                    )
                }
            }
        } else {
            eprintln!("graph-pagerank: no GPU adapter; using CPU");
            (cpu_pagerank(&graph, args.damping, args.iters), "cpu")
        }
    };
    eprintln!(
        "graph-pagerank: backend={backend}, elapsed={:?}",
        t.elapsed()
    );

    let json = serde_json::to_string(&ranks).context("serializing ranks")?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, json).with_context(|| format!("writing {path}"))?;
            eprintln!("graph-pagerank: wrote {} ranks to {path}", ranks.len());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(json.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}
