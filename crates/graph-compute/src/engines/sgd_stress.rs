//! SGD stress-majorization layout engine (`"sgd-stress"`).
//!
//! Minimizes the stress objective
//!
//! ```text
//! stress(X) = Σ_{i<j} w_ij (‖x_i − x_j‖ − d_ij)²,   w_ij = d_ij^-2
//! ```
//!
//! where `d_ij` is the graph-theoretic shortest-path distance. Because stress
//! honors graph distances directly it untangles structure that ForceAtlas2
//! leaves clumped — a recognizably different visual result
//! (`docs/layout-algorithms.md` §3).
//!
//! Two design choices, both straight from the literature:
//!
//! 1. **Stochastic gradient descent over node pairs** — Zheng, Pawar & Goodman,
//!    "Graph Drawing by Stochastic Gradient Descent" (`s_gd2`, arXiv:1710.04626).
//!    Rather than majorizing the full quadratic (Gansner, Koren & North, "Graph
//!    Drawing by Stress Majorization", GKN04) we sample one pair `(i,j)` per
//!    sub-step and move *both* endpoints along the gradient by an annealed step
//!    size `μ = min(1, w_ij · η)`. SGD reaches lower stress, faster, and is far
//!    less sensitive to initialization than monotonic majorization — at the cost
//!    of the monotonic-decrease guarantee (it's stochastic; it reaches local
//!    minima).
//!
//! 2. **PIVOT / sparse stress** — Ortmann, Klimenta & Brandes, "A Sparse Stress
//!    Model". Computing the full O(n²) distance matrix (all-pairs shortest paths)
//!    is infeasible past a few thousand nodes, so we only compute distances
//!    against `k` landmark **pivots** (one BFS per pivot on the CSR graph →
//!    O(k·(n+m))). Every node then optimizes its `k` pivot pairs each step →
//!    O(k·n) work per epoch instead of O(n²). Pivots are chosen by
//!    max/min farthest-point sampling (the maxmin heuristic from the sparse-stress
//!    paper) so they spread across the graph.
//!
//! This is a **CPU** engine: `step` integrates one SGD epoch on the host and
//! returns the new interleaved positions. It never touches the GPU, so it runs
//! anywhere and is a valid fallback target. A WGSL port (independent pairs are
//! trivially parallel — see `docs/layout-algorithms.md` §3, "GPU/shard ★★★") is
//! a follow-up; see the engine's `todo` in the integration notes.
//!
//! References:
//! - [GKN04] Gansner, Koren, North, "Graph Drawing by Stress Majorization."
//!   <https://graphviz.org/documentation/GKN04.pdf>
//! - [s_gd2] Zheng, Pawar, Goodman, "Graph Drawing by Stochastic Gradient
//!   Descent." <https://arxiv.org/abs/1710.04626>
//! - [sparse-stress] Ortmann, Klimenta, Brandes, "A Sparse Stress Model."

use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};
use crate::sim::CsrGraph;

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "sgd-stress";

/// Tunables for the SGD stress solver. Serde-roundtrippable so they ride on the
/// wire as `google.protobuf.Struct` (ADR-002).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SgdStressSettings {
    /// Number of landmark pivots `k` used for the sparse-stress approximation
    /// (Ortmann et al.). Distances are computed by one BFS per pivot → O(k·(n+m))
    /// precompute, and each node optimizes its `k` pivot pairs per epoch → O(k·n)
    /// per step. Larger `k` = higher fidelity, more cost. Clamped to `n_nodes`.
    pub n_pivots: u32,
    /// Number of SGD pair-sweeps performed per `step` call (one "epoch" =
    /// `n_nodes` pivot-pair updates per sweep). Keeps each tick's visible motion
    /// modest while letting the annealing schedule advance.
    pub sweeps_per_step: u32,
    /// Initial learning rate `η_max`. The s_gd2 schedule anneals `η`
    /// exponentially from `η_max` down to `η_min` over `n_anneal_steps`.
    pub eta_max: f32,
    /// Final learning rate `η_min`.
    pub eta_min: f32,
    /// Number of SGD sweeps over which `η` anneals from `eta_max` to `eta_min`.
    /// After this the rate is pinned at `eta_min` (fine-tuning regime).
    pub n_anneal_steps: u32,
    /// PRNG seed for pair sampling + pivot selection — fixed for reproducible
    /// layouts across runs.
    pub seed: u64,
}

impl Default for SgdStressSettings {
    fn default() -> Self {
        // 50 pivots is the value the sparse-stress paper reports as a good
        // accuracy/cost tradeoff for mid-size graphs; the schedule mirrors the
        // s_gd2 reference (eta_max ≈ max d², eta_min small, ~15 anneal sweeps).
        Self {
            n_pivots: 50,
            sweeps_per_step: 1,
            eta_max: 1.0,
            eta_min: 0.01,
            n_anneal_steps: 30,
            seed: 0x5EED_C0DE_1234_5678,
        }
    }
}

/// SplitMix64 — a tiny, allocation-free PRNG used for pivot selection and pair
/// sampling so this engine needs no `rand` dependency. (Vigna, "Further
/// scramblings of Marsaglia's xorshift generators".) Deterministic given `seed`.
/// Shared with the GPU port ([`super::sgd_stress_gpu`]) for identical pivots.
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `usize` in `[0, n)` (n > 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
}

/// Per-pivot distance row: `dist[v]` is the BFS distance from this pivot to node
/// `v`, or `u32::MAX` if `v` is unreachable.
pub(crate) struct PivotRow {
    pub(crate) pivot: u32,
    pub(crate) dist: Vec<u32>,
}

/// Choose `k = clamp(n_pivots, 1..=n)` landmark pivots via maxmin
/// farthest-point sampling (Ortmann et al.): random first pivot, then
/// iteratively the node maximizing its minimum distance to the chosen set.
/// Each returned row carries the pivot's full BFS distance vector.
///
/// `rng` is advanced in place so the CPU engine can keep using it for pair
/// sampling afterward (preserving its exact stochastic sequence); the GPU port
/// passes a throwaway rng. Shared between both engines so they pick identical
/// pivots for a given seed.
pub(crate) fn select_pivots(g: &CsrGraph, n_pivots: u32, rng: &mut SplitMix64) -> Vec<PivotRow> {
    let n = g.n_nodes as usize;
    let k = (n_pivots as usize).clamp(if n == 0 { 0 } else { 1 }, n);
    let mut pivots: Vec<PivotRow> = Vec::with_capacity(k);
    if n == 0 || k == 0 {
        return pivots;
    }

    let mut min_dist_to_pivots = vec![u32::MAX; n];
    let mut next_pivot = rng.below(n) as u32;
    for _ in 0..k {
        let dist = bfs_distances(g, next_pivot);
        let mut best_node = next_pivot;
        let mut best_dist = 0u32;
        for v in 0..n {
            let d = dist[v];
            if d != u32::MAX && d < min_dist_to_pivots[v] {
                min_dist_to_pivots[v] = d;
            }
            let cover = min_dist_to_pivots[v];
            if cover != u32::MAX && cover > best_dist {
                best_dist = cover;
                best_node = v as u32;
            }
        }
        pivots.push(PivotRow { pivot: next_pivot, dist });
        next_pivot = best_node;
    }
    pivots
}

/// Initialized solver state, built once at `init`.
struct State {
    n: usize,
    /// Interleaved x,y,z positions, length `3 * n`. Mutated in place each step.
    positions: Vec<f32>,
    /// One BFS distance row per pivot (Ortmann sparse stress).
    pivots: Vec<PivotRow>,
    rng: SplitMix64,
    /// Global SGD sweep counter — drives the annealing schedule across steps.
    sweep: u64,
}

/// SGD stress engine. Uninitialized until [`LayoutEngine::init`].
pub struct SgdStressEngine {
    descriptor: LayoutDescriptor,
    settings: SgdStressSettings,
    state: Option<State>,
}

impl SgdStressEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: SgdStressSettings::default(),
            state: None,
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "SGD stress (s_gd2, pivot)",
            description: "Stress majorization by stochastic gradient descent over node pairs (Zheng/Pawar/Goodman s_gd2), using k landmark pivots (Ortmann sparse stress) for O(k·n) scaling. Honors shortest-path distances — untangles structure FA2 leaves clumped. CPU engine; GPU port is a follow-up.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }
}

impl Default for SgdStressEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for SgdStressEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: SgdStressSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode sgd-stress settings: {e}"))?;
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        _ctx: &mut EngineCtx,
        graph: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let g = graph.graph;
        let n = g.n_nodes as usize;
        if positions.len() != 3 * n {
            return Err(format!(
                "initial positions length {} != 3 * n_nodes {}",
                positions.len(),
                3 * n
            ));
        }

        // Maxmin farthest-point pivot selection (shared with the GPU port). The
        // rng is advanced past selection and then reused for pair sampling, so
        // this engine's stochastic sequence is exactly as before the refactor.
        let mut rng = SplitMix64::new(self.settings.seed);
        let pivots = select_pivots(g, self.settings.n_pivots, &mut rng);

        self.state = Some(State {
            n,
            positions: positions.to_vec(),
            pivots,
            rng,
            sweep: 0,
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let st = self
            .state
            .as_mut()
            .expect("sgd-stress step called before successful init");

        let n = st.n;
        if n == 0 || st.pivots.is_empty() {
            return StepOutput::positions_only(st.positions.clone());
        }

        for _ in 0..settings.sweeps_per_step.max(1) {
            let eta = anneal_eta(
                st.sweep,
                settings.eta_max,
                settings.eta_min,
                settings.n_anneal_steps,
            );

            // One sweep = `n` pivot-pair updates. Each update samples a random
            // node `i` and a random pivot `p`, then applies the s_gd2 move to the
            // (i, pivot) pair. Sampling (rather than iterating in order) avoids
            // the directional bias a fixed traversal introduces.
            for _ in 0..n {
                let i = st.rng.below(n);
                let pidx = st.rng.below(st.pivots.len());
                let j = st.pivots[pidx].pivot as usize;
                if i == j {
                    continue;
                }
                let d_ij = st.pivots[pidx].dist[i];
                if d_ij == u32::MAX || d_ij == 0 {
                    // Unreachable (different component) or self — no target dist.
                    continue;
                }
                sgd_pair_update(&mut st.positions, i, j, d_ij as f32, eta);
            }

            st.sweep = st.sweep.wrapping_add(1);
        }

        StepOutput::positions_only(st.positions.clone())
    }
}

/// s_gd2 step-size schedule: anneal `η` exponentially from `eta_max` to
/// `eta_min` over `n_anneal_steps` sweeps, then hold at `eta_min`.
/// (Zheng/Pawar/Goodman use `η_t = η_max · (η_min/η_max)^(t/T)`.)
pub(crate) fn anneal_eta(sweep: u64, eta_max: f32, eta_min: f32, n_anneal_steps: u32) -> f32 {
    if n_anneal_steps == 0 {
        return eta_min;
    }
    let t = (sweep as f32) / (n_anneal_steps as f32);
    if t >= 1.0 {
        return eta_min;
    }
    let ratio = (eta_min / eta_max).max(1e-9);
    eta_max * ratio.powf(t)
}

/// Apply one s_gd2 stress-gradient update to a single pair `(i, j)` with target
/// distance `d_ij`. Weight `w_ij = d_ij^-2`. The capped step
/// `μ = min(1, w_ij · η)` moves each endpoint half the residual along their
/// connecting axis (Zheng/Pawar/Goodman, Eq. 4–6). Mutates `positions` in place.
fn sgd_pair_update(positions: &mut [f32], i: usize, j: usize, d_ij: f32, eta: f32) {
    let (xi, yi, zi) = (positions[3 * i], positions[3 * i + 1], positions[3 * i + 2]);
    let (xj, yj, zj) = (positions[3 * j], positions[3 * j + 1], positions[3 * j + 2]);

    let dx = xi - xj;
    let dy = yi - yj;
    let dz = zi - zj;
    let mag = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);

    // w_ij = d_ij^-2; capped step μ = min(1, w·η).
    let w = 1.0 / (d_ij * d_ij);
    let mu = (w * eta).min(1.0);

    // Residual: how far the current Euclidean distance is from the target,
    // split symmetrically between the two endpoints.
    let r = 0.5 * mu * (mag - d_ij) / mag;
    let rx = r * dx;
    let ry = r * dy;
    let rz = r * dz;

    positions[3 * i] = xi - rx;
    positions[3 * i + 1] = yi - ry;
    positions[3 * i + 2] = zi - rz;
    positions[3 * j] = xj + rx;
    positions[3 * j + 1] = yj + ry;
    positions[3 * j + 2] = zj + rz;
}

/// Unweighted BFS from `source` over the CSR graph, returning hop distances.
/// `u32::MAX` marks nodes unreachable from `source` (different component).
/// O(n + m). This is the per-pivot shortest-path primitive of the sparse-stress
/// model (Ortmann et al.).
pub(crate) fn bfs_distances(graph: &CsrGraph, source: u32) -> Vec<u32> {
    let n = graph.n_nodes as usize;
    let mut dist = vec![u32::MAX; n];
    if n == 0 {
        return dist;
    }
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    dist[source as usize] = 0;
    queue.push_back(source);
    while let Some(v) = queue.pop_front() {
        let dv = dist[v as usize];
        let start = graph.offsets[v as usize] as usize;
        let end = graph.offsets[v as usize + 1] as usize;
        for e in start..end {
            let u = graph.neighbors[e];
            if dist[u as usize] == u32::MAX {
                dist[u as usize] = dv + 1;
                queue.push_back(u);
            }
        }
    }
    dist
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::EngineCtx;

    fn ring_positions(n: usize) -> Vec<f32> {
        let mut p = vec![0.0f32; 3 * n];
        for i in 0..n {
            let t = (i as f32) / (n.max(1) as f32) * std::f32::consts::TAU;
            p[3 * i] = t.cos();
            p[3 * i + 1] = t.sin();
        }
        p
    }

    #[test]
    fn bfs_path_distances() {
        let g = CsrGraph::path(5);
        let d = bfs_distances(&g, 0);
        assert_eq!(d, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn step_reduces_stress_on_path() {
        let g = CsrGraph::path(12);
        let positions = ring_positions(12);
        let mut engine = SgdStressEngine::new();
        let mut ctx = EngineCtx::cpu_only();
        let shard = CsrShard::whole(&g);
        engine.init(&mut ctx, &shard, &positions).expect("init");

        let stress_before = full_stress(&g, &positions);
        let mut out = positions.clone();
        for _ in 0..40 {
            out = engine.step(&mut ctx).positions;
        }
        let stress_after = full_stress(&g, &out);
        assert!(
            stress_after < stress_before,
            "stress should decrease: before={stress_before} after={stress_after}"
        );
        assert_eq!(out.len(), positions.len());
    }

    #[test]
    fn handles_empty_and_singleton() {
        for n in [0u32, 1] {
            let g = CsrGraph::path(n);
            let positions = ring_positions(n as usize);
            let mut engine = SgdStressEngine::new();
            let mut ctx = EngineCtx::cpu_only();
            let shard = CsrShard::whole(&g);
            engine.init(&mut ctx, &shard, &positions).expect("init");
            let out = engine.step(&mut ctx).positions;
            assert_eq!(out.len(), positions.len());
        }
    }

    /// Reference full O(n²) stress for verification only (tests use small n).
    fn full_stress(g: &CsrGraph, pos: &[f32]) -> f32 {
        let n = g.n_nodes as usize;
        let mut total = 0.0f64;
        for i in 0..n {
            let d = bfs_distances(g, i as u32);
            for j in (i + 1)..n {
                let dij = d[j];
                if dij == u32::MAX || dij == 0 {
                    continue;
                }
                let dij = dij as f32;
                let dx = pos[3 * i] - pos[3 * j];
                let dy = pos[3 * i + 1] - pos[3 * j + 1];
                let dz = pos[3 * i + 2] - pos[3 * j + 2];
                let mag = (dx * dx + dy * dy + dz * dz).sqrt();
                let w = 1.0 / (dij * dij);
                let r = mag - dij;
                total += (w * r * r) as f64;
            }
        }
        total as f32
    }
}
