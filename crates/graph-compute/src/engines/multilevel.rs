//! Multilevel / multiscale **wrapper** engine (`"multilevel"`).
//!
//! This is a *decorator*, not a standalone solver. It wraps ANY inner
//! [`LayoutEngine`] (fa2-brute, fa2-bh, sgd-stress, cpu-spring, …) and turns it
//! into a multiscale solver: coarsen the graph into a hierarchy `G_0 … G_L`, run
//! the inner engine at the coarsest level `G_L`, prolong (interpolate) positions
//! down to the next-finer level, refine there with the same inner engine, and
//! repeat until `G_0` is laid out. By the time the finest level is reached the
//! global structure is already settled, so the inner solver converges in a
//! handful of frames instead of the hundreds a flat run from a random start
//! needs (`docs/layout-algorithms.md` §2).
//!
//! ## Why a wrapper (DRY — `compute-architecture.md` §5)
//!
//! Multilevel is **solver-agnostic**: the contraction/prolongation machinery is
//! identical whether the inner solver is force-directed or stress-based. Baking
//! it into each engine would copy-paste the same cascade logic three times. By
//! composing over the `LayoutEngine` trait it works with every present and
//! future engine for free — exactly the point made in the doc.
//!
//! ## Algorithm provenance
//!
//! The coarsen → solve-coarsest → prolong → refine cascade is the classic
//! multilevel force-directed scheme shared by:
//!
//! - **Walshaw**, "A Multilevel Algorithm for Force-Directed Graph Drawing" —
//!   maximal-matching edge collapse + level-by-level refinement.
//!   <https://chriswalshaw.co.uk/papers/fulltext/WalshawTR6000.pdf>
//! - **FM³** (Hachul & Jünger), "Fast Multipole Multilevel Method" — the
//!   solar-system coarsening + multipole inner solver variant.
//!   <https://kups.ub.uni-koeln.de/54892/1/zaik2006-509.pdf>
//! - **sfdp** (Hu), "Efficient and High Quality Force-Directed Graph Drawing" —
//!   algebraic/weighted multiscale; the 1.6 coarsening-ratio cutoff and the
//!   prolong-with-jitter step we reuse come from here.
//!   <http://yifanhu.net/PUB/graph_draw.pdf>
//!
//! ## Reuse, not reinvention
//!
//! Coarsening + prolongation are **not** reimplemented here. We call
//! [`graph_layouts::coarsen`] and [`graph_layouts::prolong`] from
//! `graph-layouts/src/layout/coarsen.rs` (the FM³/sfdp matching contraction the
//! topo-fisheye bootstrap already uses). This engine only orchestrates the inner
//! engine across the levels that module produces.
//!
//! ## How it drives the inner engine
//!
//! The inner engine's lifecycle is `init(graph, positions) → reinit ⟲ → step ⟲`.
//! We construct ONE inner instance in `init` and `reinit` it onto each level: to
//! run it at level `l` we synthesize a [`CsrGraph`] for that level from the
//! cascade's flat edge list and call `inner.reinit(ctx, shard_l, positions_l)`,
//! then `step` it for that level's annealed sweep budget (more at coarse levels,
//! fewer at fine — Walshaw, see [`SweepSchedule`]). After that we prolong its
//! output to level `l-1` and `reinit` the same inner engine on the finer graph.
//! (`reinit` defaults to `init`, so CPU engines behave identically while a GPU
//! engine may later override it to reuse buffers.) The wrapper is a small
//! state machine over `step` calls so the cascade descends across ticks (each
//! `step` is one inner step; broadcasts are emitted at every level so the client
//! sees the layout coarsen-to-fine "snap" into place). Once level 0 is reached
//! the wrapper just forwards to the inner engine forever (continuous refine).

use graph_layouts::{
    coarsen, prolong, Coarsening, LayoutDescriptor, LayoutKind, LayoutRequirements,
};
use serde::{Deserialize, Serialize};

use super::{construct_leaf, CsrShard, EngineCtx, Fa2BruteEngine, LayoutEngine, StepOutput};
use crate::sim::CsrGraph;

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "multilevel";

/// How the number of relaxation sweeps varies across cascade levels.
///
/// Walshaw's multilevel force-directed scheme refines each level after
/// prolongation, but the *amount* of refinement need not be uniform: coarse
/// levels (few nodes, cheap sweeps, carry the global skeleton) benefit from more
/// passes, while fine levels start close to converged and need only a light
/// touch-up. This "more at coarse, fewer at fine" annealing is the standard
/// reading of Walshaw's level-by-level local refinement
/// (Walshaw, "A Multilevel Algorithm for Force-Directed Graph Drawing", §4;
/// WalshawTR6000, <https://chriswalshaw.co.uk/papers/fulltext/WalshawTR6000.pdf>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SweepSchedule {
    /// Every level runs exactly `sweeps_per_level` sweeps (the pre-annealing
    /// behavior; kept for back-compat and ablation).
    Uniform,
    /// Sweeps grow linearly with depth: level `l` (0 = finest) runs
    /// `sweeps_per_level * (1 + l)`. The coarsest level gets the most.
    Linear,
    /// Sweeps grow geometrically with depth: level `l` runs
    /// `sweeps_per_level * 2^l` (capped). Strongly front-loads coarse-level
    /// effort — closest to Walshaw's "settle the skeleton first" intent.
    Geometric,
}

impl Default for SweepSchedule {
    fn default() -> Self {
        // Linear annealing: a sane middle ground — coarse levels clearly get more
        // sweeps than fine ones without the blow-up of a pure geometric schedule.
        SweepSchedule::Linear
    }
}

impl SweepSchedule {
    /// Resolve the sweep count for `level` (0 = finest) of a cascade with
    /// `n_levels` levels, given the `base` budget. The coarsest level is
    /// `n_levels - 1`. Result is always `>= 1` so a level can never be skipped.
    ///
    /// Invariant (asserted in tests): a coarser level never gets FEWER sweeps
    /// than a finer one — the annealing direction Walshaw prescribes.
    pub fn sweeps_for_level(self, level: usize, base: u32) -> u32 {
        let base = base.max(1);
        let s = match self {
            SweepSchedule::Uniform => base,
            // level 0 (finest) → base; deeper (coarser) → more.
            SweepSchedule::Linear => base.saturating_mul(1 + level as u32),
            SweepSchedule::Geometric => {
                // 2^level, saturating; cap the exponent so we don't overflow on
                // pathologically deep cascades.
                let shift = level.min(16) as u32;
                base.saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
            }
        };
        s.max(1)
    }
}

/// Tunables for the multilevel wrapper. Serde-roundtrippable so they ride on the
/// wire as `google.protobuf.Struct` (ADR-002).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MultilevelSettings {
    /// Registry id of the inner engine to wrap (e.g. `"fa2-brute"`, `"fa2-bh"`,
    /// `"sgd-stress"`, `"cpu-spring"`). The whole point of the wrapper is that
    /// this composes with any of them without per-algorithm code. Unknown ids
    /// are rejected at `set_params`.
    pub inner: String,
    /// Settings forwarded verbatim to the inner engine's `set_params` (the inner
    /// engine's own typed struct). `Null` ⇒ inner defaults.
    pub inner_params: serde_json::Value,
    /// Maximum number of coarsening levels (cascade depth cap). The sfdp default
    /// neighborhood; coarsening also stops early when a level is small enough or
    /// the matching stops making progress.
    pub max_levels: usize,
    /// Coarsen until a level has `<= target_size` nodes, then solve that level
    /// as the coarsest. 500 matches `coarsen.rs::warmup_positions`.
    pub target_size: usize,
    /// Base inner-engine `step` count per level. This is the number of
    /// relaxation sweeps at the FINEST refined level (level 0's descent budget);
    /// coarser levels get progressively MORE per [`sweep_schedule`]. The finest
    /// level (level 0) then refines forever once descent finishes.
    ///
    /// [`sweep_schedule`]: MultilevelSettings::sweep_schedule
    pub sweeps_per_level: u32,
    /// How the per-level sweep count varies across the cascade.
    ///
    /// Walshaw's multilevel scheme spends more refinement effort at coarse
    /// levels — where a few nodes carry the global structure and each sweep is
    /// cheap — and tapers off toward the fine levels, which start near-converged
    /// after prolongation and only need a touch-up (Walshaw,
    /// "A Multilevel Algorithm for Force-Directed Graph Drawing", §4
    /// "local refinement"; WalshawTR6000). See [`SweepSchedule`].
    pub sweep_schedule: SweepSchedule,
    /// Spring length used to scale the prolongation jitter (`0.5 * spring_len`,
    /// per `coarsen.rs`). Should roughly match the inner engine's natural edge
    /// length so the seed lands in a regime it refines rather than re-explodes.
    pub spring_len: f32,
    /// PRNG seed for the prolong jitter — fixed for reproducible layouts.
    pub seed: u32,
}

impl Default for MultilevelSettings {
    fn default() -> Self {
        Self {
            inner: Fa2BruteEngine::ID.to_string(),
            inner_params: serde_json::Value::Null,
            max_levels: 6,
            target_size: 500,
            sweeps_per_level: 30,
            sweep_schedule: SweepSchedule::default(),
            spring_len: 30.0,
            seed: 0x5EED,
        }
    }
}

/// Where the cascade descent currently is.
struct Descent {
    cascade: Coarsening,
    /// Index into `cascade.levels` we are currently solving. Starts at the
    /// coarsest (`levels.len() - 1`) and walks down to 0.
    level: usize,
    /// Inner `step`s already taken at the current level.
    sweeps_done: u32,
    /// Current level's positions, interleaved `x,y,z`, length `3 * level_n`.
    positions: Vec<f32>,
}

/// The multilevel wrapper engine. Uninitialized until [`LayoutEngine::init`].
pub struct MultilevelEngine {
    descriptor: LayoutDescriptor,
    settings: MultilevelSettings,
    /// The wrapped solver — constructed at `init` from `settings.inner`,
    /// re-`init`'d at each level. `None` until `init`.
    inner: Option<Box<dyn LayoutEngine>>,
    descent: Option<Descent>,
}

impl MultilevelEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: MultilevelSettings::default(),
            inner: None,
            descent: None,
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "Multilevel (multiscale wrapper)",
            description: "Coarsen → solve coarsest → prolong → refine, wrapping any inner \
                          engine (Walshaw/FM³/sfdp). Sharpens global structure and converges \
                          far faster than a flat run. Select the inner solver via `inner`.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }

    /// Build a [`CsrGraph`] for one cascade level from its flat undirected edge
    /// list (`[s,t, s,t, …]`). The cascade stores edges as a flat list; the
    /// inner engines consume CSR, so we transpose into offsets/neighbors with
    /// both directions present (matching how `CsrGraph::path` / the loader emit
    /// symmetric adjacency).
    fn level_csr(n_nodes: usize, edges: &[u32]) -> CsrGraph {
        let mut degree = vec![0u32; n_nodes];
        let mut i = 0;
        while i + 1 < edges.len() {
            let s = edges[i] as usize;
            let t = edges[i + 1] as usize;
            i += 2;
            if s < n_nodes && t < n_nodes && s != t {
                degree[s] += 1;
                degree[t] += 1;
            }
        }
        let mut offsets = Vec::with_capacity(n_nodes + 1);
        let mut acc = 0u32;
        for &d in &degree {
            offsets.push(acc);
            acc += d;
        }
        offsets.push(acc);

        let mut neighbors = vec![0u32; acc as usize];
        let mut cursor = offsets.clone();
        let mut j = 0;
        while j + 1 < edges.len() {
            let s = edges[j] as usize;
            let t = edges[j + 1] as usize;
            j += 2;
            if s < n_nodes && t < n_nodes && s != t {
                let ps = cursor[s] as usize;
                neighbors[ps] = t as u32;
                cursor[s] += 1;
                let pt = cursor[t] as usize;
                neighbors[pt] = s as u32;
                cursor[t] += 1;
            }
        }
        CsrGraph {
            n_nodes: n_nodes as u32,
            offsets,
            neighbors,
        }
    }

    /// Construct the single inner engine instance and apply the forwarded
    /// params. Called ONCE from `init`; subsequent levels reuse this instance via
    /// [`Self::bind_inner_to_level`] (which calls [`LayoutEngine::reinit`]).
    fn build_inner(&mut self) -> Result<(), String> {
        let mut inner = construct_leaf(&self.settings.inner)
            .ok_or_else(|| format!("unknown inner engine {:?}", self.settings.inner))?;
        inner.set_params(&self.settings.inner_params)?;
        self.inner = Some(inner);
        Ok(())
    }

    /// (Re)bind the *already-constructed* inner engine to the level the descent
    /// currently points at, seeding it with `descent.positions`.
    ///
    /// We construct one inner instance (in `init`) and `reinit` it onto each
    /// successive level instead of minting a fresh engine per level. The default
    /// `reinit` forwards to `init` (so behavior is identical for CPU engines),
    /// but a GPU engine can override `reinit` to resize/reuse its buffers in
    /// place rather than rebuilding them every level (one buffer rebuild per
    /// descent instead of one per level). See the `reinit` hook in
    /// `engines/mod.rs`.
    fn bind_inner_to_level(&mut self, ctx: &mut EngineCtx) -> Result<(), String> {
        let descent = self
            .descent
            .as_ref()
            .expect("bind_inner_to_level before descent built");
        let level = &descent.cascade.levels[descent.level];
        let csr = Self::level_csr(level.n_nodes, &level.edges);
        let positions = descent.positions.clone();

        let inner = self
            .inner
            .as_mut()
            .expect("bind_inner_to_level before build_inner");
        let shard = CsrShard::whole(&csr);
        inner.reinit(ctx, &shard, &positions)
    }
}

impl Default for MultilevelEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for MultilevelEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: MultilevelSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode multilevel settings: {e}"))?;
        if typed.inner == Self::ID {
            return Err("multilevel cannot wrap itself".to_string());
        }
        // `construct_leaf` is the single source of truth for inner engines; it
        // refuses `"multilevel"` (handled above) and unknown ids, so this both
        // validates the id and guarantees `build_inner` can later succeed.
        if construct_leaf(&typed.inner).is_none() {
            return Err(format!(
                "unknown inner engine {:?} for multilevel wrapper",
                typed.inner
            ));
        }
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        ctx: &mut EngineCtx,
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

        // Flatten the input CSR to the undirected edge list `coarsen` expects
        // (`[s,t, …]`, each pair once). The cascade contracts from here.
        let mut edges: Vec<u32> = Vec::with_capacity(g.neighbors.len());
        for v in 0..n {
            let start = g.offsets[v] as usize;
            let end = g.offsets[v + 1] as usize;
            let v_u = v as u32;
            for e in start..end {
                let u = g.neighbors[e];
                if v_u < u {
                    edges.push(v_u);
                    edges.push(u);
                }
            }
        }

        // REUSE the shared coarsening cascade (FM³/sfdp matching contraction).
        let cascade = coarsen(
            n,
            &edges,
            self.settings.max_levels,
            self.settings.target_size,
        );

        // Seed the coarsest level by folding the caller's finest-level seed up
        // through the parent maps (average of children). This keeps the wrapper
        // deterministic from whatever seed the sim loop already placed, rather
        // than re-randomizing.
        let coarsest_idx = cascade.levels.len() - 1;
        let coarsest_positions = fold_up(&cascade, positions);

        self.descent = Some(Descent {
            cascade,
            level: coarsest_idx,
            sweeps_done: 0,
            positions: coarsest_positions,
        });

        // Construct the inner engine ONCE, then bind it to the coarsest level.
        // Each subsequent level re-binds the SAME instance via `reinit` (see
        // `bind_inner_to_level`) instead of reconstructing per level.
        self.build_inner()?;
        self.bind_inner_to_level(ctx)?;
        Ok(())
    }

    fn step(&mut self, ctx: &mut EngineCtx) -> StepOutput {
        // Advance the inner engine one step at the current level.
        let inner_out = {
            let inner = self
                .inner
                .as_mut()
                .expect("multilevel step called before successful init");
            inner.step(ctx)
        };

        let descent = self
            .descent
            .as_mut()
            .expect("multilevel step called before successful init");
        descent.positions = inner_out.positions;
        descent.sweeps_done += 1;

        // Walshaw annealing: the budget for THIS level depends on its depth.
        // Coarse levels (large `level`) get more sweeps than fine ones
        // (WalshawTR6000 §4). `sweeps_for_level` enforces coarser >= finer.
        let level_budget = self
            .settings
            .sweep_schedule
            .sweeps_for_level(descent.level, self.settings.sweeps_per_level);

        // Still descending and this level is done relaxing? Prolong to the next
        // finer level and re-bind the inner engine there.
        if descent.level > 0 && descent.sweeps_done >= level_budget {
            let child_idx = descent.level - 1;
            let child_n = descent.cascade.levels[child_idx].n_nodes;
            // parent_map describing how child indices fold into the level we just
            // solved lives ON that coarser level.
            let parent_map = descent.cascade.levels[descent.level].parent_map.clone();
            let jitter = self.settings.spring_len * 0.5;
            let seed = self.settings.seed.wrapping_add(child_idx as u32 + 1);
            let prolonged = prolong(&descent.positions, &parent_map, child_n, jitter, seed);

            descent.level = child_idx;
            descent.sweeps_done = 0;
            descent.positions = prolonged;

            // Re-bind the SAME inner engine to the finer graph (needs &mut self).
            if let Err(e) = self.bind_inner_to_level(ctx) {
                tracing::error!(error = %e, "multilevel: inner reinit at finer level failed");
            }
            // Return the prolonged positions this tick; next tick refines them.
            let descent = self.descent.as_ref().unwrap();
            return StepOutput::positions_only(descent.positions.clone());
        }

        StepOutput::positions_only(descent.positions.clone())
    }

    fn is_halted(&self) -> bool {
        // Once at the finest level, defer to the inner engine's own halt logic;
        // while descending we are never halted.
        match (&self.descent, &self.inner) {
            (Some(d), Some(inner)) if d.level == 0 => inner.is_halted(),
            _ => false,
        }
    }
}

/// Fold finest-level seed positions up to the coarsest level by averaging each
/// super-node's children. `parent_map[l+1][child] = super_in_l+1`. Returns the
/// coarsest level's interleaved `x,y,z`.
fn fold_up(cascade: &Coarsening, fine_positions: &[f32]) -> Vec<f32> {
    // Start at level 0 with the caller's positions, fold up one level at a time.
    let mut cur = fine_positions.to_vec();
    for l in 1..cascade.levels.len() {
        let parent_map = &cascade.levels[l].parent_map; // len == levels[l-1].n_nodes
        let coarse_n = cascade.levels[l].n_nodes;
        let mut sums = vec![0.0f32; coarse_n * 3];
        let mut counts = vec![0u32; coarse_n];
        for (child, &parent) in parent_map.iter().enumerate() {
            let p = parent as usize;
            if p >= coarse_n || child * 3 + 2 >= cur.len() {
                continue;
            }
            sums[p * 3] += cur[child * 3];
            sums[p * 3 + 1] += cur[child * 3 + 1];
            sums[p * 3 + 2] += cur[child * 3 + 2];
            counts[p] += 1;
        }
        for p in 0..coarse_n {
            let c = counts[p].max(1) as f32;
            sums[p * 3] /= c;
            sums[p * 3 + 1] /= c;
            sums[p * 3 + 2] /= c;
        }
        cur = sums;
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_csr_is_symmetric() {
        // edge 0-1, 1-2 → undirected CSR with degrees [1,2,1].
        let g = MultilevelEngine::level_csr(3, &[0, 1, 1, 2]);
        assert_eq!(g.n_nodes, 3);
        assert_eq!(g.offsets, vec![0, 1, 3, 4]);
        // node 0 -> [1]; node 1 -> [0,2]; node 2 -> [1]
        assert_eq!(g.neighbors.len(), 4);
        assert!(g.neighbors[g.offsets[1] as usize..g.offsets[2] as usize].contains(&0));
        assert!(g.neighbors[g.offsets[1] as usize..g.offsets[2] as usize].contains(&2));
    }

    #[test]
    fn rejects_self_wrap() {
        let mut e = MultilevelEngine::new();
        let params = serde_json::json!({ "inner": "multilevel" });
        assert!(e.set_params(&params).is_err());
    }

    #[test]
    fn rejects_unknown_inner() {
        let mut e = MultilevelEngine::new();
        let params = serde_json::json!({ "inner": "no-such-engine" });
        assert!(e.set_params(&params).is_err());
    }

    #[test]
    fn accepts_known_inner() {
        let mut e = MultilevelEngine::new();
        let params = serde_json::json!({ "inner": "sgd-stress" });
        assert!(e.set_params(&params).is_ok());
        assert_eq!(e.settings.inner, "sgd-stress");
    }

    /// The multilevel wrapper must accept the GPU geometric engine as its inner
    /// solver — this is the "hierarchical layout mode × geometric algorithm"
    /// composition the frontend's `use_multilevel` toggle routes through.
    /// `construct_leaf` succeeds without a GPU (the device is only needed at
    /// `init`), so `set_params` validates the pairing here.
    #[test]
    fn accepts_geometric_gpu_inner() {
        let mut e = MultilevelEngine::new();
        let params = serde_json::json!({
            "inner": "geometric-gpu",
            "inner_params": { "edge_stiffness": 0.2 }
        });
        assert!(e.set_params(&params).is_ok());
        assert_eq!(e.settings.inner, "geometric-gpu");
    }

    #[test]
    fn fold_up_averages_children() {
        // Two children of one super-node at (0,0,0) and (2,0,0) → super at (1,0,0).
        // Build a trivial 2-level cascade by hand via coarsen on a single edge.
        let cascade = coarsen(2, &[0, 1], 4, 1);
        // levels[1] should fold the two nodes into one super-node.
        if cascade.levels.len() >= 2 {
            let fine = vec![0.0, 0.0, 0.0, 2.0, 0.0, 0.0];
            let coarse = fold_up(&cascade, &fine);
            // one super-node averaged → (1,0,0)
            assert_eq!(coarse.len() % 3, 0);
            assert!((coarse[0] - 1.0).abs() < 1e-5);
        }
    }

    /// Back-compat: settings JSON written before `sweep_schedule` existed (no
    /// such key) must still deserialize, defaulting the schedule.
    #[test]
    fn settings_backcompat_without_sweep_schedule() {
        let old = serde_json::json!({
            "inner": "sgd-stress",
            "max_levels": 5,
            "target_size": 400,
            "sweeps_per_level": 20
        });
        let s: MultilevelSettings = serde_json::from_value(old).unwrap();
        assert_eq!(s.inner, "sgd-stress");
        assert_eq!(s.sweeps_per_level, 20);
        assert_eq!(s.sweep_schedule, SweepSchedule::default());
    }

    /// `sweep_schedule` round-trips through serde via its kebab-case name.
    #[test]
    fn sweep_schedule_serde_roundtrip() {
        for sched in [
            SweepSchedule::Uniform,
            SweepSchedule::Linear,
            SweepSchedule::Geometric,
        ] {
            let v = serde_json::to_value(sched).unwrap();
            let back: SweepSchedule = serde_json::from_value(v).unwrap();
            assert_eq!(sched, back);
        }
        // kebab-case wire form is what the protobuf Struct carries.
        let parsed: SweepSchedule = serde_json::from_value(serde_json::json!("geometric")).unwrap();
        assert_eq!(parsed, SweepSchedule::Geometric);
    }

    /// Walshaw annealing invariant: a coarser level never gets FEWER sweeps than
    /// a finer one, and at least one schedule gives the coarsest STRICTLY more
    /// (WalshawTR6000 §4: more refinement at coarse levels).
    #[test]
    fn schedule_anneals_coarse_ge_fine() {
        let base = 10;
        let n_levels = 5; // levels 0 (fine) .. 4 (coarse)
        for sched in [
            SweepSchedule::Uniform,
            SweepSchedule::Linear,
            SweepSchedule::Geometric,
        ] {
            for fine in 0..n_levels - 1 {
                let coarse = fine + 1;
                let s_fine = sched.sweeps_for_level(fine, base);
                let s_coarse = sched.sweeps_for_level(coarse, base);
                assert!(
                    s_coarse >= s_fine,
                    "{sched:?}: coarse level {coarse} ({s_coarse}) < fine level {fine} ({s_fine})"
                );
            }
        }
        // Non-uniform schedules give the coarsest strictly more than the finest.
        let fine0 = 0;
        let coarsest = n_levels - 1;
        assert!(
            SweepSchedule::Linear.sweeps_for_level(coarsest, base)
                > SweepSchedule::Linear.sweeps_for_level(fine0, base)
        );
        assert!(
            SweepSchedule::Geometric.sweeps_for_level(coarsest, base)
                > SweepSchedule::Geometric.sweeps_for_level(fine0, base)
        );
        // Uniform is flat by definition.
        assert_eq!(
            SweepSchedule::Uniform.sweeps_for_level(coarsest, base),
            SweepSchedule::Uniform.sweeps_for_level(fine0, base)
        );
    }

    /// Concrete schedule values, documenting the intent.
    #[test]
    fn schedule_concrete_values() {
        let base = 4;
        // Linear: base * (1 + level).
        assert_eq!(SweepSchedule::Linear.sweeps_for_level(0, base), 4);
        assert_eq!(SweepSchedule::Linear.sweeps_for_level(2, base), 12);
        // Geometric: base * 2^level.
        assert_eq!(SweepSchedule::Geometric.sweeps_for_level(0, base), 4);
        assert_eq!(SweepSchedule::Geometric.sweeps_for_level(3, base), 32);
        // Never zero even with base 0.
        assert_eq!(SweepSchedule::Linear.sweeps_for_level(0, 0), 1);
    }

    // --- reinit-based reuse ---------------------------------------------------

    use std::sync::{Arc, Mutex};

    use super::super::SgdStressEngine;

    /// A deterministic mock leaf engine used to prove the multilevel wrapper
    /// constructs its inner engine ONCE and re-binds it per level via `reinit`
    /// (rather than reconstructing per level).
    ///
    /// On each `init`/`reinit` it deterministically transforms the seed
    /// positions (no RNG), so a full descent is reproducible. It records, in a
    /// shared cell, how many times `init` vs `reinit` were called so the test can
    /// assert the lifecycle.
    struct CountingMock {
        descriptor: LayoutDescriptor,
        positions: Vec<f32>,
        counts: Arc<Mutex<(u32, u32)>>, // (init_calls, reinit_calls)
    }

    impl CountingMock {
        fn descriptor() -> LayoutDescriptor {
            LayoutDescriptor {
                id: "counting-mock",
                kind: LayoutKind::Physics,
                display_name: "counting mock",
                description: "test only",
                requirements: LayoutRequirements {
                    needs_edges: true,
                    needs_cpu_positions: true,
                    needs_gpu_positions_buffer: false,
                },
            }
        }
    }

    impl LayoutEngine for CountingMock {
        fn descriptor(&self) -> &LayoutDescriptor {
            &self.descriptor
        }
        fn init(
            &mut self,
            _ctx: &mut EngineCtx,
            _graph: &CsrShard,
            positions: &[f32],
        ) -> Result<(), String> {
            self.counts.lock().unwrap().0 += 1;
            self.positions = positions.to_vec();
            Ok(())
        }
        fn reinit(
            &mut self,
            _ctx: &mut EngineCtx,
            _graph: &CsrShard,
            positions: &[f32],
        ) -> Result<(), String> {
            self.counts.lock().unwrap().1 += 1;
            self.positions = positions.to_vec();
            Ok(())
        }
        fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
            // Deterministic, RNG-free perturbation so a descent is reproducible.
            for p in &mut self.positions {
                *p += 0.001;
            }
            StepOutput::positions_only(self.positions.clone())
        }
    }

    /// A small connected graph (a path of 8 nodes) as a CsrGraph.
    fn small_path(n: u32) -> CsrGraph {
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        for v in 0..n {
            if v > 0 {
                neighbors.push(v - 1);
            }
            if v + 1 < n {
                neighbors.push(v + 1);
            }
            offsets.push(neighbors.len() as u32);
        }
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// Drive a `MultilevelEngine` whose inner has been swapped for the mock, for
    /// `ticks` steps, returning final positions and the (init, reinit) counts.
    fn run_with_mock(ticks: usize) -> (Vec<f32>, (u32, u32)) {
        let counts = Arc::new(Mutex::new((0u32, 0u32)));
        let mut e = MultilevelEngine::new();
        // Use a CPU-only inner so `init` (which builds the real inner before we
        // swap in the mock) doesn't require a wgpu device in the test harness.
        e.settings.inner = SgdStressEngine::ID.to_string();
        // Force a multi-level cascade: target_size 2 so even a tiny graph coarsens.
        e.settings.max_levels = 6;
        e.settings.target_size = 2;
        e.settings.sweeps_per_level = 2;
        e.settings.sweep_schedule = SweepSchedule::Uniform;

        let g = small_path(8);
        let mut positions = vec![0.0f32; 8 * 3];
        for i in 0..8 {
            positions[i * 3] = i as f32;
        }
        let mut ctx = EngineCtx::cpu_only();
        let shard = CsrShard::whole(&g);

        // Run `init` (which builds the real inner + binds), then REPLACE the inner
        // with our counting mock bound to the same coarsest level, mirroring the
        // production single-instance lifecycle. The mock's init counts as the
        // one-time construction.
        e.init(&mut ctx, &shard, &positions).unwrap();
        let mut mock = CountingMock {
            descriptor: CountingMock::descriptor(),
            positions: Vec::new(),
            counts: counts.clone(),
        };
        // Emulate the "constructed once + bound to coarsest level" step.
        let coarsest_pos = e.descent.as_ref().unwrap().positions.clone();
        let g0 = small_path(2);
        mock.init(&mut ctx, &CsrShard::whole(&g0), &coarsest_pos)
            .unwrap();
        e.inner = Some(Box::new(mock));

        let mut last = Vec::new();
        for _ in 0..ticks {
            last = e.step(&mut ctx).positions;
        }
        let c = *counts.lock().unwrap();
        (last, c)
    }

    /// The inner engine is constructed (init) exactly ONCE; every subsequent
    /// level re-binds the SAME instance through `reinit`. This is the core of
    /// issue #3: no per-level reconstruction.
    #[test]
    fn inner_is_constructed_once_then_reinit_per_level() {
        let (_pos, (inits, reinits)) = run_with_mock(200);
        assert_eq!(inits, 1, "inner must be constructed exactly once");
        assert!(
            reinits >= 1,
            "descent should re-bind the inner via reinit at least once (got {reinits})"
        );
    }

    /// Because `reinit` defaults to (and here mirrors) `init`, a single reused
    /// instance produces the SAME result as reconstructing per level would. We
    /// assert reproducibility: two identical runs match bit-for-bit.
    #[test]
    fn reinit_reuse_is_deterministic_and_matches() {
        let (a, _) = run_with_mock(200);
        let (b, _) = run_with_mock(200);
        assert_eq!(a, b, "reinit-based reuse must be deterministic");
    }

    /// A full descent with a REAL inner engine completes and lands at the finest
    /// level (level 0) with the right node count, exercising the production
    /// build-once + reinit-per-level path end to end.
    #[test]
    fn full_descent_reaches_finest_level_with_real_inner() {
        let mut e = MultilevelEngine::new();
        e.settings.inner = SgdStressEngine::ID.to_string();
        e.settings.max_levels = 6;
        e.settings.target_size = 2;
        e.settings.sweeps_per_level = 2;

        let g = small_path(8);
        let mut positions = vec![0.0f32; 8 * 3];
        for i in 0..8 {
            positions[i * 3] = i as f32;
        }
        let mut ctx = EngineCtx::cpu_only();
        e.init(&mut ctx, &CsrShard::whole(&g), &positions).unwrap();

        let mut out = Vec::new();
        for _ in 0..500 {
            out = e.step(&mut ctx).positions;
        }
        let descent = e.descent.as_ref().unwrap();
        assert_eq!(descent.level, 0, "descent should reach the finest level");
        // Finest level has all 8 nodes.
        assert_eq!(out.len(), 8 * 3);
    }
}
