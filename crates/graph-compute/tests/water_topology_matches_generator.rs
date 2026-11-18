//! Topology equality: the native SoA `WaterSheetGenerator` (source of truth)
//! and the GPU water-sheet layout's implicit topology must be the same graph.
//!
//! `native-graph-generator` is a **dev-dependency** only: this test and the
//! `water_e2e` example build the reference graph natively, while the runtime
//! path never imports the generator.
//!
//! For every shape under test:
//!   - node count: `Graph::node_count()` == `WaterSheetParams::n_nodes()`;
//!   - per-node kind: the generator's explicit `node_types` tag (O / H /
//!     sheet) equals the kind the GPU side derives from the node index
//!     (`O = 3m`, `H1 = 3m+1`, `H2 = 3m+2` molecule prefix, then a row-major
//!     sheet from `sheet_base()`);
//!   - undirected edge set: the generator's SoA bond list (each bond stored
//!     once) equals the CSR of `WaterSheetParams::graph()` (each undirected
//!     bond stored in both directions), compared as ordered endpoint pairs.
//!
//! The GPU side is the thing under test here: the generator is the oracle.
//!
//! ```text
//! cargo test -p graph-compute --test water_topology_matches_generator
//! ```

use std::collections::HashSet;

use graph_compute::water::WaterSheetParams;
use native_graph_generator::{Graph, SHEET, WaterSheetGenerator, WATER_H, WATER_O};

/// Node kind tag used by the comparison (generator tags are `u16`).
const KIND_O: u16 = WATER_O;
const KIND_H: u16 = WATER_H;
const KIND_SHEET: u16 = SHEET;

/// Kind implied by the GPU side's node index (documented in `water/mod.rs`).
fn gpu_kind(params: &WaterSheetParams, i: u32) -> u16 {
    if i < params.sheet_base() {
        if i % 3 == 0 {
            KIND_O
        } else {
            KIND_H
        }
    } else {
        KIND_SHEET
    }
}

/// Generator kind at index `i`.
fn generator_kind(graph: &Graph, i: u64) -> u16 {
    match graph.node_types[i as usize] {
        WATER_O | WATER_H | SHEET => graph.node_types[i as usize],
        other => panic!("unknown node type tag {other} at node {i}"),
    }
}

/// The generator's undirected edge set, as canonical `(min, max)` pairs.
fn generator_edges(graph: &Graph) -> HashSet<(u64, u64)> {
    graph
        .edge_sources
        .iter()
        .zip(&graph.edge_targets)
        .map(|(&a, &b)| if a <= b { (a, b) } else { (b, a) })
        .collect()
}

/// The CSR's undirected edge set, as canonical `(min, max)` pairs.
fn csr_edges(graph: &graph_compute::sim::CsrGraph) -> HashSet<(u64, u64)> {
    let mut set = HashSet::with_capacity(graph.neighbors.len() / 2);
    for i in 0..graph.n_nodes {
        let start = graph.offsets[i as usize] as usize;
        let end = graph.offsets[i as usize + 1] as usize;
        for &n in &graph.neighbors[start..end] {
            let (a, b) = if i <= n { (i as u64, n as u64) } else { (n as u64, i as u64) };
            set.insert((a, b));
        }
    }
    set
}

/// Assert that the GPU layout for `(molecules, rows, cols)` reproduces the
/// generator's graph exactly.
fn assert_same_topology(molecules: u64, rows: u64, cols: u64) {
    let generator = WaterSheetGenerator::new(molecules, rows, cols);
    let reference = generator.generate();
    let params = WaterSheetParams::new(molecules as u32, rows as u32, cols as u32);
    let csr = params.graph();

    let ctx = format!("molecules={molecules} rows={rows} cols={cols}");

    // 1. Node count.
    assert_eq!(
        reference.node_count(),
        params.n_nodes() as u64,
        "{ctx}: generator and params disagree on node count"
    );
    assert_eq!(
        reference.node_count(),
        csr.n_nodes as u64,
        "{ctx}: CSR node count"
    );
    assert_eq!(
        reference.edge_count(),
        params.n_edges() as u64,
        "{ctx}: generator and params disagree on edge count"
    );

    // 2. Per-index node kind.
    for i in 0..reference.node_count() {
        assert_eq!(
            generator_kind(&reference, i),
            gpu_kind(&params, i as u32),
            "{ctx}: node {i} kind disagrees (generator vs GPU index rule)"
        );
    }

    // 3. Undirected edge set.
    let expected = generator_edges(&reference);
    let actual = csr_edges(&csr);
    assert_eq!(expected.len(), reference.edge_count() as usize, "{ctx}: oracle edges duplicate");
    assert_eq!(actual.len(), params.n_edges() as usize, "{ctx}: CSR edges duplicate");
    let missing: Vec<_> = expected.difference(&actual).take(8).collect();
    let extra: Vec<_> = actual.difference(&expected).take(8).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{ctx}: edge sets differ; {} generator edge(s) missing from CSR (e.g. {missing:?}); \
         {} CSR edge(s) missing from generator (e.g. {extra:?})",
        expected.difference(&actual).count(),
        actual.difference(&expected).count(),
    );
}

#[test]
fn topology_matches_generator() {
    // Degenerate, single-axis, ragged, and a realistic 20k-molecule shape.
    let cases: [(u64, u64, u64); 7] = [
        (0, 0, 0),           // truly empty graph
        (0, 1, 1),           // 0 molecules, single sheet cell
        (1, 1, 1),           // minimal mixed graph
        (2, 1, 5),           // 1xN row: right bonds only
        (2, 5, 1),           // Nx1 column: down bonds only
        (7, 3, 4),           // ragged small shape
        (20_000, 256, 256),  // 125,536 nodes / 170,304 bonds
    ];
    for (molecules, rows, cols) in cases {
        eprintln!("--- checking molecules={molecules} rows={rows} cols={cols}");
        assert_same_topology(molecules, rows, cols);
    }
}
