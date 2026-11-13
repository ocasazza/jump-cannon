//! CLI: generate a water sheet, print counts, wall time and peak RSS.
//!
//! Usage: `native-graph-generator [--molecules N] [--rows R] [--cols C]`
//! (defaults match `waterSheet` in graph.nix: 1_000 molecules, 64 x 64 sheet).

use std::process::ExitCode;
use std::time::Instant;

use native_graph_generator::{WaterSheetGenerator, NODE_TYPE_NAMES, SHEET, WATER_H, WATER_O};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("native-graph-generator: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut generator = WaterSheetGenerator::new(1_000, 64, 64);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        if matches!(flag, "-h" | "--help") {
            eprintln!("usage: native-graph-generator [--molecules N] [--rows R] [--cols C]");
            return Ok(());
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {flag}"))?;
        let number = value
            .parse::<u64>()
            .map_err(|err| format!("invalid value for {flag}: {err}"))?;
        match flag {
            "--molecules" => generator.molecules = number,
            "--rows" => generator.rows = number,
            "--cols" => generator.cols = number,
            other => return Err(format!("unknown flag {other}")),
        }
        index += 2;
    }

    let expected_nodes = generator.expected_nodes();
    let expected_edges = generator.expected_edges();

    let start = Instant::now();
    let graph = generator.generate();
    let wall = start.elapsed();
    let peak_rss = peak_rss_bytes();

    let nodes = graph.node_count();
    let edges = graph.edge_count();
    if (nodes, edges) != (expected_nodes, expected_edges) {
        return Err(format!(
            "generated {nodes} nodes / {edges} edges, expected {expected_nodes} / {expected_edges}"
        ));
    }

    let water_o = graph.node_types.iter().filter(|&&t| t == WATER_O).count();
    let water_h = graph.node_types.iter().filter(|&&t| t == WATER_H).count();
    let sheet = graph.node_types.iter().filter(|&&t| t == SHEET).count();
    let seconds = wall.as_secs_f64();
    let rate = |count: u64| count as f64 / seconds;

    println!(
        "water sheet: {} molecules, {} x {} sheet ({} cells)",
        generator.molecules, generator.rows, generator.cols, sheet
    );
    println!(
        "nodes: {nodes} ({} {water_o}, {} {water_h}, {} {sheet})",
        NODE_TYPE_NAMES[WATER_O as usize],
        NODE_TYPE_NAMES[WATER_H as usize],
        NODE_TYPE_NAMES[SHEET as usize],
    );
    println!("edges: {edges}");
    println!("wall:  {seconds:.3} s");
    println!(
        "rate:  {:.1} M nodes/s, {:.1} M edges/s",
        rate(nodes) / 1e6,
        rate(edges) / 1e6
    );
    println!(
        "peak RSS: {peak_rss} bytes ({:.2} GiB)",
        peak_rss as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "per element (peak RSS): {:.1} B/node, {:.1} B/edge",
        peak_rss as f64 / nodes as f64,
        peak_rss as f64 / edges as f64
    );

    Ok(())
}

/// Peak resident set size of this process in bytes.
fn peak_rss_bytes() -> u64 {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if status != 0 {
            return 0;
        }
        // ru_maxrss is bytes on macOS, kilobytes on Linux.
        #[cfg(target_os = "macos")]
        {
            usage.ru_maxrss as u64
        }
        #[cfg(target_os = "linux")]
        {
            usage.ru_maxrss as u64 * 1024
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}
