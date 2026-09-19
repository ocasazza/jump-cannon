//! Per-node layout-stability analysis.
//!
//! Runs N short perturbed force-directed simulations and computes per-node
//! positional variance and drift. Analogous to WaterMap's hydration-site
//! thermodynamics: variance ↦ ΔG_hyd — high variance means the node is easily
//! dislodged (shallow local minimum); low variance means the node stays put
//! (deep well).
//!
//! Uses a simplified Spring-Electrical force model on the CPU. This is
//! deliberately independent of the GPU backend so stability analysis can
//! run in any environment (native test, CI, WASM).

use crate::layout::algorithms::gpu_force::GpuForceOptions;
use rand::Rng;
use rand::rngs::StdRng;
use rand::SeedableRng;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Per-node stability profile from perturbed re-layout.
///
/// Analogous to WaterMap's hydration site thermodynamics: variance ↔ ΔG_hyd.
#[derive(Clone, Debug, PartialEq)]
pub struct StabilityProfile {
    /// Per-node positional variance (σ² of final positions across perturbation runs).
    pub variance: Vec<f32>,
    /// Per-node drift: |mean(final_pos) - initial_pos|
    pub drift: Vec<f32>,
    /// Global stability: mean variance across all nodes.
    pub global_stability: f32,
    /// Number of perturbation runs used.
    pub perturbation_runs: u32,
    /// Steps per perturbation run.
    pub steps_per_run: u32,
}

/// Compute stability by running N short perturbed force-directed layouts.
///
/// * `positions` — flat `[x0,y0,z0, x1,y1,z1, …]` initial node positions.
/// * `edges` — index pairs `(source, target)`.
/// * `edge_strengths` — one weight per edge; use `&[1.0; edges.len()]` for
///   unweighted graphs.
/// * `params` — force-model parameters (uses `repulsion`, `spring_k`, `spring_len`,
///   `gravity`, `damping`, `dt` from the Spring-Electrical model).
/// * `perturbation_runs` — number of jittered re-layouts (default: 100).
/// * `steps_per_perturbation` — iterations per run (default: 50).
pub fn compute_stability(
    positions: &[f32],
    edges: &[(u32, u32)],
    edge_strengths: &[f32],
    params: &GpuForceOptions,
    perturbation_runs: u32,
    steps_per_perturbation: u32,
) -> StabilityProfile {
    let n_nodes = positions.len() / 3;
    assert!(
        n_nodes > 0,
        "compute_stability requires at least one node"
    );
    assert!(
        perturbation_runs > 0,
        "perturbation_runs must be positive"
    );
    assert!(
        steps_per_perturbation > 0,
        "steps_per_perturbation must be positive"
    );

    let jitter = params.spring_len * 0.1;

    // Accumulators: running sum and sum-of-squares for each coordinate axis
    // of each node, plus count (= perturbation_runs).
    let mut sum_x = vec![0.0f64; n_nodes];
    let mut sum_y = vec![0.0f64; n_nodes];
    let mut sum_z = vec![0.0f64; n_nodes];
    let mut sum_sq_x = vec![0.0f64; n_nodes];
    let mut sum_sq_y = vec![0.0f64; n_nodes];
    let mut sum_sq_z = vec![0.0f64; n_nodes];

    // Use a deterministic seed so stability results are reproducible.
    let mut rng = StdRng::seed_from_u64(0x57AB1E);

    let mut work = positions.to_vec();

    for _run in 0..perturbation_runs {
        // Copy initial positions and apply random jitter.
        work.copy_from_slice(positions);
        for i in 0..n_nodes {
            let base = i * 3;
            work[base] += (rng.gen::<f32>() * 2.0 - 1.0) * jitter;
            work[base + 1] += (rng.gen::<f32>() * 2.0 - 1.0) * jitter;
            work[base + 2] += (rng.gen::<f32>() * 2.0 - 1.0) * jitter;
        }

        // Run the force simulation.
        cpu_force_step_many(
            &mut work,
            edges,
            edge_strengths,
            params,
            steps_per_perturbation,
        );

        // Accumulate.
        for i in 0..n_nodes {
            let base = i * 3;
            let x = work[base] as f64;
            let y = work[base + 1] as f64;
            let z = work[base + 2] as f64;
            sum_x[i] += x;
            sum_y[i] += y;
            sum_z[i] += z;
            sum_sq_x[i] += x * x;
            sum_sq_y[i] += y * y;
            sum_sq_z[i] += z * z;
        }
    }

    let n = perturbation_runs as f64;

    let mut variance = Vec::with_capacity(n_nodes);
    let mut drift = Vec::with_capacity(n_nodes);
    let mut total_variance = 0.0f64;

    for i in 0..n_nodes {
        let base = i * 3;
        let init_x = positions[base] as f64;
        let init_y = positions[base + 1] as f64;
        let init_z = positions[base + 2] as f64;

        let mean_x = sum_x[i] / n;
        let mean_y = sum_y[i] / n;
        let mean_z = sum_z[i] / n;

        // Var(X) = E[X²] - E[X]²  (population variance over the N runs).
        let var_x = (sum_sq_x[i] / n) - (mean_x * mean_x);
        let var_y = (sum_sq_y[i] / n) - (mean_y * mean_y);
        let var_z = (sum_sq_z[i] / n) - (mean_z * mean_z);
        let var = (var_x + var_y + var_z) as f32;

        let dx = mean_x - init_x;
        let dy = mean_y - init_y;
        let dz = mean_z - init_z;
        let d = ((dx * dx + dy * dy + dz * dz).sqrt()) as f32;

        variance.push(var);
        drift.push(d);
        total_variance += var as f64;
    }

    let global_stability = (total_variance / n_nodes as f64) as f32;

    StabilityProfile {
        variance,
        drift,
        global_stability,
        perturbation_runs,
        steps_per_run: steps_per_perturbation,
    }
}

// ---------------------------------------------------------------------------
// CPU force step (Spring-Electrical model)
// ---------------------------------------------------------------------------

/// Run `steps` iterations of a simplified Spring-Electrical force model on
/// the CPU. This is intentionally independent of the GPU backend so stability
/// analysis can run anywhere.
///
/// Uses exact O(n²) repulsion (suitable for the small-to-medium graphs
/// stability analysis targets), spring attraction along edges, and a weak
/// gravity term. Semi-implicit Euler with velocity damping.
fn cpu_force_step_many(
    positions: &mut [f32],
    edges: &[(u32, u32)],
    edge_strengths: &[f32],
    params: &GpuForceOptions,
    steps: u32,
) {
    let n_nodes = positions.len() / 3;

    // Pre-compute edge strengths if provided, or default to 1.0.
    let default_strength = 1.0f32;

    // Velocity buffer.
    let mut vx = vec![0.0f32; n_nodes];
    let mut vy = vec![0.0f32; n_nodes];
    let mut vz = vec![0.0f32; n_nodes];

    let repulsion = params.repulsion;
    let spring_k = params.spring_k;
    let spring_len = params.spring_len;
    let gravity = params.gravity;
    let damping = params.damping;
    let dt = params.dt;

    for _step in 0..steps {
        let mut fx = vec![0.0f32; n_nodes];
        let mut fy = vec![0.0f32; n_nodes];
        let mut fz = vec![0.0f32; n_nodes];

        // --- Repulsion: exact O(n²) ---
        let repulsion_clip = if params.repulsion_radius > 0.0 {
            params.repulsion_radius
        } else {
            f32::MAX
        };

        for i in 0..n_nodes {
            let bi = i * 3;
            let xi = positions[bi];
            let yi = positions[bi + 1];
            let zi = positions[bi + 2];

            for j in (i + 1)..n_nodes {
                let bj = j * 3;
                let dx = xi - positions[bj];
                let dy = yi - positions[bj + 1];
                let dz = zi - positions[bj + 2];

                let dist_sq = dx * dx + dy * dy + dz * dz;
                if dist_sq < 1e-8 {
                    continue;
                }
                let dist = dist_sq.sqrt();
                if dist > repulsion_clip {
                    continue;
                }

                // Coulomb: F = repulsion * m_i * m_j / d²  (m_i = m_j = 1)
                let mag = repulsion / dist_sq;
                let fx_pair = mag * dx / dist;
                let fy_pair = mag * dy / dist;
                let fz_pair = mag * dz / dist;

                fx[i] += fx_pair;
                fy[i] += fy_pair;
                fz[i] += fz_pair;
                fx[j] -= fx_pair;
                fy[j] -= fy_pair;
                fz[j] -= fz_pair;
            }
        }

        // --- Attraction: Hooke springs along edges ---
        for (ei, &(s, t)) in edges.iter().enumerate() {
            let si = s as usize;
            let ti = t as usize;
            if si >= n_nodes || ti >= n_nodes {
                continue;
            }
            let bs = si * 3;
            let bt = ti * 3;
            let dx = positions[bt] - positions[bs];
            let dy = positions[bt + 1] - positions[bs + 1];
            let dz = positions[bt + 2] - positions[bs + 2];
            let dist = (dx * dx + dy * dy + dz * dz).sqrt();
            if dist < 1e-8 {
                continue;
            }

            let strength = edge_strengths
                .get(ei)
                .copied()
                .unwrap_or(default_strength);
            // F = spring_k * strength * (dist - spring_len)
            let mag = spring_k * strength * (dist - spring_len);
            let fx_edge = mag * dx / dist;
            let fy_edge = mag * dy / dist;
            let fz_edge = mag * dz / dist;

            fx[si] += fx_edge;
            fy[si] += fy_edge;
            fz[si] += fz_edge;
            fx[ti] -= fx_edge;
            fy[ti] -= fy_edge;
            fz[ti] -= fz_edge;
        }

        // --- Gravity: weak pull toward origin ---
        if gravity != 0.0 {
            for i in 0..n_nodes {
                let bi = i * 3;
                fx[i] -= gravity * positions[bi];
                fy[i] -= gravity * positions[bi + 1];
                fz[i] -= gravity * positions[bi + 2];
            }
        }

        // --- Semi-implicit Euler ---
        for i in 0..n_nodes {
            // v_new = v_old * damping + F * dt
            vx[i] = vx[i] * damping + fx[i] * dt;
            vy[i] = vy[i] * damping + fy[i] * dt;
            vz[i] = vz[i] * damping + fz[i] * dt;

            // x_new = x_old + v_new * dt
            let bi = i * 3;
            positions[bi] += vx[i] * dt;
            positions[bi + 1] += vy[i] * dt;
            positions[bi + 2] += vz[i] * dt;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_params() -> GpuForceOptions {
        GpuForceOptions {
            repulsion: 5000.0,
            spring_k: 0.01,
            spring_len: 100.0,
            gravity: 0.01,
            damping: 0.9,
            dt: 1.0,
            cursor_pos: [0.0; 3],
            cursor_radius: 0.0,
            cursor_strength: 0.0,
            steps_per_call: 1,
            repulsion_radius: 400.0,
            cooling_alpha: 1.0,
            cooling_floor: 0.55,
            energy_threshold: 0.0,
            repulsion_mode: crate::layout::algorithms::gpu_force::RepulsionMode::Exact,
            seed_mode: crate::layout::algorithms::gpu_force::SeedMode::Random,
            theta: 0.7,
            repulsion_samples: 8,
            force_model: crate::layout::algorithms::gpu_force::ForceModel::SpringElectrical,
            tfdp_alpha: 0.1,
            tfdp_beta: 8.0,
            tfdp_gamma: 2.0,
            tfdp_k: 3.0,
        }
    }

    /// Default params with gravity disabled (for single-node and other
    /// tests where gravity would confound the result).
    fn params_no_gravity() -> GpuForceOptions {
        let mut p = default_params();
        p.gravity = 0.0;
        p
    }

    /// Build a 3×3 grid: positions on a regular lattice, edges connecting
    /// adjacent neighbours (4-connected).
    fn make_grid_3x3() -> (Vec<f32>, Vec<(u32, u32)>, Vec<f32>) {
        let spacing = 100.0f32;
        let mut positions = Vec::with_capacity(9 * 3);
        for row in 0..3u32 {
            for col in 0..3u32 {
                positions.push(col as f32 * spacing);
                positions.push(row as f32 * spacing);
                positions.push(0.0);
            }
        }
        let mut edges = Vec::new();
        let mut strengths = Vec::new();
        for row in 0..3u32 {
            for col in 0..3u32 {
                let idx = row * 3 + col;
                if col < 2 {
                    edges.push((idx, row * 3 + col + 1));
                    strengths.push(1.0);
                }
                if row < 2 {
                    edges.push((idx, (row + 1) * 3 + col));
                    strengths.push(1.0);
                }
            }
        }
        (positions, edges, strengths)
    }

    /// Pre-converge the grid by running many force steps so the positions
    /// are at (or near) equilibrium.
    fn converge_grid(steps: u32) -> (Vec<f32>, Vec<(u32, u32)>, Vec<f32>) {
        let (mut positions, edges, strengths) = make_grid_3x3();
        let params = params_no_gravity(); // no gravity so grid stays near origin
        cpu_force_step_many(&mut positions, &edges, &strengths, &params, steps);
        (positions, edges, strengths)
    }

    #[test]
    fn grid_3x3_converged_is_stable() {
        // A grid pre-converged for 500 steps should be stable: perturbations
        // decay back to the equilibrium positions, giving low variance.
        let (positions, edges, strengths) = converge_grid(500);
        let params = params_no_gravity();
        let profile = compute_stability(
            &positions,
            &edges,
            &strengths,
            &params,
            30,
            200,
        );
        assert!(profile.global_stability < 100.0,
            "converged grid should be stable, got global_stability={}",
            profile.global_stability
        );
    }

    #[test]
    fn random_short_run_is_unstable() {
        // Very few steps from widely-scattered random positions: the layout
        // has barely moved from the jittered starts, so variance across runs
        // is dominated by the initial jitter spread (≈ spring_len * 0.1).
        let n = 9;
        let mut positions = Vec::with_capacity(n * 3);
        let mut rng = StdRng::seed_from_u64(42);
        for _ in 0..n {
            positions.push(rng.gen::<f32>() * 2000.0 - 1000.0);
            positions.push(rng.gen::<f32>() * 2000.0 - 1000.0);
            positions.push(0.0);
        }
        // Path graph: minimal structure so force sim can't collapse quickly.
        let mut edges = Vec::new();
        let mut strengths = Vec::new();
        for i in 0..(n - 1) {
            edges.push((i as u32, (i + 1) as u32));
            strengths.push(1.0);
        }
        let params = default_params();
        let profile = compute_stability(
            &positions,
            &edges,
            &strengths,
            &params,
            30,
            1, // just 1 step — not enough to converge
        );
        assert!(profile.global_stability > 50.0,
            "random positions after 1 step should be unstable, got global_stability={}",
            profile.global_stability
        );
    }

    #[test]
    fn variance_decreases_with_more_steps() {
        // Pre-converge so we start from a stable equilibrium. Then as
        // steps_per_perturbation increases, the perturbed layout has more
        // time to return to equilibrium, so variance decreases.
        let (positions, edges, strengths) = converge_grid(500);
        let params = params_no_gravity();

        let p10 = compute_stability(&positions, &edges, &strengths, &params, 30, 10);
        let p50 = compute_stability(&positions, &edges, &strengths, &params, 30, 50);
        let p200 = compute_stability(&positions, &edges, &strengths, &params, 30, 200);

        assert!(
            p10.global_stability > p50.global_stability,
            "variance should decrease with more steps: 10-step={} vs 50-step={}",
            p10.global_stability,
            p50.global_stability
        );
        assert!(
            p50.global_stability >= p200.global_stability,
            "variance should not increase with more steps: 50-step={} vs 200-step={}",
            p50.global_stability,
            p200.global_stability
        );
    }

    #[test]
    fn single_node_zero_variance() {
        // A single node with strong gravity pulling to origin: after enough
        // steps, every jittered start converges to the origin, so the
        // variance across runs is effectively zero.
        let positions = vec![10.0f32, 20.0, 30.0];
        let edges: Vec<(u32, u32)> = vec![];
        let strengths: Vec<f32> = vec![];
        let mut params = default_params();
        params.gravity = 1.0; // strong gravity to ensure fast convergence

        let profile = compute_stability(&positions, &edges, &strengths, &params, 10, 200);

        assert_eq!(profile.variance.len(), 1);
        assert!(profile.variance[0] < 0.01,
            "single node with gravity should converge to origin, got variance={}",
            profile.variance[0]
        );
        assert!(profile.global_stability < 0.01,
            "single node global_stability should be near zero, got {}",
            profile.global_stability
        );
    }
}
