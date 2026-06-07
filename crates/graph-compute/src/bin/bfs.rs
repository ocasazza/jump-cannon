//! `graph-bfs` — one-shot single-source BFS over a CSR file.
//!
//! The lavender notebooks' GPU diagnostic entrypoint, mirroring `graph-pagerank`.
//! Reads a CSR in graph-compute's binary format
//! (`[u32 n_nodes][u32 n_edges][u32×(n+1) offsets][u32×m neighbors]`, the same
//! file `CsrGraph::write_bin` writes and graph-api serves at `/graph/csr.bin`)
//! and emits a JSON array of per-node hop distances from `--source`, in CSR node
//! order. Unreachable nodes serialize as `UNREACHABLE` (`u32::MAX`).
//!
//! Hardware-agnostic: runs the wgpu kernel (Metal/Vulkan/DX12) when an adapter
//! is present, otherwise the CPU reference — and falls back to CPU automatically
//! if the GPU path rejects the graph. The notebook just calls it and reads the
//! distances; it does not care which backend ran.
//!
//! Usage:
//!     graph-bfs <csr.bin> --source <u32> [--out dists.json] [--cpu]
//!
//! With no `--out`, distances go to stdout; the chosen backend + timing go to
//! stderr (so stdout stays a clean JSON array the notebook can `json.loads`).

use std::io::Write;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use graph_compute::analytics::{cpu_bfs, gpu_bfs};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

struct Args {
    csr_path: String,
    source: Option<u32>,
    out: Option<String>,
    force_cpu: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        csr_path: String::new(),
        source: None,
        out: None,
        force_cpu: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--source" => {
                a.source = Some(
                    it.next()
                        .ok_or_else(|| anyhow!("--source needs a value"))?
                        .parse()
                        .context("parsing --source")?,
                )
            }
            "--out" => a.out = Some(it.next().ok_or_else(|| anyhow!("--out needs a path"))?),
            "--cpu" => a.force_cpu = true,
            "-h" | "--help" => {
                eprintln!("graph-bfs <csr.bin> --source <u32> [--out dists.json] [--cpu]");
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
    let source = args
        .source
        .ok_or_else(|| anyhow!("missing required --source <u32>"))?;
    let graph = CsrGraph::load_bin(&args.csr_path)
        .with_context(|| format!("loading CSR from {}", args.csr_path))?;
    if source >= graph.n_nodes {
        bail!(
            "--source {source} out of range (graph has {} nodes)",
            graph.n_nodes
        );
    }
    eprintln!(
        "graph-bfs: {} nodes, {} edges, source={source}",
        graph.n_nodes,
        graph.neighbors.len(),
    );

    let t = Instant::now();
    let (dists, backend) = if args.force_cpu {
        (cpu_bfs(&graph, source), "cpu (forced)")
    } else {
        let ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_some() {
            match gpu_bfs(&ctx, &graph, source) {
                Ok(r) => (r, "gpu (wgpu)"),
                Err(e) => {
                    eprintln!("graph-bfs: GPU path unavailable ({e}); using CPU");
                    (cpu_bfs(&graph, source), "cpu (fallback)")
                }
            }
        } else {
            eprintln!("graph-bfs: no GPU adapter; using CPU");
            (cpu_bfs(&graph, source), "cpu")
        }
    };
    eprintln!("graph-bfs: backend={backend}, elapsed={:?}", t.elapsed());

    let json = serde_json::to_string(&dists).context("serializing distances")?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, json).with_context(|| format!("writing {path}"))?;
            eprintln!("graph-bfs: wrote {} distances to {path}", dists.len());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(json.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}
