//! Native struct-of-arrays "water sheet" graph generator.
//!
//! Mirrors the shape of `waterSheet` in `crates/tvix-wasm/src/nix/graph.nix`:
//! two disconnected kinds of component in one graph,
//!
//! * a pool of `molecules` water molecules, each a 3-node path O - H1, O - H2
//!   (types `water-O` / `water-H`), with no edge between molecules;
//! * one `rows` x `cols` sheet lattice (type `sheet`), each node joined to its
//!   right and down neighbour.
//!
//! Unlike the Nix path this never materialises an attrset or JSON: nodes and
//! edges live in flat pre-sized `Vec`s (10 bytes per node, 16 bytes per edge),
//! so 100M+ node graphs build in a single O(n + e) pass.

/// Node type tag: oxygen atom of a water molecule.
pub const WATER_O: u16 = 0;
/// Node type tag: hydrogen atom of a water molecule.
pub const WATER_H: u16 = 1;
/// Node type tag: cell of the sheet lattice.
pub const SHEET: u16 = 2;

/// Type names indexed by node type tag, matching `waterSheet` in graph.nix.
pub const NODE_TYPE_NAMES: [&str; 3] = ["water-O", "water-H", "sheet"];

/// Struct-of-arrays graph: dense node ids plus parallel edge arrays.
///
/// Node ids are `0..node_count()` and index every per-node array, so the
/// arrays double as a compact, allocation-free adjacency source.
#[derive(Debug, Default)]
pub struct Graph {
    /// Node ids; id == its index into `node_types`.
    pub node_ids: Vec<u64>,
    /// Node type tags, one of [`WATER_O`], [`WATER_H`], [`SHEET`].
    pub node_types: Vec<u16>,
    /// Edge endpoint node indices, paired with `edge_targets`.
    pub edge_sources: Vec<u64>,
    /// Edge endpoint node indices, paired with `edge_sources`.
    pub edge_targets: Vec<u64>,
}

impl Graph {
    /// Number of nodes.
    pub fn node_count(&self) -> u64 {
        self.node_ids.len() as u64
    }

    /// Number of edges (undirected bonds stored once).
    pub fn edge_count(&self) -> u64 {
        self.edge_sources.len() as u64
    }

    /// Type name of node `id`, if it exists.
    pub fn node_type_name(&self, id: u64) -> Option<&'static str> {
        let tag = *self.node_types.get(usize::try_from(id).ok()?)?;
        NODE_TYPE_NAMES.get(usize::from(tag)).copied()
    }
}

/// Deterministic water-sheet generator (same topology as `waterSheet`).
#[derive(Debug, Clone, Copy)]
pub struct WaterSheetGenerator {
    /// Number of disconnected water molecules.
    pub molecules: u64,
    /// Sheet lattice rows.
    pub rows: u64,
    /// Sheet lattice columns.
    pub cols: u64,
}

impl WaterSheetGenerator {
    /// Build a generator for `molecules` water molecules plus a `rows` x `cols`
    /// sheet lattice.
    pub fn new(molecules: u64, rows: u64, cols: u64) -> Self {
        Self {
            molecules,
            rows,
            cols,
        }
    }

    /// Nodes in the sheet lattice (`rows * cols`).
    pub fn sheet_cells(&self) -> u64 {
        self.rows * self.cols
    }

    /// Node count this generator produces: 3 per molecule plus one per cell.
    pub fn expected_nodes(&self) -> u64 {
        self.molecules * 3 + self.sheet_cells()
    }

    /// Edge count this generator produces: 2 bonds per molecule (O-H1, O-H2)
    /// plus the sheet's right and down bonds (`2*rows*cols - rows - cols`).
    pub fn expected_edges(&self) -> u64 {
        self.molecules * 2 + self.rows * self.cols * 2 - self.rows - self.cols
    }

    /// Build the graph in one O(n + e) pass.
    pub fn generate(&self) -> Graph {
        let molecule_nodes = self.molecules * 3;
        let cells = self.sheet_cells();
        let node_count = molecule_nodes + cells;
        let edge_count = self.expected_edges();

        let node_ids: Vec<u64> = (0..node_count).collect();
        let mut node_types = vec![SHEET; node_ids.len()];
        // Molecule region: O, H1, H2 triples in waterSheet node order.
        for triple in node_types[..molecule_nodes as usize].chunks_exact_mut(3) {
            triple[0] = WATER_O;
            triple[1] = WATER_H;
            triple[2] = WATER_H;
        }

        let mut edge_sources = Vec::with_capacity(edge_count as usize);
        let mut edge_targets = Vec::with_capacity(edge_count as usize);

        // Molecule bonds: O-H1 then O-H2, undirected and stored once.
        for m in 0..self.molecules {
            let oxygen = m * 3;
            edge_sources.push(oxygen);
            edge_targets.push(oxygen + 1);
            edge_sources.push(oxygen);
            edge_targets.push(oxygen + 2);
        }

        // Sheet bonds: right neighbour while c + 1 < cols, then down neighbour
        // while r + 1 < rows (same order and rule as waterSheet's sh/sv).
        for i in 0..cells {
            let r = i / self.cols;
            let c = i - self.cols * r;
            let id = molecule_nodes + i;
            if c + 1 < self.cols {
                edge_sources.push(id);
                edge_targets.push(id + 1);
            }
            if r + 1 < self.rows {
                edge_sources.push(id);
                edge_targets.push(id + self.cols);
            }
        }

        debug_assert_eq!(node_ids.len() as u64, node_count);
        debug_assert_eq!(node_types.len() as u64, node_count);
        debug_assert_eq!(edge_sources.len() as u64, edge_count);
        debug_assert_eq!(edge_targets.len() as u64, edge_count);

        Graph {
            node_ids,
            node_types,
            edge_sources,
            edge_targets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOLECULES: u64 = 1_000;
    const ROWS: u64 = 64;
    const COLS: u64 = 64;

    fn generator() -> WaterSheetGenerator {
        WaterSheetGenerator::new(MOLECULES, ROWS, COLS)
    }

    /// Undirected adjacency lists; only used on test-sized graphs.
    fn adjacency(graph: &Graph) -> Vec<Vec<u64>> {
        let mut adj = vec![Vec::new(); graph.node_count() as usize];
        for (&source, &target) in graph.edge_sources.iter().zip(&graph.edge_targets) {
            adj[source as usize].push(target);
            adj[target as usize].push(source);
        }
        adj
    }

    /// Iterative flood fill from `start`; returns the visited node ids.
    fn reachable(adj: &[Vec<u64>], start: u64) -> Vec<u64> {
        let mut seen = vec![false; adj.len()];
        let mut stack = vec![start];
        seen[start as usize] = true;
        let mut visited = Vec::new();
        while let Some(node) = stack.pop() {
            visited.push(node);
            for &next in &adj[node as usize] {
                if !seen[next as usize] {
                    seen[next as usize] = true;
                    stack.push(next);
                }
            }
        }
        visited
    }

    #[test]
    fn counts_match_water_sheet_formulas() {
        let graph = generator().generate();
        assert_eq!(graph.node_count(), 3 * MOLECULES + ROWS * COLS);
        assert_eq!(graph.node_count(), 7_096);
        assert_eq!(
            graph.edge_count(),
            2 * MOLECULES + 2 * ROWS * COLS - ROWS - COLS
        );
        assert_eq!(graph.edge_count(), 10_064);
        assert_eq!(generator().expected_nodes(), graph.node_count());
        assert_eq!(generator().expected_edges(), graph.edge_count());
        // Every edge endpoint is a live node.
        assert!(graph.edge_sources.iter().all(|&id| id < graph.node_count()));
        assert!(graph.edge_targets.iter().all(|&id| id < graph.node_count()));
    }

    #[test]
    fn node_types_match_water_sheet_layout() {
        let graph = generator().generate();
        let molecules_end = 3 * MOLECULES as usize;
        for m in 0..MOLECULES as usize {
            assert_eq!(
                &graph.node_types[3 * m..3 * m + 3],
                &[WATER_O, WATER_H, WATER_H],
                "molecule {m}"
            );
        }
        assert!(graph.node_types[molecules_end..].iter().all(|&t| t == SHEET));
        assert_eq!(graph.node_type_name(0), Some("water-O"));
        assert_eq!(graph.node_type_name(1), Some("water-H"));
        assert_eq!(graph.node_type_name(molecules_end as u64), Some("sheet"));
        assert_eq!(graph.node_type_name(graph.node_count()), None);
    }

    #[test]
    fn molecules_are_3_node_2_edge_components() {
        let graph = generator().generate();
        let adj = adjacency(&graph);
        for m in [0, MOLECULES / 2, MOLECULES - 1] {
            let oxygen = m * 3;
            let mut component = reachable(&adj, oxygen);
            component.sort_unstable();
            assert_eq!(
                component,
                vec![oxygen, oxygen + 1, oxygen + 2],
                "molecule {m}"
            );
            // Two undirected bonds per molecule: total degree == 4.
            let degree: usize = component.iter().map(|&n| adj[n as usize].len()).sum();
            assert_eq!(degree, 4, "molecule {m}");
            // Oxygen is the hub; the hydrogens do not bond to each other.
            assert_eq!(adj[oxygen as usize], vec![oxygen + 1, oxygen + 2]);
            assert_eq!(adj[oxygen as usize + 1], vec![oxygen]);
            assert_eq!(adj[oxygen as usize + 2], vec![oxygen]);
        }
    }

    #[test]
    fn sheet_is_one_connected_lattice() {
        let graph = generator().generate();
        let adj = adjacency(&graph);
        let sheet_start = 3 * MOLECULES;
        let component = reachable(&adj, sheet_start);
        assert_eq!(component.len() as u64, ROWS * COLS);
        assert!(component.iter().all(|&id| id >= sheet_start));
        // Right bonds (rows * (cols - 1)) plus down bonds ((rows - 1) * cols).
        let bonds: usize = component.iter().map(|&n| adj[n as usize].len()).sum::<usize>() / 2;
        assert_eq!(bonds as u64, 2 * ROWS * COLS - ROWS - COLS);
        assert_eq!(bonds as u64, 8_064);
    }

    #[test]
    fn whole_graph_is_molecules_plus_one_sheet() {
        let graph = generator().generate();
        let adj = adjacency(&graph);
        let mut seen = vec![false; adj.len()];
        let mut sizes = Vec::new();
        for start in 0..adj.len() as u64 {
            if seen[start as usize] {
                continue;
            }
            let component = reachable(&adj, start);
            for &id in &component {
                seen[id as usize] = true;
            }
            sizes.push(component.len());
        }
        sizes.sort_unstable();
        assert_eq!(sizes.len() as u64, MOLECULES + 1);
        assert!(sizes[..MOLECULES as usize].iter().all(|&size| size == 3));
        assert_eq!(sizes[MOLECULES as usize], (ROWS * COLS) as usize);
    }

    #[test]
    fn degenerate_sizes() {
        let empty = WaterSheetGenerator::new(0, 0, 0).generate();
        assert_eq!((empty.node_count(), empty.edge_count()), (0, 0));

        let single_cell = WaterSheetGenerator::new(0, 1, 1).generate();
        assert_eq!((single_cell.node_count(), single_cell.edge_count()), (1, 0));

        // A 1 x N row has N - 1 right bonds and no down bonds.
        let row = WaterSheetGenerator::new(0, 1, 5).generate();
        assert_eq!((row.node_count(), row.edge_count()), (5, 4));

        // One molecule plus a 2 x 2 sheet: 4 sheet bonds.
        let mixed = WaterSheetGenerator::new(1, 2, 2).generate();
        assert_eq!((mixed.node_count(), mixed.edge_count()), (7, 6));
    }
}
