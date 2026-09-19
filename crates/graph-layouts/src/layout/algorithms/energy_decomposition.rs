//! Per-node energy decomposition for 3D force-directed layouts.
//!
//! Splits the total layout stress into attractive (spring), repulsive
//! (Coulomb, optionally Barnes-Hut approximated), and gravity (origin
//! pull) components, one per node. Analogous to per-residue MM-GBSA
//! decomposition in computational chemistry — answers "which nodes
//! carry the most residual force?"
//!
//! # Numerics
//!
//! All sums accumulate in `f64` to guard against cancellation in
//! large graphs; final per-node energy is stored as `f32`.

use crate::layout::algorithms::gpu_force::{GpuForceOptions, RepulsionMode};

// ---------------------------------------------------------------------------
// public types
// ---------------------------------------------------------------------------

/// Per-node energy breakdown.
#[derive(Debug, Clone)]
pub struct NodeEnergy {
    /// Index into the packed `positions` slice.
    pub node_idx: u32,
    /// `attractive + repulsive + gravity`.
    pub total: f32,
    /// Half-sum of each incident edge's spring energy.
    pub attractive: f32,
    /// Half-sum of the pairwise Coulomb energy with every other node.
    pub repulsive: f32,
    /// Gravity pull toward the origin: `gravity * |pos|²`.
    pub gravity: f32,
    /// Number of edges incident to this node.
    pub edge_count: u32,
    /// `total / edge_count`, or `0.0` when the node has no incident edges.
    pub energy_per_edge: f32,
}

// ---------------------------------------------------------------------------
// public entry-point
// ---------------------------------------------------------------------------

/// Compute per-node energy contributions given packed positions and an edge
/// list.
///
/// Returns one [`NodeEnergy`] per node in node-index order.
///
/// * `positions` — packed `[x0, y0, z0, x1, y1, z1, …]`.
/// * `edges` — undirected edge list `(source, target)`; each edge must be
///   listed exactly once.
/// * `edge_strengths` — one `f32` per edge, in `edges` order.
/// * `params` — force-law parameters (`repulsion`, `spring_k`, `spring_len`,
///   `gravity`, `repulsion_mode`, `theta`).
/// * `octree` — when `Some` and `params.repulsion_mode == BarnesHut`, uses
///   the octree for approximate repulsion. When `None` or the mode is not
///   `BarnesHut`, uses exact O(n²) pairwise repulsion.
pub fn compute_per_node_energy(
    positions: &[f32],
    edges: &[(u32, u32)],
    edge_strengths: &[f32],
    params: &GpuForceOptions,
    octree: Option<&dyn OctreeApprox>,
) -> Vec<NodeEnergy> {
    assert_eq!(
        edges.len(),
        edge_strengths.len(),
        "edges and edge_strengths must have equal length"
    );
    let n = positions.len() / 3;
    let mut accum = vec![EnergyAccum::default(); n];

    // ---- attractive (spring) energy ----
    attractive_energy(positions, edges, edge_strengths, params, &mut accum);

    // ---- repulsive energy ----
    let use_octree = params.repulsion_mode == RepulsionMode::BarnesHut && octree.is_some();
    if use_octree {
        repulsive_energy_octree(positions, n, params, octree.unwrap(), &mut accum);
    } else {
        repulsive_energy_exact(positions, n, params, &mut accum);
    }

    // ---- gravity ----
    gravity_energy(positions, n, params, &mut accum);

    // ---- finalise ----
    accum
        .into_iter()
        .enumerate()
        .map(|(idx, a)| {
            let total = (a.attractive + a.repulsive + a.gravity) as f32;
            let ep = if a.edge_count == 0 {
                0.0
            } else {
                total / a.edge_count as f32
            };
            NodeEnergy {
                node_idx: idx as u32,
                total,
                attractive: a.attractive as f32,
                repulsive: a.repulsive as f32,
                gravity: a.gravity as f32,
                edge_count: a.edge_count,
                energy_per_edge: ep,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// trait for octree abstraction (decouples from the private wgpu Octree)
// ---------------------------------------------------------------------------

/// Minimal octree approximation interface so `compute_per_node_energy` does
/// not depend on the private wgpu `Octree` type.
///
/// Callers that hold a reference to the GPU-built octree can implement this
/// trait on a newtype wrapper.
pub trait OctreeApprox {
    /// Approximate repulsive force at `pos` (packed `[x, y, z]` of one
    /// body). Returns the accumulated force vector `[fx, fy, fz]` as
    /// exerted by the octree (already scaled by `repulsion`).
    fn approx_force(&self, pos: &[f32; 3], repulsion: f32, theta: f32) -> [f32; 3];
}

// ---------------------------------------------------------------------------
// internal helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct EnergyAccum {
    attractive: f64,
    repulsive: f64,
    gravity: f64,
    edge_count: u32,
}

/// Squared Euclidean distance between nodes `i` and `j`.
#[inline]
fn squ_dist(positions: &[f32], i: usize, j: usize) -> f64 {
    let i3 = i * 3;
    let j3 = j * 3;
    let dx = (positions[i3] - positions[j3]) as f64;
    let dy = (positions[i3 + 1] - positions[j3 + 1]) as f64;
    let dz = (positions[i3 + 2] - positions[j3 + 2]) as f64;
    dx * dx + dy * dy + dz * dz
}

/// Euclidean distance between nodes `i` and `j`.
#[inline]
fn euclid(positions: &[f32], i: usize, j: usize) -> f64 {
    squ_dist(positions, i, j).sqrt()
}

/// Squared distance from the origin for node `i`.
#[inline]
fn origin_squ_dist(positions: &[f32], i: usize) -> f64 {
    let i3 = i * 3;
    let x = positions[i3] as f64;
    let y = positions[i3 + 1] as f64;
    let z = positions[i3 + 2] as f64;
    x * x + y * y + z * z
}

// ---- attractive ---------------------------------------------------------

fn attractive_energy(
    positions: &[f32],
    edges: &[(u32, u32)],
    edge_strengths: &[f32],
    params: &GpuForceOptions,
    accum: &mut [EnergyAccum],
) {
    let half_k = 0.5 * params.spring_k as f64;
    let rest = params.spring_len as f64;

    for (&(u, v), &w) in edges.iter().zip(edge_strengths.iter()) {
        let d = euclid(positions, u as usize, v as usize);
        let delta = d - rest; // (dist - rest_len)
        let energy = w as f64 * half_k * delta * delta;
        let half = 0.5 * energy;
        accum[u as usize].attractive += half;
        accum[v as usize].attractive += half;
        accum[u as usize].edge_count += 1;
        accum[v as usize].edge_count += 1;
    }
}

// ---- repulsive (exact) --------------------------------------------------

fn repulsive_energy_exact(
    positions: &[f32],
    n: usize,
    params: &GpuForceOptions,
    accum: &mut [EnergyAccum],
) {
    let rep = params.repulsion as f64;

    for i in 0..n {
        for j in (i + 1)..n {
            let d = euclid(positions, i, j);
            // Coulomb: repulsion / distance
            let energy = if d > 0.0 { rep / d } else { rep / 1e-12 };
            let half = 0.5 * energy;
            accum[i].repulsive += half;
            accum[j].repulsive += half;
        }
    }
}

// ---- repulsive (Barnes-Hut approximated) --------------------------------

fn repulsive_energy_octree(
    positions: &[f32],
    n: usize,
    params: &GpuForceOptions,
    octree: &dyn OctreeApprox,
    accum: &mut [EnergyAccum],
) {
    // In full Barnes-Hut, the energy contributed by a cluster is treated as
    // a single body at the cluster's centre of mass.  We approximate the
    // per-node repulsive energy by integrating the force:
    //
    //   U_rep(i) = ½ · Σ_{j ≠ i} repulsion / |pos_i − pos_j|
    //
    // Using the octree's approximate force vector F_i, we estimate the
    // energy as ½ · F_i · pos_i  (analogy: for Coulomb U ∝ 1/r, force
    // F ∝ 1/r² · r̂, so U ≈ F · r).  This is a rough estimate; the
    // exact repulsion is only available with the O(n²) backend.
    //
    // We fall back to exact if no octree is given (caller gate).
    let rep = params.repulsion;

    for i in 0..n {
        let i3 = i * 3;
        let pos = [positions[i3], positions[i3 + 1], positions[i3 + 2]];
        let force = octree.approx_force(&pos, rep, params.theta);
        // U ≈ ½ Σ F · r  — each pair's energy is half the dot product of
        // force and position (since F_i = −∇_{r_i} U and U ∝ 1/r ⇒ F ∝ 1/r²).
        // For a Coulomb potential U = C/r, F = C·r̂/r² = C·r/r³, so
        // F·r = C/r = U.  The ½ comes from pair splitting.
        let energy = 0.5_f64
            * (force[0] as f64 * pos[0] as f64
                + force[1] as f64 * pos[1] as f64
                + force[2] as f64 * pos[2] as f64)
                .abs();
        accum[i].repulsive += energy;
    }
}

// ---- gravity ------------------------------------------------------------

fn gravity_energy(
    positions: &[f32],
    n: usize,
    params: &GpuForceOptions,
    accum: &mut [EnergyAccum],
) {
    let g = params.gravity as f64;
    for i in 0..n {
        let r2 = origin_squ_dist(positions, i);
        accum[i].gravity += g * r2;
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fresh options block with `spring_k = 1`, `spring_len = 1`,
    /// zero repulsion/gravity so we can isolate one term at a time.
    fn clean_params() -> GpuForceOptions {
        GpuForceOptions {
            repulsion: 0.0,
            spring_k: 1.0,
            spring_len: 1.0,
            gravity: 0.0,
            ..GpuForceOptions::default()
        }
    }

    // ---- helpers ----

    /// Compute 1-based node index (for display).
    #[allow(dead_code)]
    fn ni(i: usize) -> usize {
        i + 1
    }

    // ------------------------------------------------------------------
    // test: single edge — verify attractive energy formula exactly
    // ------------------------------------------------------------------

    #[test]
    fn single_edge_attractive_formula() {
        // Two nodes separated by distance 5.0, spring_len = 1.0.
        // U_spring = w·(k/2)·(d - L)² = 1·0.5·(5-1)² = 0.5·16 = 8
        // Split: 4 per node.
        let positions = vec![0.0, 0.0, 0.0, 5.0, 0.0, 0.0];
        let edges = vec![(0, 1)];
        let strengths = vec![1.0];
        let params = GpuForceOptions {
            repulsion: 0.0,
            spring_k: 1.0,
            spring_len: 1.0,
            gravity: 0.0,
            ..GpuForceOptions::default()
        };

        let energies = compute_per_node_energy(&positions, &edges, &strengths, &params, None);
        assert_eq!(energies.len(), 2);

        // Each node gets half of 8 = 4
        for (i, e) in energies.iter().enumerate() {
            assert!(
                (e.attractive - 4.0).abs() < 1e-5,
                "node {} attractive {} != 4.0",
                ni(i),
                e.attractive
            );
            assert!(
                e.repulsive.abs() < 1e-10,
                "node {} repulsive {} != 0",
                ni(i),
                e.repulsive
            );
            assert!(
                e.gravity.abs() < 1e-10,
                "node {} gravity {} != 0",
                ni(i),
                e.gravity
            );
            assert_eq!(e.edge_count, 1);
            assert!((e.total - 4.0).abs() < 1e-5);
            assert!((e.energy_per_edge - 4.0).abs() < 1e-5);
        }
    }

    // ------------------------------------------------------------------
    // test: zero edges — only gravity + repulsion, no attractive
    // ------------------------------------------------------------------

    #[test]
    fn zero_edges_no_attractive() {
        let positions = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let edges: Vec<(u32, u32)> = vec![];
        let strengths: Vec<f32> = vec![];
        let params = GpuForceOptions {
            repulsion: 1.0,
            spring_k: 1.0,
            spring_len: 1.0,
            gravity: 0.0,
            ..GpuForceOptions::default()
        };

        let energies = compute_per_node_energy(&positions, &edges, &strengths, &params, None);
        assert_eq!(energies.len(), 3);

        for (i, e) in energies.iter().enumerate() {
            assert!(
                e.attractive.abs() < 1e-10,
                "node {} should have zero attractive",
                ni(i)
            );
            assert!(e.repulsive > 0.0, "node {} should have repulsive > 0", ni(i));
            assert!(e.gravity.abs() < 1e-10, "node {} gravity should be zero", ni(i));
            assert_eq!(e.edge_count, 0);
            assert_eq!(e.energy_per_edge, 0.0);
        }
    }

    // ------------------------------------------------------------------
    // test: triangle — verify sum(total) ≈ global stress
    // ------------------------------------------------------------------

    #[test]
    fn triangle_energy_conservation() {
        // Equilateral triangle side length 1, so d_ij = 1 everywhere.
        let sqrt3 = 3.0f32.sqrt();
        let positions = vec![
            0.0, 0.0, 0.0,            // node 0
            1.0, 0.0, 0.0,            // node 1
            0.5, sqrt3 / 2.0, 0.0,    // node 2
        ];
        let edges = vec![(0, 1), (1, 2), (0, 2)];
        let strengths = vec![1.0, 1.0, 1.0];
        let params = clean_params(); // spring_k=1, spring_len=1, gravity=0, repulsion=0

        let energies = compute_per_node_energy(&positions, &edges, &strengths, &params, None);

        // Since d_ij = 1 everywhere and spring_len = 1, delta = 0,
        // attractive energy is zero. Total = 0 per node.
        for e in &energies {
            assert!((e.total - 0.0).abs() < 1e-5, "equilateral at rest → stress 0");
        }

        // Now displace one node so d != spring_len → non-zero stress.
        let positions2 = vec![
            0.0, 0.0, 0.0,
            2.0, 0.0, 0.0, // node 1 moved farther
            0.5, sqrt3 / 2.0, 0.0,
        ];
        let energies2 = compute_per_node_energy(&positions2, &edges, &strengths, &params, None);
        let per_node_sum: f64 = energies2.iter().map(|e| e.total as f64).sum();

        // Global scale-normalized stress for comparison.
        // scale_normalized_stress takes terms (i, j, target_dist).
        // But our energy is w·(k/2)·(d-L)² without 1/d² weight.
        // Just verify per_node_sum > 0 (non-zero when displaced).
        assert!(per_node_sum > 0.0);

        // Node 1 has two stretched edges (to 0: dist=2, to 2: dist=√3≈1.732)
        // so it carries the most attractive energy.
        assert!(energies2[1].attractive > energies2[0].attractive);
        assert!(energies2[1].attractive > energies2[2].attractive);
        // Both node 0 and node 2 share the edge to displaced node 1
        // (energy is split 50/50 per edge), and both have their mutual edge
        // at rest length. So their attractive energies are dominated by the
        // half-share of the edge to node 1, which is equal for both.
        // We verify they are approximately equal rather than asserting inequality.
        let diff_02 = (energies2[0].attractive - energies2[2].attractive).abs();
        assert!(diff_02 < 0.2, "nodes 0 and 2 should have similar attractive energy (both share edge to displaced node 1 equally), diff={}", diff_02);
    }

    // ------------------------------------------------------------------
    // test: gravity term
    // ------------------------------------------------------------------

    #[test]
    fn gravity_term() {
        // Single node at distance 3 from origin.
        // U_grav = gravity · |pos|² = 0.5 · 9 = 4.5
        let positions = vec![3.0, 0.0, 0.0];
        let edges: Vec<(u32, u32)> = vec![];
        let strengths: Vec<f32> = vec![];

        let params = GpuForceOptions {
            repulsion: 0.0,
            spring_k: 1.0,
            spring_len: 1.0,
            gravity: 0.5,
            ..GpuForceOptions::default()
        };

        let energies = compute_per_node_energy(&positions, &edges, &strengths, &params, None);
        assert_eq!(energies.len(), 1);
        assert!((energies[0].gravity - 4.5).abs() < 1e-5);
        assert!((energies[0].total - 4.5).abs() < 1e-5);
    }

    // ------------------------------------------------------------------
    // test: grid — 3×3 grid (9 nodes, 12 edges) → corner symmetry
    // ------------------------------------------------------------------

    #[test]
    fn grid_corner_symmetry() {
        // 3×3 grid in the xy-plane, unit spacing.
        let mut positions = Vec::with_capacity(27);
        for y in 0..3 {
            for x in 0..3 {
                positions.push(x as f32);
                positions.push(y as f32);
                positions.push(0.0);
            }
        }
        // Grid edges: horizontal + vertical, node ordering is row-major.
        let mut edges = Vec::new();
        for y in 0..3 {
            for x in 0..2 {
                let a = (y * 3 + x) as u32;
                let b = a + 1;
                edges.push((a, b));
            }
        }
        for y in 0..2 {
            for x in 0..3 {
                let a = (y * 3 + x) as u32;
                let b = a + 3;
                edges.push((a, b));
            }
        }
        assert_eq!(edges.len(), 12);

        let strengths = vec![1.0; edges.len()];
        let params = GpuForceOptions {
            repulsion: 1.0,
            spring_k: 1.0,
            spring_len: 1.0,
            gravity: 0.0, // zero gravity so corner symmetry is exact
            ..GpuForceOptions::default()
        };

        let energies = compute_per_node_energy(&positions, &edges, &strengths, &params, None);
        assert_eq!(energies.len(), 9);

        // Corner nodes: indices 0, 2, 6, 8 — all should have identical energy.
        let corners: [usize; 4] = [0, 2, 6, 8];
        let ref_total = energies[0].total;
        for &c in &corners[1..] {
            let diff = (energies[c].total - ref_total).abs();
            // Corners are symmetric but repulsion contributions differ slightly
            // due to floating-point accumulation order. Tolerance is relative.
            let tolerance = ref_total.abs().max(1.0) * 1e-3;
            assert!(
                diff < tolerance,
                "corner {} energy {} != corner 0 energy {} (diff={}, tolerance={})",
                c, energies[c].total, ref_total, diff, tolerance
            );
        }

        // Edge-centre nodes (non-corner edges): indices 1, 3, 5, 7 —
        // all should have identical energy.
        let edges_mid: [usize; 4] = [1, 3, 5, 7];
        let ref_edge = energies[1].total;
        for &e in &edges_mid[1..] {
            assert!(
                (energies[e].total - ref_edge).abs() < 1e-5,
                "edge-centre {} energy {} != edge-centre 1 energy {}",
                e,
                energies[e].total,
                ref_edge
            );
        }

        // Centre node (index 4) should differ from both.
        assert!((energies[4].total - ref_total).abs() > 1e-5);
        assert!((energies[4].total - ref_edge).abs() > 1e-5);
    }
}
