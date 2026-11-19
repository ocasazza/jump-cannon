//! GPU-resident water-sheet layout: topology contract, SoA position storage,
//! deterministic CPU reference, and the node-id mapping shared by the CPU path
//! and the WGSL kernels.
//!
//! Topology mirrors `waterSheet` in `crates/tvix-wasm/src/nix/graph.nix`
//! exactly:
//!
//!   - molecule `m` (0..M): `O = 3m`, `H1 = 3m+1`, `H2 = 3m+2`, with undirected
//!     bonds `O–H1` and `O–H2` (each molecule is a 3-node path);
//!   - sheet node `(r, c)`: id `3M + r*cols + c` (row-major), bonded to its
//!     right and down neighbours.
//!
//! The two component kinds are DISCONNECTED (no edge between a molecule and
//! the sheet), which is what makes the windowed GPU design exact rather than
//! approximate: the sheet pass and the molecule pass read/write disjoint node
//! ranges and can dispatch independently over the same ping-pong buffers.
//!
//! Layout forces ("repulsion + sheet-bond springs + lattice spacing"):
//!   - bond springs (O–H at `bond_rest`, sheet at `sheet_rest`) along the CSR
//!     edges;
//!   - bounded-window soft repulsion so nodes keep spacing: a sheet node
//!     repels sheet nodes within Chebyshev radius `r_sheet` in the lattice
//!     grid; a molecule's O repels other O's within radius `r_mol` in the
//!     molecule grid (`mol_grid_w × ⌈M/mol_grid_w⌉`, row-major, so the
//!     index-space window is a spatial window for the canonical seed).
//!
//! Every node's force is a fixed-order sum over a bounded window — no atomics,
//! no cross-thread reduction — so the CPU reference and the GPU kernels
//! compute the same quantity and only floating-point contraction order can
//! differ (compared within tolerance, never bit-identical).

use crate::sim::CsrGraph;

pub mod cpu;

/// Nodes per molecule (O, H1, H2).
pub const MOLECULE_STRIDE: u32 = 3;

/// The water-sheet shape: `n_nodes = 3*molecules + rows*cols`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaterSheetParams {
    pub molecules: u32,
    pub rows: u32,
    pub cols: u32,
}

impl WaterSheetParams {
    /// Build params for `molecules` molecules plus a `rows` x `cols` sheet —
    /// the same shape `WaterSheetGenerator::new` of the native SoA generator
    /// produces (dev-dep oracle), so both sides can be constructed from the
    /// identical triple.
    pub fn new(molecules: u32, rows: u32, cols: u32) -> Self {
        Self {
            molecules,
            rows,
            cols,
        }
    }

    pub fn n_nodes(&self) -> u32 {
        MOLECULE_STRIDE * self.molecules + self.rows * self.cols
    }

    /// Molecule bonds (2 per molecule) + sheet bonds (right + down, missing at
    /// the right/bottom edges).
    pub fn n_edges(&self) -> u32 {
        2 * self.molecules + 2 * self.rows * self.cols - self.rows - self.cols
    }

    /// First sheet node id.
    pub fn sheet_base(&self) -> u32 {
        MOLECULE_STRIDE * self.molecules
    }

    /// Width of the row-major molecule grid used for O–O windowed repulsion.
    /// ⌈√M⌉ so the grid is near-square and index windows stay spatially local.
    pub fn mol_grid_w(&self) -> u32 {
        if self.molecules == 0 {
            return 1;
        }
        ((self.molecules as f64).sqrt().ceil() as u32).max(1)
    }

    /// Rows of the molecule grid.
    pub fn mol_grid_h(&self) -> u32 {
        if self.molecules == 0 {
            return 1;
        }
        self.molecules.div_ceil(self.mol_grid_w())
    }

    /// Build the canonical `waterSheet` CSR. Undirected bonds are stored in
    /// both directions (the `CsrGraph` symmetric-adjacency convention), so a
    /// sheet node's neighbours are, in fixed order: up, left, right, down.
    pub fn graph(&self) -> CsrGraph {
        let n = self.n_nodes();
        let mut offsets = Vec::with_capacity(n as usize + 1);
        let mut neighbors = Vec::with_capacity(self.n_edges() as usize * 2);
        for m in 0..self.molecules {
            let o = MOLECULE_STRIDE * m;
            // O → H1, H2
            offsets.push(neighbors.len() as u32);
            neighbors.push(o + 1);
            neighbors.push(o + 2);
            // H1 → O
            offsets.push(neighbors.len() as u32);
            neighbors.push(o);
            // H2 → O
            offsets.push(neighbors.len() as u32);
            neighbors.push(o);
        }
        let sb = self.sheet_base();
        for i in 0..self.rows * self.cols {
            offsets.push(neighbors.len() as u32);
            let r = i / self.cols;
            let c = i - self.cols * r;
            if r > 0 {
                neighbors.push(sb + i - self.cols);
            }
            if c > 0 {
                neighbors.push(sb + i - 1);
            }
            if c + 1 < self.cols {
                neighbors.push(sb + i + 1);
            }
            if r + 1 < self.rows {
                neighbors.push(sb + i + self.cols);
            }
        }
        offsets.push(neighbors.len() as u32);
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// Validate that a caller-supplied CSR really is a `waterSheet` graph of
    /// this shape (node count + edge count). The engine calls this at `init`
    /// so a mismatched graph is rejected instead of silently mis-indexed.
    pub fn validate_csr(&self, g: &CsrGraph) -> Result<(), String> {
        if g.n_nodes != self.n_nodes() {
            return Err(format!(
                "water-sheet: graph has {} nodes, params ({}, {}x{}) need {}",
                g.n_nodes,
                self.molecules,
                self.rows,
                self.cols,
                self.n_nodes()
            ));
        }
        let deg_sum = *g.offsets.last().unwrap_or(&0);
        if deg_sum != 2 * self.n_edges() {
            return Err(format!(
                "water-sheet: graph has {deg_sum} directed adjacency entries, \
                 a ({molecules}, {rows}x{cols}) waterSheet needs {}",
                2 * self.n_edges(),
                molecules = self.molecules,
                rows = self.rows,
                cols = self.cols,
            ));
        }
        Ok(())
    }

    /// Infer the `(molecules, rows, cols)` shape of a CSR built in
    /// `waterSheet` node order (molecule prefix, then row-major sheet), so a
    /// bare graph push — the UI engine picker or the graph-api compute broker,
    /// which send the CSR with no shape — can select this engine without
    /// `set_params`.
    ///
    /// Recovery:
    ///   - the molecule prefix is the longest run, from node 0, of 3-node
    ///     H–O–H paths: `O = 3m` bonded to exactly `{O+1, O+2}`, each H to
    ///     exactly `{O}`;
    ///   - the sheet is the remaining `cells = n - 3M` nodes. Its top-left
    ///     cell `s` has at most two neighbours — right (`s+1`) and down
    ///     (`s+cols`) — so `cols` is `(smallest neighbour of s greater than
    ///     s+1) - s` when one exists, else the sheet is a single row
    ///     (`cols = cells`);
    ///   - `rows = cells / cols`, which must divide exactly.
    ///
    /// A 1-column sheet is a simple path — the SAME CSR as the 1-row sheet of
    /// the same length — so `Nx1` canonicalises to the single-row form
    /// `(1, N)`. For every shape the recovered params reproduce the input
    /// graph (`params.graph() == csr`), and a shape-matching `set_params`
    /// round-trips exactly.
    ///
    /// Returns `Err` for a remainder that is not a valid sheet (a neighbour of
    /// the base cell pointing back at the molecule prefix, or a cell count
    /// that does not fill `cols` columns), and re-validates the result with
    /// [`validate_csr`] so a count-mismatched graph is still rejected.
    pub fn infer_from_csr(g: &CsrGraph) -> Result<Self, String> {
        let n = g.n_nodes;
        let nbrs = |i: u32| -> &[u32] {
            let (a, b) = (g.offsets[i as usize] as usize, g.offsets[(i + 1) as usize] as usize);
            &g.neighbors[a..b]
        };
        // Molecule prefix: each O bonded to exactly {O+1, O+2}, each H to {O}.
        let mut molecules = 0u32;
        while 3 * molecules + 2 < n {
            let o = 3 * molecules;
            let is_o = nbrs(o).len() == 2
                && nbrs(o).contains(&(o + 1))
                && nbrs(o).contains(&(o + 2));
            let is_h = |h: u32| nbrs(h).len() == 1 && nbrs(h)[0] == o;
            if is_o && is_h(o + 1) && is_h(o + 2) {
                molecules += 1;
            } else {
                break;
            }
        }
        let s = 3 * molecules;
        let cells = n - s;
        let (rows, cols) = if cells == 0 {
            (0, 0)
        } else {
            let sn = nbrs(s);
            if sn.iter().any(|&v| v <= s) {
                return Err(format!(
                    "water-sheet: cannot infer shape: sheet base node {s} is bonded \
                     to a node <= {s} (molecules and sheet must be disconnected)"
                ));
            }
            // The base cell's neighbours are right (s+1) and/or down (s+cols);
            // a down neighbour is the smallest one greater than s+1. A
            // 1-column sheet's only bond IS the down bond s->s+1, so it falls
            // into the no-down branch and canonicalises to the identical
            // single-row path.
            let cols = match sn.iter().find(|&&v| v > s + 1).copied() {
                Some(down) => down - s,
                None => cells,
            };
            if cells % cols != 0 {
                return Err(format!(
                    "water-sheet: cannot infer shape: {cells} sheet cells do not fill \
                     {cols} columns"
                ));
            }
            (cells / cols, cols)
        };
        let params = Self { molecules, rows, cols };
        params.validate_csr(g)?;
        Ok(params)
    }
}

/// Numeric precision of the GPU-resident water-sheet state.
///
/// - `F32`: the original kernel — f32 positions, velocities and forces.
/// - `F64Pos`: high-precision positions stored as an exact 64-bit fixed-point
///   pair (scale 2^-40), which needs no `SHADER_F64` feature and no fp64
///   hardware; f32 velocities and forces. Forces are computed from the
///   positions rounded to f32, the velocity is integrated in f32, and the f32
///   increment is added to the fixed-point position with an exact integer add.
/// - `F64`: positions, velocities and forces all f64 (native WGSL `f64`,
///   requires `wgpu::Features::SHADER_F64`; on an adapter without it — Apple
///   GPUs expose no fp64 — the engine rejects the mode with an error naming
///   it, never a silent precision downgrade).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaterPrecision {
    #[default]
    F32,
    F64Pos,
    F64,
}

/// Tunables for the water-sheet layout. Carried in `set_params` JSON by the
/// engine, and used verbatim by the CPU reference so both paths share one
/// source of truth for the physics constants.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WaterSheetSettings {
    /// waterSheet shape — must match the CSR handed to `init`.
    pub molecules: u32,
    pub rows: u32,
    pub cols: u32,

    /// Numeric precision of the GPU state (see [`WaterPrecision`]).
    pub precision: WaterPrecision,

    /// O–H bond rest length.
    pub bond_rest: f32,
    /// Sheet lattice spacing (spring rest between lattice neighbours).
    pub sheet_rest: f32,

    /// Bond spring stiffness.
    pub k_bond: f32,
    /// Windowed soft-repulsion strength.
    pub k_rep: f32,
    /// Repulsion cutoff radius (beyond it the force is exactly zero).
    pub rcut: f32,

    /// Semi-implicit Euler step.
    pub dt: f32,
    /// Per-step velocity retention.
    pub damping: f32,

    /// Sheet repulsion window radius (Chebyshev, in lattice cells).
    pub r_sheet: u32,
    /// Molecule repulsion window radius (Chebyshev, in molecule-grid cells).
    pub r_mol: u32,

    /// Use the shared-memory staged sheet kernel (2-D dispatch tiling). Its
    /// shared tile is sized from `r_sheet` at pipeline build (`r_sheet <= 8`).
    pub staged: bool,
    /// Workgroup size for the 1-D kernels (naive + molecule pass).
    pub workgroup: u32,
}

impl Default for WaterSheetSettings {
    fn default() -> Self {
        // Defaults mirror the `waterSheet` nix defaults (1000 molecules,
        // 64×64 sheet) and physics constants tuned so the deterministic CPU
        // path converges to the lattice within ~200 steps.
        Self {
            molecules: 1000,
            rows: 64,
            cols: 64,
            precision: WaterPrecision::default(),
            bond_rest: 0.96,
            sheet_rest: 1.0,
            k_bond: 4.0,
            k_rep: 2.0,
            rcut: 1.6,
            dt: 0.05,
            damping: 0.9,
            r_sheet: 1,
            r_mol: 1,
            staged: false,
            workgroup: 256,
        }
    }
}

impl WaterSheetSettings {
    pub fn params(&self) -> WaterSheetParams {
        WaterSheetParams {
            molecules: self.molecules,
            rows: self.rows,
            cols: self.cols,
        }
    }
}

/// Deterministic per-index jitter in [-1, 1] (xorshift32; no floats in the
/// hashing so CPU and any future GPU seeder agree trivially).
pub fn jitter(i: u32, salt: u32) -> f32 {
    let mut x = i ^ (salt << 16) ^ 0x9E37_79B9;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    ((x & 0xFFFF) as f32 / 32768.0) - 1.0
}

/// Canonical deterministic seed: the sheet on its ideal lattice (spacing
/// `sheet_rest`, plane z = +2) and the molecules on a parallel grid (spacing
/// 1.4, plane z = −2) with H's at ±bond_rest/2 along x — each perturbed by a
/// fixed small jitter so the layout has to actually relax (a zero-jitter seed
/// would make a spring test vacuous).
pub fn seed_positions(params: &WaterSheetParams, s: &WaterSheetSettings) -> Vec<f32> {
    let n = params.n_nodes() as usize;
    let mut p = vec![0.0f32; 3 * n];
    let sb = params.sheet_base();

    // Molecules: O on the molecule grid, H at ±bond_rest/2.
    let w = params.mol_grid_w();
    for m in 0..params.molecules {
        let mr = m / w;
        let mc = m - w * mr;
        let o = (MOLECULE_STRIDE * m) as usize;
        let ox = mc as f32 * 1.4 + 0.25 * jitter(MOLECULE_STRIDE * m, 4);
        let oy = mr as f32 * 1.4 + 0.25 * jitter(MOLECULE_STRIDE * m, 5);
        let oz = -2.0 + 0.25 * jitter(MOLECULE_STRIDE * m, 6);
        p[3 * o] = ox;
        p[3 * o + 1] = oy;
        p[3 * o + 2] = oz;
        for (h, sign) in [(o + 1, 1.0f32), (o + 2, -1.0f32)] {
            p[3 * h] = ox + sign * s.bond_rest * 0.5 + 0.1 * jitter(h as u32, 7);
            p[3 * h + 1] = oy + 0.1 * jitter(h as u32, 8);
            p[3 * h + 2] = oz + 0.1 * jitter(h as u32, 9);
        }
    }

    // Sheet: ideal lattice + jitter.
    for r in 0..params.rows {
        for c in 0..params.cols {
            let j = sb + r * params.cols + c;
            let i = j as usize;
            p[3 * i] = c as f32 * s.sheet_rest + 0.25 * jitter(j, 1);
            p[3 * i + 1] = r as f32 * s.sheet_rest + 0.25 * jitter(j, 2);
            p[3 * i + 2] = 2.0 + 0.25 * jitter(j, 3);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_sheet_topology_counts_match_graph_nix() {
        let p = WaterSheetParams {
            molecules: 10,
            rows: 8,
            cols: 8,
        };
        let g = p.graph();
        assert_eq!(g.n_nodes, 3 * 10 + 64);
        // edges: 2*10 molecule bonds + 2*64 sheet bonds − 8 − 8 missing at the
        // right/bottom edges = 132; CSR stores each undirected edge twice.
        assert_eq!(*g.offsets.last().unwrap(), 2 * (2 * 10 + 2 * 64 - 8 - 8));
        p.validate_csr(&g).unwrap();
    }

    #[test]
    fn validate_rejects_wrong_shape() {
        let p = WaterSheetParams {
            molecules: 10,
            rows: 8,
            cols: 8,
        };
        let wrong = WaterSheetParams {
            molecules: 9,
            rows: 8,
            cols: 8,
        };
        let g = wrong.graph();
        assert!(p.validate_csr(&g).is_err());
    }

    fn assert_same_csr(a: &CsrGraph, b: &CsrGraph) {
        assert_eq!(a.n_nodes, b.n_nodes);
        assert_eq!(a.offsets, b.offsets);
        assert_eq!(a.neighbors, b.neighbors);
    }

    /// Unambiguous shapes: the inference must return the exact triple and the
    /// recovered params must reproduce the input CSR byte for byte.
    #[test]
    fn infer_recovers_exact_shape() {
        let cases = [
            (0u32, 0u32, 0u32),    // empty graph
            (0, 1, 1),             // single sheet cell
            (0, 1, 7),             // single row
            (5, 0, 0),             // molecules only, no sheet
            (7, 3, 4),             // ragged
            (20_000, 256, 256),    // 125,536 nodes / 170,304 bonds
            (3, 1, 5),             // single row with molecules
        ];
        for (m, r, c) in cases {
            let p = WaterSheetParams::new(m, r, c);
            let g = p.graph();
            let q = WaterSheetParams::infer_from_csr(&g)
                .unwrap_or_else(|e| panic!("infer({m}, {r}, {c}): {e}"));
            assert_eq!(q, p, "infer({m}, {r}, {c})");
            assert_same_csr(&q.graph(), &g);
        }
    }

    /// A 1-column sheet is a simple path — the SAME CSR as the 1-row sheet of
    /// the same length (its only bond is the down bond `s -> s+1`) — so `Nx1`
    /// canonicalises to the single-row form `(1, N)` and must still reproduce
    /// the graph exactly.
    #[test]
    fn infer_canonicalises_1_column_sheets() {
        for (m, r, c) in [(0u32, 7u32, 1u32), (3, 5, 1)] {
            let p = WaterSheetParams::new(m, r, c);
            let g = p.graph();
            let q = WaterSheetParams::infer_from_csr(&g)
                .unwrap_or_else(|e| panic!("infer({m}, {r}, {c}): {e}"));
            assert_eq!(q, WaterSheetParams::new(m, 1, r * c), "({m}, {r}, {c})");
            assert_same_csr(&q.graph(), &g);
        }
        // The ambiguity is a fact of the topology, not an inference bug.
        assert_same_csr(
            &WaterSheetParams::new(0, 7, 1).graph(),
            &WaterSheetParams::new(0, 1, 7).graph(),
        );
    }

    #[test]
    fn infer_rejects_non_water_sheet() {
        // 4-cycle 0-1-2-3-0: the base node's far neighbour is 3 away, giving
        // cols = 3, and 4 cells do not fill 3 columns.
        let cycle4 = CsrGraph {
            n_nodes: 4,
            offsets: vec![0, 2, 4, 6, 8],
            neighbors: vec![1, 3, 0, 2, 1, 3, 2, 0],
        };
        let e = WaterSheetParams::infer_from_csr(&cycle4).unwrap_err();
        assert!(e.contains("cannot infer shape"), "{e}");

        // A molecule–sheet bond: the scan stops before it (H2 has two
        // neighbours, so it is not a valid molecule tail), and the remainder
        // then fails the count check in validate_csr.
        let bonded = CsrGraph {
            n_nodes: 4,
            offsets: vec![0, 2, 3, 5, 6, 6],
            neighbors: vec![1, 2, 0, 0, 3, 2],
        };
        let e = WaterSheetParams::infer_from_csr(&bonded).unwrap_err();
        assert!(e.starts_with("water-sheet:"), "{e}");
    }
}
