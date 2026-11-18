//! Deterministic CPU reference for the water-sheet layout.
//!
//! SoA position storage (`x`/`y`/`z` as separate `Vec<f32>`, sized to the
//! waterSheet graph) plus a pure-f32 integrator that evaluates the SAME
//! fixed-order force sums as the WGSL kernels. It is the oracle the GPU tests
//! compare against and the deterministic path for small-case geometry tests.
//!
//! Two-phase per step (forces from OLD positions for every node, then
//! integrate), matching the GPU ping-pong: no node ever observes a neighbour's
//! already-updated position.

use super::{WaterSheetParams, WaterSheetSettings, MOLECULE_STRIDE};
use crate::sim::CsrGraph;

/// SoA positions: one `Vec<f32>` per axis.
#[derive(Clone, Debug, PartialEq)]
pub struct Positions {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub z: Vec<f32>,
}

impl Positions {
    pub fn zeroed(n: usize) -> Self {
        Self {
            x: vec![0.0; n],
            y: vec![0.0; n],
            z: vec![0.0; n],
        }
    }

    pub fn len(&self) -> usize {
        self.x.len()
    }

    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    /// Split interleaved `x,y,z,x,y,z,…` into SoA.
    pub fn from_interleaved(v: &[f32]) -> Self {
        assert_eq!(v.len() % 3, 0, "interleaved positions must be 3*n long");
        let n = v.len() / 3;
        let mut p = Self::zeroed(n);
        for i in 0..n {
            p.x[i] = v[3 * i];
            p.y[i] = v[3 * i + 1];
            p.z[i] = v[3 * i + 2];
        }
        p
    }

    /// Re-interleave for the `LayoutEngine` wire contract.
    pub fn to_interleaved(&self) -> Vec<f32> {
        let n = self.len();
        let mut v = Vec::with_capacity(3 * n);
        for i in 0..n {
            v.push(self.x[i]);
            v.push(self.y[i]);
            v.push(self.z[i]);
        }
        v
    }
}

/// SoA velocities (same shape as [`Positions`]).
pub type Velocities = Positions;

/// Add the windowed soft repulsion from node `j` onto `f` (force on `i`).
#[inline]
fn add_repulsion(
    f: &mut [f32; 3],
    pos: &Positions,
    i: usize,
    j: usize,
    rcut: f32,
    k_rep: f32,
) {
    let dx = pos.x[i] - pos.x[j];
    let dy = pos.y[i] - pos.y[j];
    let dz = pos.z[i] - pos.z[j];
    let len = (dx * dx + dy * dy + dz * dz).sqrt();
    if len < rcut && len > 1e-9 {
        let t = 1.0 - len / rcut;
        let w = k_rep * (t * t) / len;
        f[0] += w * dx;
        f[1] += w * dy;
        f[2] += w * dz;
    }
}

/// Force on node `i` from the CURRENT positions (bond springs, then windowed
/// repulsion, in that fixed order).
fn node_force(
    i: usize,
    pos: &Positions,
    g: &CsrGraph,
    p: &WaterSheetParams,
    s: &WaterSheetSettings,
) -> [f32; 3] {
    let mut f = [0.0f32; 3];
    let sheet_base = p.sheet_base() as usize;

    // Bond springs along CSR edges. Rest length depends only on this node's
    // kind (every bond is O–H or sheet–sheet, so both ends agree).
    let rest = if i < sheet_base { s.bond_rest } else { s.sheet_rest };
    for k in g.offsets[i] as usize..g.offsets[i + 1] as usize {
        let j = g.neighbors[k] as usize;
        let dx = pos.x[j] - pos.x[i];
        let dy = pos.y[j] - pos.y[i];
        let dz = pos.z[j] - pos.z[i];
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        if len > 1e-9 {
            let t = s.k_bond * (len - rest) / len;
            f[0] += t * dx;
            f[1] += t * dy;
            f[2] += t * dz;
        }
    }

    if i >= sheet_base {
        // Sheet node (r, c): repel lattice nodes inside the Chebyshev window.
        let local = (i - sheet_base) as i64;
        let cols = p.cols as i64;
        let rows = p.rows as i64;
        let r = local / cols;
        let c = local - cols * r;
        let rad = s.r_sheet as i64;
        for dr in -rad..=rad {
            for dc in -rad..=rad {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let rr = r + dr;
                let cc = c + dc;
                if rr < 0 || cc < 0 || rr >= rows || cc >= cols {
                    continue;
                }
                let j = sheet_base + (rr * cols + cc) as usize;
                add_repulsion(&mut f, pos, i, j, s.rcut, s.k_rep);
            }
        }
    } else if i % MOLECULE_STRIDE as usize == 0 {
        // Oxygen of molecule m: repel other O's inside the molecule-grid window.
        let m = (i / MOLECULE_STRIDE as usize) as i64;
        let w = p.mol_grid_w() as i64;
        let mr = m / w;
        let mc = m - w * mr;
        let h = p.mol_grid_h() as i64;
        let n_mol = p.molecules as i64;
        let rad = s.r_mol as i64;
        for dr in -rad..=rad {
            for dc in -rad..=rad {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let rr = mr + dr;
                let cc = mc + dc;
                if rr < 0 || cc < 0 || rr >= h || cc >= w {
                    continue;
                }
                let jm = rr * w + cc;
                if jm >= n_mol {
                    continue;
                }
                add_repulsion(
                    &mut f,
                    pos,
                    i,
                    (jm * MOLECULE_STRIDE as i64) as usize,
                    s.rcut,
                    s.k_rep,
                );
            }
        }
    }
    f
}

/// Advance one step: forces for every node from the old positions, then
/// semi-implicit Euler `v = (v + dt·f)·damping; p += dt·v`.
pub fn step_cpu(
    pos: &mut Positions,
    vel: &mut Velocities,
    g: &CsrGraph,
    p: &WaterSheetParams,
    s: &WaterSheetSettings,
) {
    let n = pos.len();
    let mut fx = vec![0.0f32; n];
    let mut fy = vec![0.0f32; n];
    let mut fz = vec![0.0f32; n];
    for i in 0..n {
        let f = node_force(i, pos, g, p, s);
        fx[i] = f[0];
        fy[i] = f[1];
        fz[i] = f[2];
    }
    for i in 0..n {
        vel.x[i] = (vel.x[i] + s.dt * fx[i]) * s.damping;
        vel.y[i] = (vel.y[i] + s.dt * fy[i]) * s.damping;
        vel.z[i] = (vel.z[i] + s.dt * fz[i]) * s.damping;
        pos.x[i] += s.dt * vel.x[i];
        pos.y[i] += s.dt * vel.y[i];
        pos.z[i] += s.dt * vel.z[i];
    }
}

/// Run `steps` CPU steps from the canonical seed; returns final SoA positions.
pub fn run_cpu(s: &WaterSheetSettings, steps: usize) -> Positions {
    let p = s.params();
    let g = p.graph();
    let mut pos = Positions::from_interleaved(&super::seed_positions(&p, s));
    let mut vel = Velocities::zeroed(pos.len());
    for _ in 0..steps {
        step_cpu(&mut pos, &mut vel, &g, &p, s);
    }
    pos
}

/// Worst-case deviations of a layout from the target geometry; used by both
/// the CPU and GPU tests so they assert the same thing.
#[derive(Clone, Copy, Debug)]
pub struct GeometryReport {
    /// max |dist(lattice neighbours) − sheet_rest| over all right/down bonds.
    pub max_sheet_spacing_err: f32,
    /// max |dist(O,H) − bond_rest| over every molecule's two O–H bonds.
    pub max_oh_err: f32,
    /// max |pos − ideal grid position| of the sheet, after removing the sheet's
    /// centroid (the sheet is free to translate).
    pub max_lattice_offset: f32,
}

/// Measure [`GeometryReport`] for SoA positions of the waterSheet in `s`.
pub fn geometry_report(pos: &Positions, s: &WaterSheetSettings) -> GeometryReport {
    let p = s.params();
    let sb = p.sheet_base() as usize;
    let d = |a: usize, b: usize| -> f32 {
        let dx = pos.x[a] - pos.x[b];
        let dy = pos.y[a] - pos.y[b];
        let dz = pos.z[a] - pos.z[b];
        (dx * dx + dy * dy + dz * dz).sqrt()
    };
    let mut max_oh = 0.0f32;
    for m in 0..p.molecules as usize {
        let o = MOLECULE_STRIDE as usize * m;
        max_oh = max_oh.max((d(o, o + 1) - s.bond_rest).abs());
        max_oh = max_oh.max((d(o, o + 2) - s.bond_rest).abs());
    }
    let (rows, cols) = (p.rows as usize, p.cols as usize);
    let mut max_sp = 0.0f32;
    for r in 0..rows {
        for c in 0..cols {
            let i = sb + r * cols + c;
            if c + 1 < cols {
                max_sp = max_sp.max((d(i, i + 1) - s.sheet_rest).abs());
            }
            if r + 1 < rows {
                max_sp = max_sp.max((d(i, i + cols) - s.sheet_rest).abs());
            }
        }
    }
    // Offset from the ideal lattice, modulo a rigid translation (centroid).
    let n = (rows * cols) as f32;
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    for r in 0..rows {
        for c in 0..cols {
            let i = sb + r * cols + c;
            cx += pos.x[i] - c as f32 * s.sheet_rest;
            cy += pos.y[i] - r as f32 * s.sheet_rest;
        }
    }
    let (cx, cy) = (cx / n, cy / n);
    let mut max_off = 0.0f32;
    for r in 0..rows {
        for c in 0..cols {
            let i = sb + r * cols + c;
            let ex = pos.x[i] - c as f32 * s.sheet_rest - cx;
            let ey = pos.y[i] - r as f32 * s.sheet_rest - cy;
            max_off = max_off.max((ex * ex + ey * ey).sqrt());
        }
    }
    GeometryReport {
        max_sheet_spacing_err: max_sp,
        max_oh_err: max_oh,
        max_lattice_offset: max_off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 molecules + 8×8 sheet (94 nodes), the canonical small case.
    pub(crate) fn small() -> WaterSheetSettings {
        WaterSheetSettings {
            molecules: 10,
            rows: 8,
            cols: 8,
            ..WaterSheetSettings::default()
        }
    }

    const STEPS: usize = 300;
    const SPACING_TOL: f32 = 0.15;
    const OH_TOL: f32 = 0.05;
    const OFFSET_TOL: f32 = 0.5;

    #[test]
    fn seed_is_not_already_solved() {
        // Guards the test below against being vacuous: the jittered seed must
        // violate the tolerances it asserts after relaxation.
        let s = small();
        let p = s.params();
        let seed = Positions::from_interleaved(&super::super::seed_positions(&p, &s));
        let r = geometry_report(&seed, &s);
        assert!(r.max_oh_err > OH_TOL || r.max_sheet_spacing_err > SPACING_TOL, "{r:?}");
    }

    fn assert_relaxes(s: &WaterSheetSettings) {
        let pos = run_cpu(s, STEPS);
        let r = geometry_report(&pos, s);
        eprintln!(
            "cpu geometry after {STEPS} steps (r_sheet={}, r_mol={}): {r:?}",
            s.r_sheet, s.r_mol
        );
        assert!(pos.x.iter().chain(&pos.y).chain(&pos.z).all(|v| v.is_finite()));
        assert!(r.max_sheet_spacing_err < SPACING_TOL, "sheet spacing: {r:?}");
        assert!(r.max_oh_err < OH_TOL, "O-H bond length: {r:?}");
        assert!(r.max_lattice_offset < OFFSET_TOL, "lattice offset: {r:?}");
    }

    /// Default (tuned) window.
    #[test]
    fn cpu_relaxes_to_lattice_and_bond_lengths() {
        assert_relaxes(&small());
    }

    /// The original wide window (r_sheet = 3, r_mol = 2): the tuned narrow
    /// default must not have changed what the layout converges to.
    #[test]
    fn cpu_relaxes_with_wide_window() {
        assert_relaxes(&WaterSheetSettings {
            r_sheet: 3,
            r_mol: 2,
            ..small()
        });
    }

    #[test]
    fn cpu_is_deterministic() {
        let s = small();
        assert_eq!(run_cpu(&s, 50), run_cpu(&s, 50));
    }
}
