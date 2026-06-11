//! `graph-layout-stability` — empirical stability harness for layout engines.
//!
//! Loads a real CSR graph (graph-compute binary format, the same file graph-api
//! serves at `/graph/csr.bin`) or a seeded synthetic scale-free graph, seeds a
//! deterministic random ball of radius `sqrt(n)·5` (the renderer's seed
//! convention), runs the selected engine N steps, and prints per-step stability
//! telemetry: max |coordinate|, mean per-node displacement, and non-finite
//! counts. This is the tool the fa2 divergence / geometric flip-flop fixes were
//! measured with; the CI-safe assertions live in `tests/layout_stability.rs`.
//!
//! Usage:
//!     graph-layout-stability --engine fa2-bh [--csr /tmp/jc-csr.bin | --synthetic 10000]
//!         [--steps 1000] [--params '{"gravity":1.0}'] [--sample 10]

use anyhow::{anyhow, bail, Context, Result};

use graph_compute::engines::{
    CsrShard, EngineCtx, Fa2BhEngine, Fa2BruteEngine, GeometricEngine, GeometricGpuEngine,
    LayoutEngine,
};
use graph_compute::sim::CsrGraph;
use graph_compute::stability::{ball_seed, synthetic_scale_free, StepStats};

struct Args {
    engine: String,
    csr: Option<String>,
    synthetic: Option<u32>,
    steps: usize,
    params: Option<serde_json::Value>,
    sample: usize,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        engine: String::new(),
        csr: None,
        synthetic: None,
        steps: 1000,
        params: None,
        sample: 10,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .ok_or_else(|| anyhow!("{name} needs a value"))
        };
        match arg.as_str() {
            "--engine" => a.engine = val("--engine")?,
            "--csr" => a.csr = Some(val("--csr")?),
            "--synthetic" => a.synthetic = Some(val("--synthetic")?.parse()?),
            "--steps" => a.steps = val("--steps")?.parse()?,
            "--sample" => a.sample = val("--sample")?.parse()?,
            "--params" => {
                a.params = Some(serde_json::from_str(&val("--params")?).context("--params JSON")?)
            }
            other => bail!("unknown arg {other}"),
        }
    }
    if a.engine.is_empty() {
        bail!("--engine is required (fa2-bh | fa2-brute | geometric | geometric-gpu)");
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let graph = match (&args.csr, args.synthetic) {
        (Some(path), _) => CsrGraph::load_bin(path).context("load CSR")?,
        (None, Some(n)) => synthetic_scale_free(n, 5, 0xC0FFEE),
        (None, None) => bail!("provide --csr <file> or --synthetic <n>"),
    };
    let n = graph.n_nodes as usize;
    eprintln!(
        "graph: {} nodes, {} directed adjacency entries",
        n,
        graph.neighbors.len()
    );

    let mut engine: Box<dyn LayoutEngine> = match args.engine.as_str() {
        "fa2-bh" => Box::new(Fa2BhEngine::new()),
        "fa2-brute" => Box::new(Fa2BruteEngine::new()),
        "geometric" => Box::new(GeometricEngine::new()),
        "geometric-gpu" => Box::new(GeometricGpuEngine::new()),
        other => bail!("unknown engine {other}"),
    };
    if let Some(p) = &args.params {
        engine.set_params(p).map_err(|e| anyhow!(e))?;
    }

    let mut ctx = EngineCtx::try_new_gpu();
    if ctx.gpu.is_none() {
        eprintln!("note: no wgpu adapter; GPU engines will fail init");
    }

    let seed = ball_seed(n, 0x5EED);
    engine
        .init(&mut ctx, &CsrShard::whole(&graph), &seed)
        .map_err(|e| anyhow!("init: {e}"))?;

    let mut prev = seed.clone();
    println!("step\tmax_abs\tp50_r\tp99_r\tmean_disp\tp50_disp\tnonfinite");
    for step in 1..=args.steps {
        let out = engine.step(&mut ctx).positions;
        let st = StepStats::measure(&prev, &out);
        if step % args.sample == 0 || step <= 5 || st.nonfinite > 0 {
            println!(
                "{step}\t{:.2}\t{:.2}\t{:.2}\t{:.4}\t{:.4}\t{}",
                st.max_abs, st.p50_radius, st.p99_radius, st.mean_disp, st.p50_disp, st.nonfinite
            );
        }
        if st.nonfinite > 0 {
            eprintln!("non-finite positions at step {step}; aborting");
            std::process::exit(2);
        }
        prev = out;
    }
    Ok(())
}
