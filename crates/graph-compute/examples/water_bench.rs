//! Throughput bench for the GPU water-sheet layout (`water-sheet-gpu`).
//!
//! For each total node count it builds the canonical waterSheet (molecule pool
//! = N/10 molecules → 30% of nodes, near-square sheet for the rest), uploads it,
//! times GPU steps with a queue sync, and prints ms/iteration, nodes/s and
//! device-buffer bytes. A size that cannot be allocated (adapter buffer limits
//! or device OOM) prints `SKIPPED` with the reason instead of aborting.
//!
//! ```text
//! cargo run --release -p graph-compute --example water_bench -- \
//!     [--sizes 1e6,1e7,1e8] [--steps 20] [--batch 10] [--workgroup N] \
//!     [--staged 0|1] [--rsheet R] [--rmol R]
//!
//! Omitted flags default to [`WaterSheetSettings::default()`].
//! ```
//!
//! `--steps` is the number of timed iterations per size, executed as
//! `steps / batch` submits of `batch` iterations each (one command buffer per
//! submit, synced after each). Timings are GPU wall-clock including queue sync;
//! host graph build / upload time is reported separately as `init_s`.

use std::time::Instant;

use graph_compute::engines::WaterSheetGpuEngine;
use graph_compute::water::{seed_positions, WaterSheetSettings};
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};

struct Args {
    sizes: Vec<f64>,
    steps: u32,
    batch: u32,
    workgroup: u32,
    staged: bool,
    r_sheet: u32,
    r_mol: u32,
}

fn parse_args() -> Args {
    // Defaults mirror `WaterSheetSettings::default()` so the bench can never
    // drift from the engine's tuned configuration.
    let d = WaterSheetSettings::default();
    let mut a = Args {
        sizes: vec![1e6, 1e7, 1e8],
        steps: 20,
        batch: 10,
        workgroup: d.workgroup,
        staged: d.staged,
        r_sheet: d.r_sheet,
        r_mol: d.r_mol,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let key = argv[i].as_str();
        let val = |i: usize| -> &str {
            argv.get(i + 1)
                .unwrap_or_else(|| panic!("{key} needs a value"))
                .as_str()
        };
        match key {
            // cargo passes through "--bench" for criterion-style invocations.
            "--bench" => {
                i += 1;
                continue;
            }
            "--sizes" => {
                a.sizes = val(i)
                    .split(',')
                    .map(|s| s.parse::<f64>().expect("size"))
                    .collect()
            }
            "--steps" => a.steps = val(i).parse().expect("steps"),
            "--batch" => a.batch = val(i).parse().expect("batch"),
            "--workgroup" => a.workgroup = val(i).parse().expect("workgroup"),
            "--staged" => a.staged = matches!(val(i), "1" | "true"),
            "--rsheet" => a.r_sheet = val(i).parse().expect("rsheet"),
            "--rmol" => a.r_mol = val(i).parse().expect("rmol"),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a.batch = a.batch.clamp(1, a.steps.max(1));
    a
}

/// Shape with total nodes ≥ `target`: M = target/10 molecules, near-square sheet.
fn shape_for(target: f64, base: &WaterSheetSettings) -> WaterSheetSettings {
    let molecules = (target / 10.0) as u32;
    let sheet_target = target - 3.0 * molecules as f64;
    let side = sheet_target.sqrt().ceil() as u32;
    WaterSheetSettings {
        molecules,
        rows: side,
        cols: side,
        ..base.clone()
    }
}

fn human(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.2}G", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else {
        format!("{n:.0}")
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
}

fn main() {
    let args = parse_args();
    let ctx = EngineCtx::try_new_gpu();
    let Some(gpu) = ctx.gpu.clone() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let limits = gpu.device.limits();
    println!(
        "adapter: {} ({:?}) | max_buffer_size {} | max_storage_binding {}",
        gpu.adapter_info.name,
        gpu.adapter_info.backend,
        gib(limits.max_buffer_size),
        gib(limits.max_storage_buffer_binding_size as u64),
    );
    println!(
        "config: workgroup={} staged={} r_sheet={} r_mol={} steps={} batch={}",
        args.workgroup, args.staged, args.r_sheet, args.r_mol, args.steps, args.batch
    );
    println!(
        "{:>12} {:>12} {:>10} {:>10} {:>12} {:>14} {:>12} {:>9}",
        "target", "nodes", "ms/it min", "ms/it med", "nodes/s(min)", "device bytes", "bytes/node", "init_s"
    );

    let base = WaterSheetSettings {
        workgroup: args.workgroup,
        staged: args.staged,
        r_sheet: args.r_sheet,
        r_mol: args.r_mol,
        ..WaterSheetSettings::default()
    };

    for &target in &args.sizes {
        let s = shape_for(target, &base);
        let p = s.params();
        let n = p.n_nodes() as u64;

        // Cheap pre-flight against adapter limits: bonds are implicit, so the
        // largest single buffer is a position/velocity axis (4 B per node).
        let largest = 4 * n;
        if largest > limits.max_buffer_size
            || largest > limits.max_storage_buffer_binding_size as u64
        {
            println!(
                "{:>12} {:>12} SKIPPED: largest buffer ~{} exceeds adapter max_buffer_size {} / binding {}",
                human(target),
                human(n as f64),
                gib(largest),
                gib(limits.max_buffer_size),
                gib(limits.max_storage_buffer_binding_size as u64),
            );
            continue;
        }

        let t_init = Instant::now();
        let g = p.graph();
        let seed = seed_positions(&p, &s);

        // Capture device OOM / validation failures instead of panicking.
        gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut engine = WaterSheetGpuEngine::new();
        let mut ectx = EngineCtx { gpu: Some(gpu.clone()) };
        let init_res = engine
            .set_params(&serde_json::to_value(&s).unwrap())
            .and_then(|_| engine.init(&mut ectx, &CsrShard::whole(&g), &seed));
        // Force the uploads/pipeline creation through so errors surface here.
        gpu.device.poll(wgpu::Maintain::Wait);
        let val_err = pollster::block_on(gpu.device.pop_error_scope());
        let oom_err = pollster::block_on(gpu.device.pop_error_scope());
        drop(seed);
        drop(g);
        if let Some(reason) = init_res
            .err()
            .or_else(|| val_err.map(|e| format!("validation: {e}")))
            .or_else(|| oom_err.map(|e| format!("out of memory: {e}")))
        {
            println!(
                "{:>12} {:>12} SKIPPED: {reason}",
                human(target),
                human(n as f64)
            );
            continue;
        }
        let init_s = t_init.elapsed().as_secs_f64();

        // Warm-up (pipeline/driver caches), then timed submits.
        engine.step_n(2);
        engine.sync();

        let submits = (args.steps / args.batch).max(1);
        let mut per_iter_ms = Vec::with_capacity(submits as usize);
        for _ in 0..submits {
            let t = Instant::now();
            engine.step_n(args.batch);
            engine.sync();
            per_iter_ms.push(t.elapsed().as_secs_f64() * 1e3 / args.batch as f64);
        }
        per_iter_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // `min` is the headline (least contended sample on a shared GPU);
        // the median is printed beside it so noise is visible.
        let min_ms = per_iter_ms[0];
        let med_ms = per_iter_ms[per_iter_ms.len() / 2];
        let bytes = engine.device_bytes();
        println!(
            "{:>12} {:>12} {:>10.3} {:>10.3} {:>12} {:>14} {:>12.1} {:>9.1}",
            human(target),
            human(n as f64),
            min_ms,
            med_ms,
            human(n as f64 / (min_ms / 1e3)),
            gib(bytes),
            bytes as f64 / n as f64,
            init_s
        );
    }
}
