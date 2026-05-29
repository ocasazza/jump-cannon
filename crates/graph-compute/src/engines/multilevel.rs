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
//! The inner engine's lifecycle is `init(graph, positions) → step ⟲`. To run it
//! at level `l` we synthesize a [`CsrGraph`] for that level from the cascade's
//! flat edge list and call `inner.init(ctx, shard_l, positions_l)`, then `step`
//! it `sweeps_per_level` times. After that we prolong its output to level `l-1`
//! and re-`init` the inner engine on the finer graph. The wrapper is a small
//! state machine over `step` calls so the cascade descends across ticks (each
//! `step` is one inner step; broadcasts are emitted at every level so the client
//! sees the layout coarsen-to-fine "snap" into place). Once level 0 is reached
//! the wrapper just forwards to the inner engine forever (continuous refine).

use graph_layouts::{coarsen, prolong, Coarsening, LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};

use super::{
    CpuSpringEngine, CsrShard, EngineCtx, Fa2BhEngine, Fa2BruteEngine, LayoutEngine, SgdStressEngine,
    StepOutput,
};
use crate::sim::CsrGraph;

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "multilevel";

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
    /// Inner-engine `step`s to run at each level before prolonging to the next
    /// finer level. Coarse levels need more relaxation; finer levels are already
    /// close (Walshaw). We use the same count at every level for simplicity and
    /// let the finest level (level 0) refine forever.
    pub sweeps_per_level: u32,
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
            spring_len: 30.0,
            seed: 0x5EED,
        }
    }
}

/// Construct the inner engine named by `id`. Mirrors the builtin registry's
/// constructor table (the frozen foundation's `EngineRegistry::builtin`) but is
/// kept local so the wrapper does not need a back-reference to the live
/// registry. Keep this in sync when new engines are registered. `multilevel`
/// itself is intentionally excluded (no recursive wrapping).
fn construct_inner(id: &str) -> Option<Box<dyn LayoutEngine>> {
    match id {
        Fa2BruteEngine::ID => Some(Box::new(Fa2BruteEngine::new())),
        Fa2BhEngine::ID => Some(Box::new(Fa2BhEngine::new())),
        SgdStressEngine::ID => Some(Box::new(SgdStressEngine::new())),
        CpuSpringEngine::ID => Some(Box::new(CpuSpringEngine::new())),
        _ => None,
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

    /// (Re)initialize the inner engine on the level the descent currently points
    /// at, seeding it with `descent.positions`.
    fn init_inner_for_level(&mut self, ctx: &mut EngineCtx) -> Result<(), String> {
        let descent = self
            .descent
            .as_ref()
            .expect("init_inner_for_level before descent built");
        let level = &descent.cascade.levels[descent.level];
        let csr = Self::level_csr(level.n_nodes, &level.edges);
        let positions = descent.positions.clone();

        // Fresh inner instance per level: engines aren't required to support
        // re-init on a different graph (GPU engines rebuild all buffers at
        // init), so we mint a clean one and apply the forwarded params.
        let mut inner = construct_inner(&self.settings.inner)
            .ok_or_else(|| format!("unknown inner engine {:?}", self.settings.inner))?;
        inner.set_params(&self.settings.inner_params)?;
        let shard = CsrShard::whole(&csr);
        inner.init(ctx, &shard, &positions)?;
        self.inner = Some(inner);
        Ok(())
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
        if construct_inner(&typed.inner).is_none() {
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
        let cascade = coarsen(n, &edges, self.settings.max_levels, self.settings.target_size);

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

        // Bring up the inner engine on the coarsest level.
        self.init_inner_for_level(ctx)?;
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

        // Still descending and this level is done relaxing? Prolong to the next
        // finer level and re-init the inner engine there.
        if descent.level > 0 && descent.sweeps_done >= self.settings.sweeps_per_level {
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

            // Re-init the inner engine on the finer graph (needs &mut self).
            if let Err(e) = self.init_inner_for_level(ctx) {
                tracing::error!(error = %e, "multilevel: inner re-init at finer level failed");
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
}
