//! End-to-end: native SoA generator -> GPU water-sheet layout, ~100M nodes.
//!
//! 1. Generate the `waterSheet` graph with `native_graph_generator` (the
//!    source of truth for node order and bonds; a dev-dependency here), timed
//!    as `generate_s`.
//! 2. Verify the generator's node/edge counts and per-node kinds against
//!    `WaterSheetParams` (the GPU side's implicit topology; equality is
//!    proven exhaustively by `tests/water_topology_matches_generator.rs`).
//! 3. Run the GPU engine `steps` iterations and report ms/iter (min/median),
//!    nodes/s and device bytes.
//! 4. Read the final positions back and report the geometry the layout exists
//!    to produce — O–H bond error, lattice spacing error, lattice offset —
//!    so the run shows convergence, not just execution.
//!
//! ```text
//! cargo run -p graph-compute --release --example water_e2e -- \
//!     [--molecules 30000000] [--rows 3163] [--cols 3163] [--steps 300] [--batch 10]
//! ```
//!
//! Defaults: 30,000,000 molecules (90.0M nodes) + a 3,163 x 3,163 sheet
//! (10,004,569 nodes) = 100,004,569 nodes, 80,002,812 undirected bonds.

use std::time::Instant;

use graph_compute::engines::WaterSheetGpuEngine;
use graph_compute::water::cpu::{geometry_report, Positions};
use graph_compute::water::{seed_positions, WaterSheetParams, WaterSheetSettings};
use graph_compute::{CsrShard, EngineCtx, LayoutEngine};
use native_graph_generator::{SHEET, WaterSheetGenerator, WATER_H, WATER_O};

struct Args {
    molecules: u32,
    rows: u32,
    cols: u32,
    steps: u32,
    batch: u32,
}

fn parse_args() -> Args {
    let mut a = Args {
        molecules: 30_000_000,
        rows: 3_163,
        cols: 3_163,
        steps: 300,
        batch: 10,
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
            "--molecules" => a.molecules = val(i).parse().expect("molecules"),
            "--rows" => a.rows = val(i).parse().expect("rows"),
            "--cols" => a.cols = val(i).parse().expect("cols"),
            "--steps" => a.steps = val(i).parse().expect("steps"),
            "--batch" => a.batch = val(i).parse().expect("batch"),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a.batch = a.batch.clamp(1, a.steps.max(1));
    a
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
}

fn main() {
    let a = parse_args();
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

    // 1. Native generation (the oracle).
    let generator = WaterSheetGenerator::new(
        u64::from(a.molecules),
        u64::from(a.rows),
        u64::from(a.cols),
    );
    let t_gen = Instant::now();
    let reference = generator.generate();
    let generate_s = t_gen.elapsed().as_secs_f64();
    let params = WaterSheetParams::new(a.molecules, a.rows, a.cols);
    println!(
        "generate: molecules={} sheet={}x{} | nodes={} bonds={} | generate_s={:.2}",
        a.molecules,
        a.rows,
        a.cols,
        reference.node_count(),
        reference.edge_count(),
        generate_s,
    );

    // 2. Cross-check the generator against the GPU side's shape.
    assert_eq!(
        reference.node_count(),
        params.n_nodes() as u64,
        "generator / WaterSheetParams node count"
    );
    assert_eq!(
        reference.edge_count(),
        params.n_edges() as u64,
        "generator / WaterSheetParams edge count"
    );
    let mut counts = [0u64; 3];
    for &tag in &reference.node_types {
        match tag {
            WATER_O => counts[0] += 1,
            WATER_H => counts[1] += 1,
            SHEET => counts[2] += 1,
            other => panic!("unknown node type tag {other}"),
        }
    }
    assert_eq!(counts[0], u64::from(a.molecules), "oxygen count");
    assert_eq!(counts[1], 2 * u64::from(a.molecules), "hydrogen count");
    assert_eq!(counts[2], u64::from(a.rows) * u64::from(a.cols), "sheet count");
    println!(
        "verify: nodes + bonds + kinds (O={} H={} sheet={}) match WaterSheetParams",
        counts[0], counts[1], counts[2]
    );
    drop(reference);

    // 3. GPU init: CSR (counts validated by the engine) + canonical seed.
    let s = WaterSheetSettings {
        molecules: a.molecules,
        rows: a.rows,
        cols: a.cols,
        ..WaterSheetSettings::default()
    };
    let n = params.n_nodes() as u64;
    let largest = 4 * n;
    if largest > limits.max_buffer_size
        || largest > limits.max_storage_buffer_binding_size as u64
    {
        eprintln!(
            "SKIPPED: largest buffer ~{} exceeds adapter max_buffer_size {} / binding {}",
            gib(largest),
            gib(limits.max_buffer_size),
            gib(limits.max_storage_buffer_binding_size as u64),
        );
        return;
    }

    let t_init = Instant::now();
    let csr = params.graph();
    let seed = seed_positions(&params, &s);

    gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut engine = WaterSheetGpuEngine::new();
    let mut ectx = EngineCtx { gpu: Some(gpu.clone()) };
    let init_res = engine
        .set_params(&serde_json::to_value(&s).unwrap())
        .and_then(|_| engine.init(&mut ectx, &CsrShard::whole(&csr), &seed));
    gpu.device.poll(wgpu::Maintain::Wait);
    let val_err = pollster::block_on(gpu.device.pop_error_scope());
    let oom_err = pollster::block_on(gpu.device.pop_error_scope());
    drop(seed);
    drop(csr);
    if let Some(reason) = init_res
        .err()
        .or_else(|| val_err.map(|e| format!("validation: {e}")))
        .or_else(|| oom_err.map(|e| format!("out of memory: {e}")))
    {
        eprintln!("FAILED: {reason}");
        std::process::exit(1);
    }
    let init_s = t_init.elapsed().as_secs_f64();

    // 4. Warm-up, then timed steps.
    engine.step_n(2);
    engine.sync();

    let submits = (a.steps / a.batch).max(1);
    let total_steps = submits * a.batch;
    let mut per_iter_ms = Vec::with_capacity(submits as usize);
    for _ in 0..submits {
        let t = Instant::now();
        engine.step_n(a.batch);
        engine.sync();
        per_iter_ms.push(t.elapsed().as_secs_f64() * 1e3 / a.batch as f64);
    }
    per_iter_ms.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let min_ms = per_iter_ms[0];
    let med_ms = per_iter_ms[per_iter_ms.len() / 2];
    let bytes = engine.device_bytes();
    println!(
        "run: init_s={init_s:.2} steps={total_steps} | ms/it min={min_ms:.3} med={med_ms:.3} \
         | nodes/s(min)={:.2}G | device bytes {} ({:.1} B/node)",
        n as f64 / (min_ms / 1e3) / 1e9,
        gib(bytes),
        bytes as f64 / n as f64,
    );

    // 5. Post-run geometry: the layout must actually converge.
    let t_geo = Instant::now();
    let positions = Positions::from_interleaved(&engine.read_positions());
    let r = geometry_report(&positions, &s);
    println!(
        "geometry after {total_steps} steps: max|O-H - bond_rest|={:.4} (bond_rest={:.2}) \
         | max|spacing - sheet_rest|={:.4} (sheet_rest={:.2}) | max lattice offset={:.4} \
         | readback+report {:.1}s",
        r.max_oh_err,
        s.bond_rest,
        r.max_sheet_spacing_err,
        s.sheet_rest,
        r.max_lattice_offset,
        t_geo.elapsed().as_secs_f64(),
    );
}
