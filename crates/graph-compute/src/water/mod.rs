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
}
