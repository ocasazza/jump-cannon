//! `graph-cc` — one-shot connected components over a CSR file.
//!
//! The lavender notebooks' GPU diagnostic entrypoint, mirroring `graph-pagerank`.
//! Reads a CSR in graph-compute's binary format
//! (`[u32 n_nodes][u32 n_edges][u32×(n+1) offsets][u32×m neighbors]`, the same
//! file `CsrGraph::write_bin` writes and graph-api serves at `/graph/csr.bin`)
//! and emits a JSON array of per-node component labels, in CSR node order.
//!
//! Hardware-agnostic: runs the wgpu kernel (Metal/Vulkan/DX12) when an adapter
//! is present, otherwise the CPU reference — and falls back to CPU automatically
//! if the GPU path rejects the graph. The notebook just calls it and reads the
//! labels; it does not care which backend ran.
//!
//! Usage:
//!     graph-cc <csr.bin> [--out labels.json] [--cpu]
//!
//! With no `--out`, labels go to stdout; the chosen backend + timing go to stderr
//! (so stdout stays a clean JSON array the notebook can `json.loads`).

use std::io::Write;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use graph_compute::analytics::{cpu_connected_components, gpu_connected_components};
use graph_compute::sim::CsrGraph;
use graph_compute::EngineCtx;

struct Args {
    csr_path: String,
    out: Option<String>,
    force_cpu: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        csr_path: String::new(),
        out: None,
        force_cpu: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => a.out = Some(it.next().ok_or_else(|| anyhow!("--out needs a path"))?),
            "--cpu" => a.force_cpu = true,
            "-h" | "--help" => {
                eprintln!("graph-cc <csr.bin> [--out labels.json] [--cpu]");
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
        "graph-cc: {} nodes, {} edges",
        graph.n_nodes,
        graph.neighbors.len(),
    );

    let t = Instant::now();
    let (labels, backend) = if args.force_cpu {
        (cpu_connected_components(&graph), "cpu (forced)")
    } else {
        let ctx = EngineCtx::try_new_gpu();
        if ctx.gpu.is_some() {
            match gpu_connected_components(&ctx, &graph) {
                Ok(r) => (r, "gpu (wgpu)"),
                Err(e) => {
                    eprintln!("graph-cc: GPU path unavailable ({e}); using CPU");
                    (cpu_connected_components(&graph), "cpu (fallback)")
                }
            }
        } else {
            eprintln!("graph-cc: no GPU adapter; using CPU");
            (cpu_connected_components(&graph), "cpu")
        }
    };
    eprintln!("graph-cc: backend={backend}, elapsed={:?}", t.elapsed());

    let json = serde_json::to_string(&labels).context("serializing labels")?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, json).with_context(|| format!("writing {path}"))?;
            eprintln!("graph-cc: wrote {} labels to {path}", labels.len());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(json.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}
