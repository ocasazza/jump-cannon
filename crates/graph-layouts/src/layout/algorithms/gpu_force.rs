//! WebGPU compute-shader force-directed layout.
//!
//! Runs natively (vulkan/metal/dx12 via wgpu defaults) and in browsers
//! (wgpu's WebGPU backend). No rendering — this is a layout engine; the
//! consumer reads positions out and renders them however it likes.
//!
//! Repulsion backends: exact O(n²), Barnes-Hut octree, or negative
//! sampling (see [`RepulsionMode`]); force laws: spring-electrical or
//! t-FDP (see [`ForceModel`]); CSR-adjacency attraction over Tigr virtual
//! vertices; gravity + cursor; semi-implicit Euler with velocity damping.
//! Designed to step incrementally — caller picks `steps_per_call` and runs
//! `run()` each frame (or as desired).

use crate::types::Graph;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

/// State of the asynchronous energy_buf -> energy_staging readback. Shared
/// between the main thread and the wgpu map_async callback via Arc<Mutex<>>.
///
/// **Critical invariant**: the `map_async` callback must NEVER call any wgpu
/// method (no `get_mapped_range`, no `unmap`, no buffer access). On WASM the
/// callback fires *synchronously* from inside the queue submit codepath, and
/// any wgpu re-entry from there hits `Buffer is already mapped` /
/// "recursive use of an object" panics.
///
/// Discipline: callback only flips this state. All wgpu access happens at
/// the top of the next `step_with_encoder` (`drain_energy_readback`), where
/// no other wgpu code is in flight.
#[derive(Debug)]
enum EnergyReadback {
    /// No copy in flight; staging buffer is unmapped and idle.
    Idle,
    /// `copy_buffer_to_buffer` was recorded into the current frame's
    /// encoder. We have NOT yet issued `map_async` — that has to wait
    /// until the encoder is actually submitted (by eframe, after we
    /// return from `step_with_encoder`). We park here for one frame; on
    /// the next `step_with_encoder` entry we issue `map_async` (the prior
    /// encoder is now submitted, so the buffer is no longer "in use" from
    /// wgpu's perspective).
    CopyScheduled,
    /// `map_async` issued; waiting for the driver/browser to fire our
    /// callback. On WASM the callback can fire synchronously from inside
    /// the queue submit path — the callback flips state and does NOT
    /// touch wgpu, so re-entrancy is safe.
    Mapping,
    /// Callback fired. Ok = staging buffer is now mapped (drain must
    /// `get_mapped_range` + `unmap`); Err = map failed (no unmap needed).
    Done(Result<(), wgpu::BufferAsyncError>),
}

impl Default for EnergyReadback {
    fn default() -> Self {
        EnergyReadback::Idle
    }
}

// ---------- Public API -------------------------------------------------------

/// Repulsion backend selection.
///
/// * `Exact` — every node visits every other node. O(n²) per step; the
///   reference implementation the other two are measured against. Fine
///   below a few thousand nodes.
/// * `BarnesHut` — GPU-built octree (`shaders/octree.wgsl`), stackless rope traversal in WGSL.
///   Default: best visual result on clustered graphs (hubs + long tails).
/// * `NegativeSampling` — K random partners per node per step. O(n·K);
///   the only backend whose cost is independent of spatial density, so
///   it is the large-graph path. Pair with `ForceModel::TFdp` for the
///   SNAP-tFDP estimator (arXiv:2608.01907).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepulsionMode {
    Exact,
    BarnesHut,
    NegativeSampling,
}

impl Default for RepulsionMode {
    fn default() -> Self { RepulsionMode::BarnesHut }
}

/// Pairwise force law shared by the spring and repulsion kernels.
///
/// * `SpringElectrical` — Hooke springs with rest length `spring_len`
///   plus Coulomb `repulsion · m_j / d²` repulsion. The historical model.
/// * `TFdp` — Student-t forces from t-FDP (Zhong et al., TVCG 2023,
///   arXiv:2303.03964). With `r = d / spring_len`:
///   attraction `α (r + β r / (1 + r²))`, repulsion `r / (1 + r²)^γ`.
///   Bounded at short range (no `1/d²` blow-up, so no velocity clamp
///   fights) and `r^(1-2γ)` at long range. Under `NegativeSampling` the
///   repulsion is degree-weighted by `(d_i + d_j) / 2` — the expectation
///   SNAP-tFDP (arXiv:2608.01907, eq. 6) proves its edge-centric sampler
///   optimises — and scaled by `tfdp_k`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForceModel {
    SpringElectrical,
    TFdp,
}

impl Default for ForceModel {
    fn default() -> Self { ForceModel::SpringElectrical }
}

impl ForceModel {
    fn as_u32(self) -> u32 {
        match self {
            ForceModel::SpringElectrical => 0,
            ForceModel::TFdp => 1,
        }
    }
    /// Unknown strings fall back to the default rather than to a
    /// specific variant, so a stale persisted value can never silently
    /// select a non-default force law.
    fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "t_fdp" | "tfdp" | "t-fdp" => ForceModel::TFdp,
            "spring_electrical" | "spring" => ForceModel::SpringElectrical,
            _ => ForceModel::default(),
        }
    }
    fn to_str(self) -> &'static str {
        match self {
            ForceModel::SpringElectrical => "spring_electrical",
            ForceModel::TFdp => "t_fdp",
        }
    }
}

/// How the force-directed sim seeds its initial node positions.
///
/// The sim itself runs in 3-D (xyz, with a vec4-padded GPU buffer); seeders
/// must produce 3-D positions or the layout will collapse to a plane. All
/// variants below are 3-D-safe.
#[derive(Clone, Debug, PartialEq)]
pub enum SeedMode {
    /// Independent uniform-random samples in `[-radius, +radius]` per axis,
    /// where `radius ∝ sqrt(n) * spring_len`. The historical default; cheap
    /// but produces a noisy ball that the sim has to untangle from scratch.
    Random,
    /// Topological-fisheye multilevel seed (Gansner-Koren-North §4): build
    /// a hierarchy of coarsened graphs whose candidate set is graph edges
    /// ∪ filtered Delaunay edges, lay out the coarsest level with a tiny
    /// CPU FR sim, then prolong + relax level-by-level back down. The sim
    /// inherits a near-converged layout and spends its frame budget on
    /// local refinement instead of global untangling.
    TopoFisheye,
    /// Keep whatever positions already live in the shared buffer — generate
    /// no seed and, on `init_with_device`, skip the `write_buffer` that would
    /// otherwise overwrite the buffer. Use this when the caller has already
    /// placed *meaningful* positions in the buffer (a generated graph's sphere,
    /// an explicitly-applied "Initial seed", or a previous sim's settled state)
    /// and wants the force sim to *resume* from them rather than re-seed.
    ///
    /// This is deliberately **not** the default: a fresh vault whose server
    /// positions are all-zero (the `/graph/positions` endpoint is 2-D x,y and
    /// can be degenerate) still needs `Random`/`TopoFisheye` to spread it out.
    None,
    /// Device-side multilevel coarsening seed (see [`super::gpu_multilevel`]).
    /// Builds a heavy-edge-matching coarsening cascade, lays out the coarsest
    /// level, and prolongs positions back down into the fine buffer entirely
    /// on the GPU — zero host readback. The large-graph seed: chosen by
    /// [`GpuForceOptions::for_n_nodes`] above 10k nodes.
    GpuMultilevel,
}

impl Default for SeedMode {
    fn default() -> Self {
        // Preserve historical behaviour; opt-in to topo-fisheye explicitly.
        SeedMode::Random
    }
}

impl SeedMode {
    fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "topo_fisheye" | "topofisheye" | "tf" | "fisheye" => SeedMode::TopoFisheye,
            "none" | "keep" | "keep_current" => SeedMode::None,
            "gpu_multilevel" | "multilevel" | "ml" => SeedMode::GpuMultilevel,
            _ => SeedMode::Random,
        }
    }
    fn to_str(&self) -> &'static str {
        match self {
            SeedMode::Random => "random",
            SeedMode::TopoFisheye => "topo_fisheye",
            SeedMode::None => "none",
            SeedMode::GpuMultilevel => "gpu_multilevel",
        }
    }
}

impl RepulsionMode {
    fn as_u32(self) -> u32 {
        match self {
            RepulsionMode::Exact => 0,
            RepulsionMode::BarnesHut => 1,
            RepulsionMode::NegativeSampling => 2,
        }
    }
    /// Unknown strings (including the retired `"grid"`) fall back to the
    /// default backend. Falling back to `Exact` here would silently put a
    /// stale localStorage value onto the O(n²) path.
    fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "exact" | "naive" => RepulsionMode::Exact,
            "barneshut" | "barnes_hut" | "bh" => RepulsionMode::BarnesHut,
            "negativesampling" | "negative_sampling" | "ns" => RepulsionMode::NegativeSampling,
            _ => RepulsionMode::default(),
        }
    }
    fn to_str(self) -> &'static str {
        match self {
            RepulsionMode::Exact => "exact",
            RepulsionMode::BarnesHut => "barnes_hut",
            RepulsionMode::NegativeSampling => "negative_sampling",
        }
    }
}

/// Tunables for the GPU force engine. Anything in here can be updated
/// per-frame via [`GpuForceLayout::set_options`] without rebuilding GPU
/// resources — only the uniform buffer is rewritten.
#[derive(Clone, Debug)]
pub struct GpuForceOptions {
    pub repulsion: f32,
    pub spring_k: f32,
    pub spring_len: f32,
    pub gravity: f32,
    pub damping: f32,
    pub dt: f32,
    pub cursor_pos: [f32; 3],
    /// 0.0 disables the cursor force entirely.
    pub cursor_radius: f32,
    /// Negative attracts, positive repels.
    pub cursor_strength: f32,
    pub steps_per_call: u32,
    /// Per-pair distance clip on repulsion. <=0 means "no clip" (full O(n^2)
    /// attractor at infinity). Default = 4 * spring_len = 1600.
    pub repulsion_radius: f32,
    /// Geometric per-call cooling factor applied to `effective_damping`.
    /// 1.0 = no cooling. 0.997 cools toward `cooling_floor` over a few
    /// hundred frames. Clamped to `[0.5, 1.0]` at use sites.
    ///
    /// Cooling formula (applied once per `run` / `step_with_encoder` call,
    /// AFTER first-call init has set `effective_damping = options.damping`):
    ///
    /// ```text
    /// effective_damping
    ///     = (effective_damping * cooling_alpha).max(cooling_floor.min(damping))
    /// ```
    ///
    /// Read the inner `min(floor, damping)` as **"the effective floor is
    /// the configured floor, but never above the user's configured damping
    /// — if the user already wants more friction than the floor allows,
    /// honour that."** This is intentional, *not* a typo for `.max`:
    ///
    /// - `damping=0.90, floor=0.55`: cools `0.90 → 0.55` over time.
    /// - `damping=0.30, floor=0.55`: stays pinned at `0.30`. Without the
    ///   `.min`, we'd raise damping back to `0.55` against the user's
    ///   explicit "be more frictional" choice.
    ///
    /// TODO(cooling): expose `effective_damping` for diagnostics/tests so
    /// the validate/test phase can assert convergence on the formula
    /// without having to peek at private state.
    pub cooling_alpha: f32,
    /// Lower bound on `effective_damping` under cooling — but only when the
    /// user's configured `damping` is itself above this floor. See
    /// `cooling_alpha` for the full formula and the `damping < floor` edge
    /// case.
    pub cooling_floor: f32,
    /// Average kinetic-energy threshold below which we consider the layout
    /// converged and short-circuit further dispatches. 0 disables.
    pub energy_threshold: f32,
    /// Repulsion backend. See [`RepulsionMode`].
    pub repulsion_mode: RepulsionMode,
    /// Initial-position seeder. Default `Random` for back-compat. Pick
    /// `TopoFisheye` to seed from the §4 multilevel coarsening pipeline.
    pub seed_mode: SeedMode,
    /// Barnes-Hut acceptance criterion: treat a subtree as a single
    /// body when (cell_size / dist) < theta. 0.5..1.0 is the useful
    /// range; 0.7 is a common sweet spot per Burtscher & Pingali 2011.
    pub theta: f32,
    /// K — random samples per node per step under `NegativeSampling`.
    /// DRGraph reports good convergence at K in [5, 20]; default 8.
    pub repulsion_samples: u32,
    /// Pairwise force law. See [`ForceModel`].
    pub force_model: ForceModel,
    /// t-FDP attraction gain α. Paper default 0.1.
    pub tfdp_alpha: f32,
    /// t-FDP short-range attraction boost β. Paper default 8.
    pub tfdp_beta: f32,
    /// t-FDP repulsion decay exponent γ. Paper default 2.
    pub tfdp_gamma: f32,
    /// SNAP-tFDP negative-sample weight k (relative repulsion strength
    /// under `NegativeSampling` + `TFdp`). Paper recommends 3: k=1 over-
    /// contracts clusters, gains saturate above 3.
    pub tfdp_k: f32,
}

impl GpuForceOptions {
    /// Compare every field *except* the three cursor-pose fields
    /// (`cursor_pos`, `cursor_radius`, `cursor_strength`). Used by
    /// [`GpuForceLayout::set_options`] to decide whether an options swap
    /// is "the user moved the cursor" (do not wake) or "the user changed
    /// a slider / preset / backend" (wake).
    ///
    /// Bit-equality on f32s is fine here: the renderer either copies the
    /// existing options through (no change → identical bits) or writes a
    /// fresh value the user just produced (deliberately different).
    pub fn eq_ignoring_cursor(&self, other: &Self) -> bool {
        // Exhaustive destructure so adding a field to GpuForceOptions
        // without classifying it here is a compile error rather than a
        // silent "new field never wakes the sim" bug. If the new field
        // is non-cursor, add it to the comparison below; if it's a new
        // cursor-pose field, add it to the `_` ignore list and keep this
        // method honest with the doc-comment.
        let Self {
            repulsion,
            spring_k,
            spring_len,
            gravity,
            damping,
            dt,
            cursor_pos: _,
            cursor_radius: _,
            cursor_strength: _,
            steps_per_call,
            repulsion_radius,
            cooling_alpha,
            cooling_floor,
            energy_threshold,
            force_model,
            tfdp_alpha,
            tfdp_beta,
            tfdp_gamma,
            tfdp_k,
            repulsion_mode,
            seed_mode,
            theta,
            repulsion_samples,
        } = self;
        let Self {
            repulsion: o_repulsion,
            spring_k: o_spring_k,
            spring_len: o_spring_len,
            gravity: o_gravity,
            damping: o_damping,
            dt: o_dt,
            cursor_pos: _,
            cursor_radius: _,
            cursor_strength: _,
            steps_per_call: o_steps_per_call,
            repulsion_radius: o_repulsion_radius,
            cooling_alpha: o_cooling_alpha,
            cooling_floor: o_cooling_floor,
            energy_threshold: o_energy_threshold,
            force_model: o_force_model,
            tfdp_alpha: o_tfdp_alpha,
            tfdp_beta: o_tfdp_beta,
            tfdp_gamma: o_tfdp_gamma,
            tfdp_k: o_tfdp_k,
            repulsion_mode: o_repulsion_mode,
            seed_mode: o_seed_mode,
            theta: o_theta,
            repulsion_samples: o_repulsion_samples,
        } = other;
        repulsion.to_bits()           == o_repulsion.to_bits()
            && spring_k.to_bits()         == o_spring_k.to_bits()
            && spring_len.to_bits()       == o_spring_len.to_bits()
            && gravity.to_bits()          == o_gravity.to_bits()
            && damping.to_bits()          == o_damping.to_bits()
            && dt.to_bits()               == o_dt.to_bits()
            && steps_per_call             == o_steps_per_call
            && repulsion_radius.to_bits() == o_repulsion_radius.to_bits()
            && cooling_alpha.to_bits()    == o_cooling_alpha.to_bits()
            && cooling_floor.to_bits()    == o_cooling_floor.to_bits()
            && energy_threshold.to_bits() == o_energy_threshold.to_bits()
            && force_model                == o_force_model
            && tfdp_alpha.to_bits()       == o_tfdp_alpha.to_bits()
            && tfdp_beta.to_bits()        == o_tfdp_beta.to_bits()
            && tfdp_gamma.to_bits()       == o_tfdp_gamma.to_bits()
            && tfdp_k.to_bits()           == o_tfdp_k.to_bits()
            && repulsion_mode             == o_repulsion_mode
            && seed_mode                  == o_seed_mode
            && theta.to_bits()            == o_theta.to_bits()
            && repulsion_samples          == o_repulsion_samples
    }

    /// N-aware defaults. The hand-tuned `Default` block (repulsion 4000,
    /// spring_len 400) was anchored to a ~10k-node vault; using those
    /// numbers for a 100-node graph leaves the layout densely packed and
    /// for a 100k-node graph leaves it cramped. Scale the magnitude
    /// knobs against `cbrt(n)` (3D analog of the FR `sqrt(area/n)`
    /// scaling) so the equilibrium edge length grows with the graph.
    ///
    /// Anchors:
    ///   n =     4 → spring_len  ≈ 40  (clamp floor)
    ///   n =   100 → spring_len  ≈ 86,  repulsion  ≈ 861
    ///   n =  1000 → spring_len  ≈ 186, repulsion  ≈ 1857
    ///   n = 10000 → spring_len  ≈ 400, repulsion  ≈ 4000  (user-tuned anchor)
    ///   n =100000 → spring_len  ≈ 862, repulsion  ≈ 8617
    pub fn for_n_nodes(n: usize) -> Self {
        let mut o = Self::default();
        let cbrt = (n.max(1) as f32).powf(1.0 / 3.0);
        // Coefficient picked so cbrt(10000) * coeff ≈ 400 (the user's
        // approved spring_len for ~10k nodes).
        let len  = (18.57 * cbrt).clamp(40.0, 1500.0);
        let repl = (185.7 * cbrt).clamp(200.0, 50_000.0);
        o.spring_len = len;
        o.repulsion  = repl;
        o.repulsion_radius = (4.0 * len).max(160.0);
        // Scale the auto-halt energy threshold with the layout's
        // natural length. The fixed default (0.05) was tuned for the
        // ~10k-node anchor where `spring_len=400` and per-node KE
        // routinely lands in the 1-10 range during the relax phase.
        // On a small vault (`spring_len ~ 40`) KE never reaches that
        // threshold, so the sim halts after `HALT_GRACE_STEPS` while
        // still in the early everything-uniform phase — the user sees
        // a frozen layout that "isn't moving."
        //
        // 1e-4 × spring_len matches the floor scale used in
        // `force.wgsl::dist2_floor` (`spring_len² × 1e-4`) so a node
        // that hasn't moved more than a small fraction of one spring
        // length per step is what counts as "settled."
        o.energy_threshold = (len * 1.0e-4).max(1.0e-6);
        // Below ~10k nodes the CPU topo-fisheye / warmup seed is cheaper than
        // 16 levels of GPU dispatch overhead; above it, the fully-device
        // multilevel seed keeps large graphs off the host entirely.
        o.seed_mode = if n > 10_000 { SeedMode::GpuMultilevel } else { SeedMode::Random };
        o
    }
}

impl Default for GpuForceOptions {
    fn default() -> Self {
        Self {
            // Spread-friendly defaults: real Obsidian vaults are big
            // (10k+ nodes, dense hub clusters) so the sim needs strong
            // repulsion + long springs to keep communities legible.
            // repulsion_radius = 4 × spring_len bounds the long-range
            // repulsion every backend pays for.
            repulsion: 4000.0,
            spring_k: 1.0,
            spring_len: 400.0,
            gravity: 0.01,
            damping: 0.90,
            dt: 0.10,
            cursor_pos: [0.0; 3],
            cursor_radius: 0.0,
            cursor_strength: 0.0,
            steps_per_call: 8,
            repulsion_radius: 1600.0,
            cooling_alpha: 0.997,
            cooling_floor: 0.55,
            energy_threshold: 0.05,
            repulsion_mode: RepulsionMode::default(),
            seed_mode: SeedMode::default(),
            theta: 0.7,
            repulsion_samples: 8,
            force_model: ForceModel::default(),
            tfdp_alpha: 0.1,
            tfdp_beta: 8.0,
            tfdp_gamma: 2.0,
            tfdp_k: 3.0,
        }
    }
}


// Hand-rolled serde so callers can pass JSON through the WASM bridge
// without dragging serde derives onto wgpu types.
impl serde::Serialize for GpuForceOptions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("GpuForceOptions", 23)?;
        st.serialize_field("repulsion", &self.repulsion)?;
        st.serialize_field("spring_k", &self.spring_k)?;
        st.serialize_field("spring_len", &self.spring_len)?;
        st.serialize_field("gravity", &self.gravity)?;
        st.serialize_field("damping", &self.damping)?;
        st.serialize_field("dt", &self.dt)?;
        st.serialize_field("cursor_pos", &self.cursor_pos)?;
        st.serialize_field("cursor_radius", &self.cursor_radius)?;
        st.serialize_field("cursor_strength", &self.cursor_strength)?;
        st.serialize_field("steps_per_call", &self.steps_per_call)?;
        st.serialize_field("repulsion_radius", &self.repulsion_radius)?;
        st.serialize_field("cooling_alpha", &self.cooling_alpha)?;
        st.serialize_field("cooling_floor", &self.cooling_floor)?;
        st.serialize_field("energy_threshold", &self.energy_threshold)?;
        st.serialize_field("repulsion_mode", self.repulsion_mode.to_str())?;
        st.serialize_field("seed_mode", self.seed_mode.to_str())?;
        st.serialize_field("theta", &self.theta)?;
        st.serialize_field("repulsion_samples", &self.repulsion_samples)?;
        st.serialize_field("force_model", self.force_model.to_str())?;
        st.serialize_field("tfdp_alpha", &self.tfdp_alpha)?;
        st.serialize_field("tfdp_beta", &self.tfdp_beta)?;
        st.serialize_field("tfdp_gamma", &self.tfdp_gamma)?;
        st.serialize_field("tfdp_k", &self.tfdp_k)?;
        st.end()
    }
}

impl<'de> serde::Deserialize<'de> for GpuForceOptions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            repulsion: Option<f32>,
            #[serde(default)]
            spring_k: Option<f32>,
            #[serde(default)]
            spring_len: Option<f32>,
            #[serde(default)]
            gravity: Option<f32>,
            #[serde(default)]
            damping: Option<f32>,
            #[serde(default)]
            dt: Option<f32>,
            #[serde(default)]
            cursor_pos: Option<[f32; 3]>,
            #[serde(default)]
            cursor_radius: Option<f32>,
            #[serde(default)]
            cursor_strength: Option<f32>,
            #[serde(default)]
            steps_per_call: Option<u32>,
            #[serde(default)]
            repulsion_radius: Option<f32>,
            #[serde(default)]
            cooling_alpha: Option<f32>,
            #[serde(default)]
            cooling_floor: Option<f32>,
            #[serde(default)]
            energy_threshold: Option<f32>,
            #[serde(default)]
            repulsion_mode: Option<String>,
            #[serde(default)]
            seed_mode: Option<String>,
            #[serde(default)]
            theta: Option<f32>,
            #[serde(default)]
            repulsion_samples: Option<u32>,
            #[serde(default)]
            force_model: Option<String>,
            #[serde(default)]
            tfdp_alpha: Option<f32>,
            #[serde(default)]
            tfdp_beta: Option<f32>,
            #[serde(default)]
            tfdp_gamma: Option<f32>,
            #[serde(default)]
            tfdp_k: Option<f32>,
        }
        let r = Raw::deserialize(d)?;
        let def = GpuForceOptions::default();
        Ok(GpuForceOptions {
            repulsion: r.repulsion.unwrap_or(def.repulsion),
            spring_k: r.spring_k.unwrap_or(def.spring_k),
            spring_len: r.spring_len.unwrap_or(def.spring_len),
            gravity: r.gravity.unwrap_or(def.gravity),
            damping: r.damping.unwrap_or(def.damping),
            dt: r.dt.unwrap_or(def.dt),
            cursor_pos: r.cursor_pos.unwrap_or(def.cursor_pos),
            cursor_radius: r.cursor_radius.unwrap_or(def.cursor_radius),
            cursor_strength: r.cursor_strength.unwrap_or(def.cursor_strength),
            steps_per_call: r.steps_per_call.unwrap_or(def.steps_per_call),
            repulsion_radius: r.repulsion_radius.unwrap_or(def.repulsion_radius),
            cooling_alpha: r.cooling_alpha.unwrap_or(def.cooling_alpha),
            cooling_floor: r.cooling_floor.unwrap_or(def.cooling_floor),
            energy_threshold: r.energy_threshold.unwrap_or(def.energy_threshold),
            repulsion_mode: r.repulsion_mode
                .as_deref()
                .map(RepulsionMode::from_str)
                .unwrap_or(def.repulsion_mode),
            seed_mode: r.seed_mode
                .as_deref()
                .map(SeedMode::from_str)
                .unwrap_or(def.seed_mode),
            theta: r.theta.unwrap_or(def.theta),
            repulsion_samples: r.repulsion_samples.unwrap_or(def.repulsion_samples),
            force_model: r.force_model
                .as_deref()
                .map(ForceModel::from_str)
                .unwrap_or(def.force_model),
            tfdp_alpha: r.tfdp_alpha.unwrap_or(def.tfdp_alpha),
            tfdp_beta: r.tfdp_beta.unwrap_or(def.tfdp_beta),
            tfdp_gamma: r.tfdp_gamma.unwrap_or(def.tfdp_gamma),
            tfdp_k: r.tfdp_k.unwrap_or(def.tfdp_k),
        })
    }
}

/// Index-based topology for the string-free ingest path. `edges` is the
/// undirected edge list `[s0, t0, s1, t1, ...]` (each edge listed once,
/// indices `< n_nodes`). `positions`, when `Some`, is `[x, y, z]` per node
/// and overrides the seeder wholesale; when `None` the layout seeds per its
/// configured [`SeedMode`].
pub struct CsrInput<'a> {
    pub n_nodes: u32,
    pub edges: &'a [u32],
    pub positions: Option<&'a [f32]>,
}

/// Owns the wgpu device + queue when the layout is constructed via the
/// legacy `run()` path. The shared/borrowed path leaves this `None` since
/// the caller's renderer owns those.
struct OwnedDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

pub struct GpuForceLayout {
    options: GpuForceOptions,
    state: Option<GpuState>,
    owned_device: Option<OwnedDevice>,
    /// Once max-KE has stayed below `energy_threshold` for `HALT_FRAMES`
    /// consecutive observed readbacks, the sim is considered settled and
    /// `step_with_encoder` becomes a no-op until something calls `wake()` or
    /// updates options in a way that perturbs the system.
    halted: bool,
    halt_streak: u32,
    /// Step count since last wake. Halt is suppressed until this exceeds
    /// `HALT_GRACE_STEPS` so the sim can break out of degenerate initial
    /// conditions (e.g., uniform sphere, ring) before being declared settled.
    steps_since_wake: u32,
    /// Most recent max-KE reduction value (for diagnostics / stats UI).
    last_max_ke: f32,
    /// Monotonic dispatch counter — fed into the WGSL PRNG seed so each
    /// step picks a different K-set under negative sampling. Wraps fine.
    step_index: u32,
}

/// How many consecutive low-KE readbacks we require before halting. With
/// `steps_per_call = 8` and ~60fps this is half a second of "settled" before
/// we flip to halt.
const HALT_FRAMES: u32 = 30;

/// Minimum number of compute dispatches before halting becomes possible.
/// Prevents premature halt in the early "everything is at uniform low velocity"
/// phase that happens with random sphere seeding.
///
/// Sizing: with the default `steps_per_call = 8` at 60 fps that's
/// `600 / (8 * 60) ≈ 1.25 s` of grace. With the post-click cool-down
/// path's transient `steps_per_call = 2` it stretches to ~5 s, which
/// matches the comment in `app.rs::apply_cursor_force`. Keep both call
/// sites in sync if either knob shifts again.
const HALT_GRACE_STEPS: u32 = 600;

impl GpuForceLayout {
    pub fn new(options: GpuForceOptions) -> Self {
        Self {
            options,
            state: None,
            owned_device: None,
            halted: false,
            halt_streak: 0,
            steps_since_wake: 0,
            last_max_ke: 0.0,
            step_index: 0,
        }
    }

    /// Replace the live options.
    ///
    /// **Wake policy.** A naive "always wake on any options change" is
    /// wrong: the renderer pushes cursor pose into `cursor_pos /
    /// cursor_radius / cursor_strength` every frame the user holds LMB
    /// (and once more on release to zero them). Each of those flow
    /// through `set_settings_json → set_settings → set_options`. If we
    /// `wake()` on every cursor mutation, a halted sim restarts the
    /// instant the user clicks anywhere on the canvas — even if the
    /// click did nothing (no force radius, no actual perturbation) —
    /// and the user sees the graph drift again from rest.
    ///
    /// Policy (option (a) per the bug ticket): hash the **non-cursor**
    /// fields and only `wake()` when those change. Cursor fields are
    /// always copied through (so the active force still applies on the
    /// next dispatch), but a halted sim stays halted unless the cursor
    /// is actually exerting force — in which case
    /// `dispatch_borrowed_step` writes nonzero velocities and the
    /// energy threshold will exit halt naturally on the next readback.
    ///
    /// Slider / preset / backend changes still hit `wake()` because they
    /// alter the non-cursor hash.
    pub fn set_options(&mut self, options: GpuForceOptions) {
        let non_cursor_changed = !options.eq_ignoring_cursor(&self.options);
        self.options = options;
        if non_cursor_changed {
            self.wake();
        }
    }

    /// Re-activate a halted sim. Call this from JS / cursor tool / preset
    /// switch / anywhere that perturbs the layout from the outside.
    pub fn wake(&mut self) {
        self.halted = false;
        self.halt_streak = 0;
        self.steps_since_wake = 0;
        // Reset effective_damping back to the user's configured `damping`.
        // Without this, a backend swap / preset apply / cursor poke that
        // arrives after the sim has cooled to the floor (e.g. 0.55) gets
        // its fresh velocities crushed within a few steps and the user
        // sees no movement. Restarting at the configured damping lets the
        // cooling schedule run from the top again.
        if let Some(s) = self.state.as_mut() {
            s.effective_damping = self.options.damping;
        }
    }

    /// True once the sim has been observed below `energy_threshold` for
    /// [`HALT_FRAMES`] consecutive readbacks. While halted, `step_with_encoder`
    /// is a no-op.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Most recent max-per-node kinetic-energy proxy from the readback path.
    /// Returns 0.0 before the first readback completes.
    pub fn last_max_ke(&self) -> f32 {
        self.last_max_ke
    }

    pub fn options(&self) -> &GpuForceOptions {
        &self.options
    }

    pub fn node_count(&self) -> Option<usize> {
        self.state.as_ref().map(|s| s.n_nodes as usize)
    }

    /// Run `steps_per_call` simulation steps. Initialises GPU resources on
    /// first call (or whenever the graph topology has changed). Writes back
    /// positions into `graph.nodes[*].position3`.
    ///
    /// This is the legacy "I own everything" path — it creates its own
    /// `wgpu::Instance + Device + Queue + positions buffer`. Kept for native
    /// standalone callers / WASM `LayoutManager`. For sharing GPU resources
    /// with a renderer, use [`init_with_device`] + [`step_with_encoder`].
    pub async fn run(&mut self, graph: &mut Graph) -> Result<(), String> {
        // (Re)build GPU state if topology changed or this is the first run.
        let needs_rebuild = match &self.state {
            None => true,
            Some(state) => {
                state.n_nodes as usize != graph.nodes.len()
                    || state.n_edges as usize != graph.edges.len()
                    || !matches!(state.positions, PositionsStorage::Owned { .. })
            }
        };
        if needs_rebuild {
            self.ensure_owned_device().await?;
            let od = self.owned_device.as_ref().unwrap();
            let pc = precompute(graph, &self.options.seed_mode, self.options.spring_len);
            self.state = Some(GpuState::new_owned(&od.device, pc)?);
            if matches!(self.options.seed_mode, SeedMode::GpuMultilevel) {
                self.state
                    .as_mut()
                    .unwrap()
                    .run_multilevel_seed_owned(&od.device, &od.queue, &self.options);
            }
        }

        let od = self
            .owned_device
            .as_ref()
            .ok_or_else(|| "owned device missing".to_string())?;
        let state = self.state.as_mut().unwrap();
        encode_owned_steps(od, state, &self.options, &mut self.step_index);
        let positions = state.read_positions_owned(&od.device, &od.queue).await?;
        // Write back into the graph in the same id-order we built the buffer.
        if let Some(order) = state.node_order.as_ref() {
            for (id, p) in order.iter().zip(positions.chunks_exact(4)) {
                if let Some(node) = graph.nodes.get_mut(id) {
                    node.position3 = Some([p[0], p[1], p[2]]);
                }
            }
        }
        Ok(())
    }

    /// Build GPU compute resources against a caller-supplied
    /// `wgpu::Device + Queue + positions buffer`. The positions buffer must
    /// be sized for `graph.nodes.len() * 16` bytes (vec3 + pad per node) and
    /// usable as a STORAGE buffer (and whatever else the caller needs —
    /// typically also VERTEX/COPY_SRC/COPY_DST for renderer sharing).
    ///
    /// After init, [`step_with_encoder`] records compute dispatches into a
    /// caller-supplied encoder. The shared positions buffer always contains
    /// the latest simulation state after the encoder is submitted, so a
    /// vertex shader bound to the same buffer reads current positions with
    /// zero CPU copies per frame.
    pub fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buffer: &wgpu::Buffer,
    ) -> Result<(), String> {
        let pc = precompute(graph, &self.options.seed_mode, self.options.spring_len);
        let mut state = GpuState::new_borrowed(device, positions_buffer, pc)?;
        // SeedMode::None means "keep whatever is already in the shared buffer":
        // skip the write_buffer that would clobber meaningful caller-supplied
        // positions (generated sphere, applied seed, prior settled state).
        // The initial-position mirror kept in `new_borrowed` comes from the
        // graph's `position3` (synced from the live buffer by the caller).
        if !matches!(self.options.seed_mode, SeedMode::None) {
            state.upload_initial_positions_to(queue, positions_buffer);
        }
        if matches!(self.options.seed_mode, SeedMode::GpuMultilevel) {
            state.run_multilevel_seed(device, queue, positions_buffer, &self.options);
        }
        self.state = Some(state);
        Ok(())
    }

    /// Acquire and cache an owned `wgpu::Device`/`Queue` for the `run` /
    /// `run_csr` paths. No-op once one exists.
    async fn ensure_owned_device(&mut self) -> Result<(), String> {
        if self.owned_device.is_some() {
            return Ok(());
        }
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| "no GPU adapter".to_string())?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("graph-layouts/gpu_force"),
                    required_features: wgpu::Features::empty(),
                    required_limits: gpu_force_device_limits(&adapter.limits()),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .map_err(|e| format!("request_device failed: {e}"))?;
        self.owned_device = Some(OwnedDevice { device, queue });
        Ok(())
    }

    /// String-free counterpart to [`init_with_device`]: build GPU compute
    /// resources from an index-based [`CsrInput`] against a caller-supplied
    /// device + queue + positions buffer. No [`Graph`] is materialised.
    pub fn init_with_device_csr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &CsrInput<'_>,
        positions_buffer: &wgpu::Buffer,
    ) -> Result<(), String> {
        let pc = precompute_csr(
            input.n_nodes,
            input.edges,
            input.positions,
            &self.options.seed_mode,
            self.options.spring_len,
        );
        let mut state = GpuState::new_borrowed(device, positions_buffer, pc)?;
        // SeedMode::None means "keep whatever is already in the shared buffer";
        // every other mode uploads the seeded (or caller-supplied) positions.
        if !matches!(self.options.seed_mode, SeedMode::None) {
            state.upload_initial_positions_to(queue, positions_buffer);
        }
        if matches!(self.options.seed_mode, SeedMode::GpuMultilevel) {
            state.run_multilevel_seed(device, queue, positions_buffer, &self.options);
        }
        self.state = Some(state);
        Ok(())
    }

    /// Owned-device, string-free run: (re)builds GPU state from `input`,
    /// steps `steps_per_call` times, and writes `[x, y, z]` per node into
    /// `out` (cleared and resized). Never allocates a [`Graph`].
    pub async fn run_csr(
        &mut self,
        input: &CsrInput<'_>,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let n_edges = (input.edges.len() / 2) as u32;
        let needs_rebuild = match &self.state {
            None => true,
            Some(state) => {
                state.n_nodes != input.n_nodes
                    || state.n_edges != n_edges
                    || !matches!(state.positions, PositionsStorage::Owned { .. })
            }
        };
        if needs_rebuild {
            self.ensure_owned_device().await?;
            let od = self.owned_device.as_ref().unwrap();
            let pc = precompute_csr(
                input.n_nodes,
                input.edges,
                input.positions,
                &self.options.seed_mode,
                self.options.spring_len,
            );
            self.state = Some(GpuState::new_owned(&od.device, pc)?);
            if matches!(self.options.seed_mode, SeedMode::GpuMultilevel) {
                self.state
                    .as_mut()
                    .unwrap()
                    .run_multilevel_seed_owned(&od.device, &od.queue, &self.options);
            }
        }

        let od = self
            .owned_device
            .as_ref()
            .ok_or_else(|| "owned device missing".to_string())?;
        let state = self.state.as_mut().unwrap();
        encode_owned_steps(od, state, &self.options, &mut self.step_index);
        let positions = state.read_positions_owned(&od.device, &od.queue).await?;
        // Strip the vec4 padding down to `[x, y, z]` per node.
        let n = state.n_nodes as usize;
        out.clear();
        out.reserve(n * 3);
        for p in positions.chunks_exact(4).take(n) {
            out.push(p[0]);
            out.push(p[1]);
            out.push(p[2]);
        }
        Ok(())
    }

    /// Record `steps_per_call` compute dispatches into the caller's encoder.
    /// `device` and `queue` must be the same ones passed to
    /// `init_with_device`. `queue` is used to write the params uniform
    /// before the dispatches; `device` to allocate the per-step bind group.
    /// No-op if the layout isn't initialised.
    pub fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        shared_buffer: &wgpu::Buffer,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };

        // Drive any pending native callbacks. On WASM the browser drives
        // map_async via the event loop; on native we have to poll. `Poll`
        // is non-blocking — if no GPU work has finished yet this just
        // returns immediately and the callback fires on a later frame.
        #[cfg(not(target_arch = "wasm32"))]
        {
            device.poll(wgpu::Maintain::Poll);
        }

        // If the previous frame parked us in CopyScheduled (we recorded
        // the copy but didn't issue map_async because eframe hadn't
        // submitted yet), eframe has now submitted that encoder. It's
        // safe to issue map_async — the copy is in flight, the buffer is
        // no longer "in use by a pending submit". The callback will only
        // mutate state, so even synchronous WASM dispatch is safe.
        let was_copy_scheduled = matches!(
            state.energy_readback.lock().ok().as_deref(),
            Some(EnergyReadback::CopyScheduled)
        );
        if was_copy_scheduled {
            state.issue_energy_map();
        }

        // Drain a previously-scheduled readback (if any) and update halt
        // bookkeeping. We do this BEFORE the early-return so that even after
        // halting we still unmap a stragglar staging buffer cleanly.
        if let Some(max_ke) = state.drain_energy_readback() {
            self.last_max_ke = max_ke;
            // Suppress halt during the grace period — even truly low velocities
            // early on are usually because the random initial layout hasn't
            // had time to gain energy yet, not because it's converged.
            if self.steps_since_wake >= HALT_GRACE_STEPS
                && self.options.energy_threshold > 0.0
                && max_ke < self.options.energy_threshold
            {
                self.halt_streak = self.halt_streak.saturating_add(1);
                if self.halt_streak >= HALT_FRAMES {
                    if !self.halted {
                        // log::info! so this surfaces on WASM (console)
                        // as well as native (env_logger / pretty_env_logger).
                        log::info!(
                            "gpu_force: halted (max_ke={:.4} < threshold={:.4} after {} steps)",
                            max_ke, self.options.energy_threshold, self.steps_since_wake
                        );
                    }
                    self.halted = true;
                }
            } else {
                self.halt_streak = 0;
            }
        }

        if self.halted {
            // Sim is at rest. No dispatch, no readback. The renderer will
            // still draw the last positions (they live in the shared buffer).
            return;
        }

        // First-call init for cooling. See twin comment in `run()` for the
        // policy (idempotent re-init; no oscillation when damping=0.0).
        if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
            state.effective_damping = self.options.damping;
        }
        // Cool per call. Formula documented on `GpuForceOptions::cooling_alpha`.
        let alpha = self.options.cooling_alpha.clamp(0.5, 1.0);
        let floor = self.options.cooling_floor.clamp(0.0, 1.0);
        state.effective_damping = (state.effective_damping * alpha).max(floor.min(self.options.damping));

        // Barnes-Hut builds its octree on the GPU once per call, recorded
        // into the caller's encoder before the first force step. Zero host
        // readback, no CPU tree. Exact/NegativeSampling read positions direct.
        if matches!(self.options.repulsion_mode, RepulsionMode::BarnesHut) {
            let (pos_in, _) = state.borrowed_in_out(shared_buffer);
            state
                .octree
                .encode_build(device, encoder, pos_in, &state.oct_nodes_buf);
        }
        // First write_params with the *current* step_index — re-written
        // per inner step below so the WGSL PRNG advances under negative
        // sampling.
        state.write_params(queue, &self.options, self.step_index);
        let steps = self.options.steps_per_call.max(1);
        for step_i in 0..steps {
            if step_i > 0 {
                self.step_index = self.step_index.wrapping_add(1);
                state.write_params(queue, &self.options, self.step_index);
            }
            state.dispatch_borrowed_step(device, encoder, shared_buffer);
            state.swap_position_buffers();
        }
        // Bump once more so the next call's first step also gets a fresh
        // seed (otherwise calls 1 and 2 would replay the same step_index).
        self.step_index = self.step_index.wrapping_add(1);
        self.steps_since_wake = self.steps_since_wake.saturating_add(steps);
        // Make sure the shared (external/borrowed) buffer ends up holding
        // the latest result. Convention:
        //   - Borrowed mode: pos_a == shared, pos_b == internal.
        //   - Each dispatch writes "out", then we flip a_is_in.
        //   - After dispatch+swap, a_is_in indicates which buffer is the
        //     NEXT step's "in" — i.e. which buffer holds the latest result.
        //     So after the loop, if a_is_in == true the latest is pos_a
        //     (shared, good). If a_is_in == false the latest is pos_b
        //     (internal) — copy it to shared so the renderer reads it.
        if !state.a_is_in {
            encoder.copy_buffer_to_buffer(
                state.positions.pos_b(),
                0,
                shared_buffer,
                0,
                state.pos_buf_size,
            );
        }

        // Schedule an async energy readback ONLY if energy_threshold > 0
        // (i.e. the user actually wants the auto-halt feature). When it's
        // disabled, skip the copy_buffer_to_buffer + map_async entirely —
        // those generate per-frame "Buffer used while mapped" warnings on
        // WASM where map_async fires synchronously and re-entrancy bites.
        if self.options.energy_threshold > 0.0 {
            let readback_idle = state
                .energy_readback
                .lock()
                .map(|g| matches!(*g, EnergyReadback::Idle))
                .unwrap_or(false);
            if readback_idle {
                // Record the copy + park in CopyScheduled. The next
                // step_with_encoder entry sees CopyScheduled and issues
                // map_async there — by which point eframe has submitted
                // this frame's encoder. Calling issue_energy_map here
                // would race the not-yet-submitted copy and trigger
                // wgpu's "Buffer used in submit while mapped" warning
                // every frame.
                state.schedule_energy_copy(encoder);
            }
        }
    }

    /// Read positions back to the CPU. Useful for picking / debugging from
    /// the new shared-buffer path (the legacy `run()` already does this
    /// internally).
    pub async fn read_back_positions(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared_buffer: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let Some(state) = self.state.as_ref() else {
            return Err("layout not initialised".to_string());
        };
        state
            .read_positions_with_device(device, queue, shared_buffer)
            .await
    }
}

/// Cool damping and record `steps_per_call` owned-device force steps into a
/// fresh encoder each step, advancing `step_index`. Shared by `run` and
/// `run_csr` (the `Graph` and CSR owned-device entry points).
fn encode_owned_steps(
    od: &OwnedDevice,
    state: &mut GpuState,
    options: &GpuForceOptions,
    step_index: &mut u32,
) {
    // First-call init for cooling. `effective_damping` is constructed at 1.0
    // in `GpuState::new_*`; the `<= 0.0` branch also catches the edge case
    // where someone explicitly set `damping = 0.0` ("freeze immediately"),
    // in which case `effective_damping` stays 0.0 (the re-init writes 0.0
    // back). Idempotent — no oscillation.
    if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
        state.effective_damping = options.damping;
    }
    // Cool damping per call. See `GpuForceOptions::cooling_alpha` for the
    // formula and the rationale behind the inner `floor.min(damping)`.
    let alpha = options.cooling_alpha.clamp(0.5, 1.0);
    let floor = options.cooling_floor.clamp(0.0, 1.0);
    state.effective_damping = (state.effective_damping * alpha).max(floor.min(options.damping));

    // Barnes-Hut builds its octree entirely on the GPU once per call,
    // reading the current positions. No host readback, no CPU tree.
    if matches!(options.repulsion_mode, RepulsionMode::BarnesHut) {
        let (pos_in, _) = state.owned_in_out();
        let mut enc = od.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("octree_build"),
        });
        state
            .octree
            .encode_build(&od.device, &mut enc, pos_in, &state.oct_nodes_buf);
        od.queue.submit(Some(enc.finish()));
    }
    let total_steps = options.steps_per_call.max(1);
    for _ in 0..total_steps {
        // Re-write params per step so step_index advances under negative
        // sampling (the WGSL PRNG keys off it). Other backends ignore it but
        // the write is cheap.
        state.write_params(&od.queue, options, *step_index);
        *step_index = step_index.wrapping_add(1);
        state.dispatch_step_direct(&od.device, &od.queue);
        state.swap_position_buffers();
    }
}

// ---------- Internal GPU state ----------------------------------------------

/// Mirrors `SimParams` in `shaders/force.wgsl` field-for-field. Every row
/// below is one 16-byte uniform slot; keep both sides in lockstep.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SimParamsRaw {
    repulsion: f32,
    spring_k: f32,
    spring_len: f32,
    gravity: f32,

    damping: f32,
    dt: f32,
    cursor_radius: f32,
    cursor_strength: f32,

    cursor_pos: [f32; 3],
    n_nodes: u32,

    n_edges: u32,
    repulsion_radius: f32,
    repulsion_mode: u32,
    bh_theta: f32,

    n_octree: u32,
    repulsion_samples: u32,   // K — only consulted when repulsion_mode == 2.
    step_index: u32,          // PRNG seed component for negative sampling.
    force_model: u32,         // 0 = spring-electrical, 1 = t-FDP.

    tfdp_alpha: f32,
    tfdp_beta: f32,
    tfdp_gamma: f32,
    tfdp_k: f32,
}

/// Serialise a `SimParamsRaw` for a device-multilevel coarse-level force
/// step: `NegativeSampling` repulsion (no octree scratch), the user's force
/// model + spring/repulsion tunables, and the supplied node/edge counts.
/// `n_nodes` is spliced with the device-computed coarse count on the GPU for
/// coarse levels (see `gpu_multilevel`), so the value passed here is only a
/// placeholder there; the fine level passes its exact count.
pub(crate) fn ml_coarse_params_bytes(
    opts: &GpuForceOptions,
    n_nodes: u32,
    n_edges: u32,
    step_index: u32,
) -> Vec<u8> {
    let raw = SimParamsRaw {
        repulsion: opts.repulsion,
        spring_k: opts.spring_k,
        spring_len: opts.spring_len,
        gravity: opts.gravity,
        damping: opts.damping,
        dt: opts.dt,
        cursor_radius: 0.0,
        cursor_strength: 0.0,
        cursor_pos: [0.0; 3],
        n_nodes,
        n_edges,
        repulsion_radius: opts.repulsion_radius,
        repulsion_mode: RepulsionMode::NegativeSampling.as_u32(),
        bh_theta: opts.theta.clamp(0.1, 2.0),
        n_octree: 0,
        repulsion_samples: opts.repulsion_samples.max(1),
        step_index,
        force_model: opts.force_model.as_u32(),
        tfdp_alpha: opts.tfdp_alpha.max(0.0),
        tfdp_beta: opts.tfdp_beta.max(0.0),
        tfdp_gamma: opts.tfdp_gamma.max(0.5),
        tfdp_k: opts.tfdp_k.max(0.0),
    };
    bytemuck::bytes_of(&raw).to_vec()
}

// Each vec3<f32> in a storage buffer occupies 16 bytes (vec3 has stride/align
// of 16 in WGSL). We use a 4-component layout on the CPU side to match.
const VEC3_STRIDE: u64 = 16;

/// wgpu rejects zero-sized storage buffers at bind-group validation, and an
/// empty graph (0 nodes / 0 edges) still mounts the pipelines (e.g. a
/// freshly created world in the app), so vec-backed buffers pad to one
/// dummy element. Shaders never read the dummy: dispatch counts derive from
/// n_nodes / n_edges.
fn nonempty_f32(v: &[f32]) -> &[f32] {
    if v.is_empty() { &[0.0; 4] } else { v }
}

fn nonempty_u32(v: &[u32]) -> &[u32] {
    if v.is_empty() { &[0] } else { v }
}

/// Position buffer ownership.
///
/// In the legacy `run()` path the GPU state owns both ping-pong buffers.
/// In the renderer-shared path the renderer owns one buffer (used as both
/// vertex source and compute storage) and we own the second internal
/// ping-pong target. The shared buffer is supplied to step / readback
/// methods as a reference so we don't have to clone wgpu::Buffer (which
/// isn't Clone in wgpu 23).
enum PositionsStorage {
    Owned {
        pos_a: wgpu::Buffer,
        pos_b: wgpu::Buffer,
    },
    /// Marker variant — the actual shared `pos_a` is passed in to each
    /// method that needs it. We still own the internal `pos_b` ping-pong.
    Borrowed {
        pos_b: wgpu::Buffer,
    },
}

impl PositionsStorage {
    fn pos_b(&self) -> &wgpu::Buffer {
        match self {
            PositionsStorage::Owned { pos_b, .. } | PositionsStorage::Borrowed { pos_b, .. } => {
                pos_b
            }
        }
    }
}

struct GpuState {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    positions: PositionsStorage,
    /// True while pos_a is the "in" and pos_b is the "out" buffer.
    a_is_in: bool,
    velocities: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    edge_neighbors: wgpu::Buffer,
    /// Hub-aware (Tigr) virtual-vertex CSR + per-virtual spring partials.
    /// Built once at GpuState init from `edge_offsets`/`edge_neighbors`. See
    /// `HUB_THRESHOLD` and `spring_step` in shaders/force.wgsl.
    /// Packed `node_to_virt_offsets` (length n+1) + `virt_real_idx`
    /// (length n_virtual) — one storage binding instead of two so the
    /// pipeline fits Chrome WebGPU's per-stage cap of 10. See the
    /// matching layout note in `build_aux_buffers`.
    virt_csr_buf: wgpu::Buffer,
    virt_edge_offsets_buf: wgpu::Buffer,
    spring_force_partial_buf: wgpu::Buffer,
    n_virtual: u32,
    spring_bind_group_layout: wgpu::BindGroupLayout,
    spring_pipeline: wgpu::ComputePipeline,
    params_buf: wgpu::Buffer,
    /// Legacy per-node mass storage buffer (1 + log2(degree)). Kept
    /// allocated for layout stability; the shader now reads mass from
    /// positions[i].w (the commit that reduced the per-stage storage-buffer
    /// count to fit Chrome WebGPU's cap of 10).
    #[allow(dead_code)]
    mass_buf: wgpu::Buffer,
    /// Per-node KE proxy = |vel|^2 written by the shader; CPU reads back
    /// (small) for energy_threshold checks.
    energy_buf: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
    /// Barnes-Hut octree storage (the OctNode array the force kernel walks).
    /// Sized for ≤ 2N+16 slots; populated entirely on the GPU by
    /// `octree.encode_build` (shaders/octree.wgsl), never CPU-side.
    oct_nodes_buf: wgpu::Buffer,
    /// Group(1) BGL referenced by `force_step` whenever BH mode is
    /// active. Bound in every mode (the shader has the binding declared,
    /// so it must be present in the bind group) — the non-BH paths just
    /// never read it.
    oct_bind_group_layout: wgpu::BindGroupLayout,
    /// Octree node capacity (== oct_nodes length in nodes). Written into
    /// `params.n_octree` in Barnes-Hut mode as the rope walk cap; the exact
    /// GPU-computed node count lives in the octree's oct_aux[0].
    oct_capacity_nodes: u32,
    /// Fully-GPU Barnes-Hut octree build (scratch buffers + pipelines).
    /// Records its dispatches into the caller's encoder each BH call.
    octree: Octree,
    /// Staging buffer for CPU readback. Only allocated in the owned path
    /// and on-demand for the borrowed path's `read_back_positions`.
    staging: Option<wgpu::Buffer>,

    n_nodes: u32,
    n_edges: u32,
    pos_buf_size: u64,

    /// Initial (CPU-built) positions, kept around so the borrowed-mode path
    /// can seed the shared buffer via `queue.write_buffer` after init.
    initial_positions: Vec<f32>,


    /// Stable node-id ordering used to interpret the position buffer.
    node_order: Option<Vec<String>>,

    /// Effective damping currently in use; cooled per call.
    effective_damping: f32,

    /// Async energy-readback state. Shared with the wgpu map_async callback.
    /// On native, drained inside `step_with_encoder` after `device.poll(Poll)`;
    /// on WASM, the browser drives the callback between rAF ticks.
    energy_readback: Arc<Mutex<EnergyReadback>>,
    /// Fine CSR directed-slot count (`edge_offsets[n]`); sizes the device
    /// multilevel coarsening scratch without a host readback.
    fine_directed_slots: u32,
    /// Retained device multilevel seed (built on `SeedMode::GpuMultilevel`).
    /// Kept resident so the readback-only test helper can report level sizes;
    /// its scratch is ~1.5x the fine node arrays plus ~1x the fine edge arrays
    /// (see `gpu_multilevel`). `None` for every other seed mode.
    multilevel: Option<super::gpu_multilevel::GpuMultilevel>,
}

/// CPU-side pre-compute: stable id ordering, initial positions
/// (padded vec4 layout), velocities, and CSR adjacency arrays.
/// Threshold above which a vertex is split into multiple "virtual vertices"
/// for the spring kernel (Tigr, ASPLOS'18 §3.1). On a power-law graph this
/// is the difference between one hub stalling its whole warp and the warp
/// finishing in O(HUB_THRESHOLD) time.
const HUB_THRESHOLD: u32 = 32;

struct PreCompute {
    n_nodes: u32,
    n_edges: u32,
    node_order: Option<Vec<String>>,
    initial_positions: Vec<f32>, // padded vec4-per-node
    velocities: Vec<f32>,
    edge_offsets: Vec<u32>,
    edge_neighbors: Vec<u32>,
    /// Per-node mass = 1 + log2(degree). Hubs end up heavier.
    mass: Vec<f32>,
    /// Virtual-vertex CSR (Tigr) for the hub-aware spring kernel.
    n_virtual: u32,
    virt_real_idx: Vec<u32>,
    virt_edge_offsets: Vec<u32>,
    node_to_virt_offsets: Vec<u32>,
}

/// Seed initial node positions in flat `[x, y, z]` form (three floats per
/// node) for the given [`SeedMode`]. `edges` is the index-based undirected
/// edge list `[s0, t0, s1, t1, ...]`; the TopoFisheye seeder filters
/// self-loops and out-of-range endpoints. `None` yields zeros (the caller
/// supplies meaningful positions separately).
fn seed_positions_flat(
    n_nodes: u32,
    edges: &[u32],
    seed_mode: &SeedMode,
    spring_len: f32,
) -> Vec<f32> {
    let n = n_nodes as usize;
    let radius = ((n_nodes as f32).max(1.0).sqrt()) * 5.0;
    let mut seeded: Vec<f32> = match seed_mode {
        SeedMode::Random => {
            let mut s: u32 = 0x9E37_79B1;
            let mut next = || {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s as f32 / u32::MAX as f32) * 2.0 - 1.0
            };
            (0..n_nodes)
                .flat_map(|_| [next() * radius, next() * radius, next() * radius])
                .collect()
        }
        SeedMode::TopoFisheye => {
            // Build a flat undirected edge list, filtering self-loops and
            // out-of-range endpoints exactly as the historical path did.
            let mut fe: Vec<u32> = Vec::with_capacity(edges.len());
            let mut k = 0;
            while k + 1 < edges.len() {
                let s = edges[k];
                let t = edges[k + 1];
                k += 2;
                if s == t || s as usize >= n || t as usize >= n {
                    continue;
                }
                fe.push(s);
                fe.push(t);
            }
            crate::layout::topo_fisheye::seed_positions(
                n,
                &fe,
                spring_len.max(1.0),
                0x9E37_79B1,
                &crate::layout::topo_fisheye::CoarsenParams::default(),
            )
        }
        // No generated seed: zeros as a base. The caller either supplies
        // meaningful positions (which override these) or accepts zeros.
        SeedMode::None => vec![0.0f32; 3 * n],
        // Device multilevel seed overwrites xyz on the GPU; host seeds zeros.
        SeedMode::GpuMultilevel => vec![0.0f32; 3 * n],
    };
    // Defensive: if the seeder returned the wrong length (e.g. empty graph
    // edge case), fall back to a zero ball so downstream sizing stays sane.
    if seeded.len() != 3 * n {
        seeded = vec![0.0f32; 3 * n];
    }
    seeded
}

/// Index-based pre-compute: the real work behind [`precompute`], with no
/// string ids and no [`Graph`]. `edges` is the undirected edge list
/// `[s0, t0, s1, t1, ...]` (each edge once, indices `< n_nodes`); self-loops
/// and out-of-range endpoints are skipped. `positions`, when `Some`, is a
/// flat `[x, y, z]` triple per node that overrides the seeder for every
/// node; when `None` the seeder runs per `seed_mode`. `node_order` is `None`
/// on this path (there are no ids to map back through).
fn precompute_csr(
    n_nodes: u32,
    edges: &[u32],
    positions: Option<&[f32]>,
    seed_mode: &SeedMode,
    spring_len: f32,
) -> PreCompute {
    let n = n_nodes as usize;
    // Base flat xyz: caller-supplied positions override the seeder wholesale.
    let base: Vec<f32> = match positions {
        Some(p) if p.len() == 3 * n => p.to_vec(),
        Some(p) => {
            // Wrong length: use what fits, zero-fill the rest.
            let mut v = vec![0.0f32; 3 * n];
            let m = p.len().min(3 * n);
            v[..m].copy_from_slice(&p[..m]);
            v
        }
        None => seed_positions_flat(n_nodes, edges, seed_mode, spring_len),
    };

    // Expand to vec4-padded `[x, y, z, 0]`; mass is injected into .w below.
    let mut positions_v: Vec<f32> = Vec::with_capacity(n * 4);
    for i in 0..n {
        positions_v.extend_from_slice(&[base[3 * i], base[3 * i + 1], base[3 * i + 2], 0.0]);
    }
    let velocities: Vec<f32> = vec![0.0; n * 4];

    // CSR adjacency (undirected: each edge contributes both directions).
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut k = 0;
    while k + 1 < edges.len() {
        let s = edges[k];
        let t = edges[k + 1];
        k += 2;
        if s == t || s as usize >= n || t as usize >= n {
            continue;
        }
        adj[s as usize].push(t);
        adj[t as usize].push(s);
    }
    let mut edge_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut edge_neighbors: Vec<u32> = Vec::new();
    let mut acc: u32 = 0;
    edge_offsets.push(0);
    for ns in &adj {
        acc += ns.len() as u32;
        edge_neighbors.extend_from_slice(ns);
        edge_offsets.push(acc);
    }
    if edge_neighbors.is_empty() {
        edge_neighbors.push(0);
    }
    let mass: Vec<f32> = adj
        .iter()
        .map(|ns| 1.0 + ((ns.len() as f32).max(1.0)).log2())
        .collect();
    // Pack mass into positions[i].w. The WGSL `force_step` reads mass
    // from the .w slot of each vec4 position so the standalone `mass`
    // storage buffer can be dropped — see force.wgsl bindings 0/1 note
    // for context (per-stage storage-buffer cap fits when this is
    // packed). Mass values never change after this, so a single inject
    // here is enough; the shader preserves .w on every position write.
    for (i, m) in mass.iter().enumerate() {
        let w_off = 4 * i + 3;
        if w_off < positions_v.len() {
            positions_v[w_off] = *m;
        }
    }
    let mass = if mass.is_empty() { vec![1.0f32] } else { mass };

    // ---- Virtual-vertex CSR (Tigr) -----------------------------------------
    // Each real vertex i contributes max(1, ceil(deg/HUB_THRESHOLD)) virtual
    // vertices. The min-1 invariant keeps `node_to_virt_offsets` strictly
    // monotonic so the gather loop in `force_step` is one trivial iteration
    // for isolated nodes.
    //
    // virt_edge_offsets is the CSR-style offset array for virtual vertices:
    //   virt_edge_offsets[v]   = first edge_neighbor index for virtual v
    //   virt_edge_offsets[v+1] = one past last edge_neighbor index for v
    // At a real-vertex boundary we patch the last entry so v+1's start
    // equals the new real vertex's chunk start (covers the degree-0 case).
    let mut virt_real_idx: Vec<u32> = Vec::with_capacity(n);
    let mut virt_edge_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut node_to_virt_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    virt_edge_offsets.push(0);
    node_to_virt_offsets.push(0);
    let mut virt_count: u32 = 0;
    for i in 0..n {
        let start = edge_offsets[i];
        let end = edge_offsets[i + 1];
        let deg = end - start;
        let chunks = ((deg + HUB_THRESHOLD - 1) / HUB_THRESHOLD).max(1);
        for c in 0..chunks {
            let chunk_start = start + c * HUB_THRESHOLD;
            let chunk_end = (chunk_start + HUB_THRESHOLD).min(end);
            virt_real_idx.push(i as u32);
            let last_idx = virt_edge_offsets.len() - 1;
            if virt_edge_offsets[last_idx] != chunk_start {
                virt_edge_offsets[last_idx] = chunk_start;
            }
            virt_edge_offsets.push(chunk_end);
            virt_count += 1;
        }
        node_to_virt_offsets.push(virt_count);
    }
    if virt_real_idx.is_empty() {
        virt_real_idx.push(0);
    }
    if virt_edge_offsets.len() < 2 {
        virt_edge_offsets.clear();
        virt_edge_offsets.push(0);
        virt_edge_offsets.push(0);
    }
    if node_to_virt_offsets.len() < 2 {
        node_to_virt_offsets.clear();
        node_to_virt_offsets.push(0);
        node_to_virt_offsets.push(0);
    }
    let n_virtual = virt_count.max(1);

    PreCompute {
        n_nodes,
        n_edges: (edges.len() / 2) as u32,
        node_order: None,
        initial_positions: positions_v,
        velocities,
        edge_offsets,
        edge_neighbors,
        mass,
        n_virtual,
        virt_real_idx,
        virt_edge_offsets,
        node_to_virt_offsets,
    }
}

/// Graph adapter over [`precompute_csr`]: builds the id-sorted node order and
/// an index-based edge list, seeds positions, and applies per-node
/// `position3` overrides. Faithful to the historical per-node semantics —
/// each node with an author-supplied `position3` overrides the seeder for
/// its slot, every other node keeps the seeded value — implemented by
/// seeding a base with `seed_positions_flat` and passing the overridden
/// vector as `positions: Some(..)` (approach (b)). This is the only entry
/// point that touches [`Graph`]; the CSR path never allocates strings.
fn precompute(graph: &Graph, seed_mode: &SeedMode, spring_len: f32) -> PreCompute {
    let n_nodes = graph.nodes.len() as u32;
    let n = n_nodes as usize;

    let mut node_order: Vec<String> = graph.nodes.keys().cloned().collect();
    node_order.sort();
    let id_to_idx: std::collections::HashMap<&str, u32> = node_order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i as u32))
        .collect();

    // Flat undirected edge list in id-sorted index space. Dangling edges
    // (endpoint not present) are dropped here; self-loops are carried through
    // and filtered inside `precompute_csr` (matching the historical path).
    let mut edges: Vec<u32> = Vec::with_capacity(graph.edges.len() * 2);
    for e in graph.edges.values() {
        let (Some(&s), Some(&t)) = (
            id_to_idx.get(e.source.as_str()),
            id_to_idx.get(e.target.as_str()),
        ) else {
            continue;
        };
        edges.push(s);
        edges.push(t);
    }

    // Seed a base, then override each node's slot from its `position3`.
    let seeded = seed_positions_flat(n_nodes, &edges, seed_mode, spring_len);
    let mut positions: Vec<f32> = vec![0.0f32; 3 * n];
    for (idx, id) in node_order.iter().enumerate() {
        let p = graph.nodes[id].position3.unwrap_or([
            seeded[3 * idx],
            seeded[3 * idx + 1],
            seeded[3 * idx + 2],
        ]);
        positions[3 * idx] = p[0];
        positions[3 * idx + 1] = p[1];
        positions[3 * idx + 2] = p[2];
    }

    let mut pc = precompute_csr(n_nodes, &edges, Some(&positions), seed_mode, spring_len);
    // Preserve the historical `n_edges` (raw edge count, including any
    // self-loops / dangling edges dropped above) so the owned/borrowed
    // rebuild heuristics stay stable, and record the id order for write-back.
    pc.n_edges = graph.edges.len() as u32;
    pc.node_order = Some(node_order);
    pc
}


struct ForcePipelines {
    force_step: wgpu::ComputePipeline,
    force_bgl: wgpu::BindGroupLayout,
    /// Group(1) for force_step: the octree storage buffer. Bound in every
    /// repulsion mode (WGSL requires every declared binding to be
    /// present); the non-BH paths simply don't touch it.
    oct_bgl: wgpu::BindGroupLayout,
    /// Group(2): hub-aware (Tigr) virtual-vertex CSR + per-virtual spring
    /// partials. Bound by both `spring_step` and `force_step`.
    spring_bgl: wgpu::BindGroupLayout,
    /// Standalone hub-aware spring kernel (one thread per virtual vertex).
    spring_step: wgpu::ComputePipeline,
}

fn build_pipeline(device: &wgpu::Device) -> ForcePipelines {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("force.wgsl"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "shaders/force.wgsl"
        ))),
    });
    let bgl_entries = [
        storage_entry(0, true),
        storage_entry(1, false),
        storage_entry(2, false),
        storage_entry(3, true),
        storage_entry(4, true),
        wgpu::BindGroupLayoutEntry {
            binding: 5,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        // Binding 6 = energy_out. Mass is packed into positions[i].w (see
        // force.wgsl preamble + precompute below), so this is the whole
        // group: 5 storage + 1 uniform.
        storage_entry(6, false),
    ];
    let bind_group_layout = device.create_bind_group_layout(
        &wgpu::BindGroupLayoutDescriptor {
            label: Some("gpu_force_bgl"),
            entries: &bgl_entries,
        },
    );

    // Group 1: octree storage. Single read-only storage buffer at @binding(1)
    // matching `oct_nodes` in force.wgsl. We omit the params/bbox bindings
    // (only used by the v2 GPU build kernels) — force_step doesn't reference
    // them, so leaving them out of the BGL keeps the layout minimal.
    let oct_bgl_entries = [
        storage_entry(1, true), // oct_nodes (read-only in force_step)
    ];
    let oct_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_octree_bgl"),
        entries: &oct_bgl_entries,
    });

    // Group 2: hub-aware virtual-vertex CSR + per-virtual spring partials.
    // Bound by both `spring_step` (writes partials) and `force_step` (reads).
    let spring_bgl_entries = [
        // virt_csr packs `node_to_virt_offsets` (length n+1) followed by
        // `virt_real_idx` (length n_virtual). Saves one storage-binding
        // slot vs the previous separate buffers, getting the per-stage
        // count under Chrome WebGPU's cap of 10.
        storage_entry(0, true),  // virt_csr (read)
        storage_entry(1, true),  // virt_edge_offsets (read)
        storage_entry(2, false), // spring_force_partial (rw)
    ];
    let spring_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_spring_bgl"),
        entries: &spring_bgl_entries,
    });

    // Both kernels share one layout: group 0 = main, group 1 = octree,
    // group 2 = hub-aware spring partials.
    let pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_pl"),
            bind_group_layouts: &[&bind_group_layout, &oct_bgl, &spring_bgl],
            push_constant_ranges: &[],
        },
    );
    let mk = |name: &'static str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(name),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some(name),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let force_step = mk("force_step");
    let spring_step = mk("spring_step");

    ForcePipelines {
        force_step,
        force_bgl: bind_group_layout,
        oct_bgl,
        spring_bgl,
        spring_step,
    }
}

impl GpuState {
    /// Build state with caller-supplied device + owned positions buffers.
    fn new_owned(device: &wgpu::Device, pc: PreCompute) -> Result<Self, String> {
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        let pos_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_a"),
            contents: bytemuck::cast_slice(nonempty_f32(&pc.initial_positions)),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_b"),
            contents: bytemuck::cast_slice(nonempty_f32(&pc.initial_positions)),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let aux = build_aux_buffers(device, &pc);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions_staging"),
            size: pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let pipelines = build_pipeline(device);

        Ok(Self::assemble(
            device,
            pipelines,
            PositionsStorage::Owned { pos_a, pos_b },
            aux,
            Some(staging),
            pc,
            pos_buf_size,
        ))
    }

    /// Shared tail of `new_owned` / `new_borrowed`: everything after the
    /// position buffers and pipelines exist.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        device: &wgpu::Device,
        pipelines: ForcePipelines,
        positions: PositionsStorage,
        aux: AuxBuffers,
        staging: Option<wgpu::Buffer>,
        pc: PreCompute,
        pos_buf_size: u64,
    ) -> Self {
        let oct_capacity_nodes =
            (aux.oct_nodes_capacity / std::mem::size_of::<OctNodeRaw>() as u64) as u32;
        let octree = Octree::new(device, pc.n_nodes, oct_capacity_nodes);
        Self {
            pipeline: pipelines.force_step,
            bind_group_layout: pipelines.force_bgl,
            positions,
            a_is_in: true,
            velocities: aux.vel,
            edge_offsets: aux.off,
            edge_neighbors: aux.neigh,
            virt_csr_buf: aux.virt_csr,
            virt_edge_offsets_buf: aux.virt_edge_offsets,
            spring_force_partial_buf: aux.spring_force_partial,
            n_virtual: aux.n_virtual,
            spring_bind_group_layout: pipelines.spring_bgl,
            spring_pipeline: pipelines.spring_step,
            params_buf: aux.params,
            mass_buf: aux.mass,
            energy_buf: aux.energy,
            energy_staging: aux.energy_staging,
            oct_nodes_buf: aux.oct_nodes,
            oct_bind_group_layout: pipelines.oct_bgl,
            oct_capacity_nodes,
            octree,
            staging,
            n_nodes: pc.n_nodes,
            n_edges: pc.n_edges,
            pos_buf_size,
            initial_positions: pc.initial_positions,
            node_order: pc.node_order,
            effective_damping: 1.0,
            energy_readback: Arc::new(Mutex::new(EnergyReadback::Idle)),
            fine_directed_slots: pc.edge_offsets.last().copied().unwrap_or(0),
            multilevel: None,
        }
    }

    /// Build state against caller-supplied device + a borrowed positions
    /// storage buffer (typically owned by the renderer). We don't take the
    /// queue here — the caller passes it to `upload_initial_positions` and
    /// `step_with_encoder`. This avoids cloning wgpu::Queue.
    fn new_borrowed(
        device: &wgpu::Device,
        positions_buffer: &wgpu::Buffer,
        pc: PreCompute,
    ) -> Result<Self, String> {
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        // Internal ping-pong target. COPY_SRC so we can copy back to the
        // shared buffer; COPY_DST so we can seed it.
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_internal_b"),
            contents: bytemuck::cast_slice(nonempty_f32(&pc.initial_positions)),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let aux = build_aux_buffers(device, &pc);
        let pipelines = build_pipeline(device);

        let _ = positions_buffer; // sized check happens via caller usage

        Ok(Self::assemble(
            device,
            pipelines,
            PositionsStorage::Borrowed { pos_b },
            aux,
            None,
            pc,
            pos_buf_size,
        ))
    }

    /// Seed the shared (borrowed) positions buffer with our initial values.
    /// Caller must supply the same shared buffer that was passed to
    /// `new_borrowed`.
    fn upload_initial_positions_to(&self, queue: &wgpu::Queue, shared: &wgpu::Buffer) {
        queue.write_buffer(shared, 0, bytemuck::cast_slice(&self.initial_positions));
    }

    /// Run the device multilevel coarsening seed against a borrowed shared
    /// positions buffer, writing the final fine positions into `fine_pos`.
    fn run_multilevel_seed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fine_pos: &wgpu::Buffer,
        options: &GpuForceOptions,
    ) {
        let ml = super::gpu_multilevel::GpuMultilevel::new(device, self.n_nodes, self.fine_directed_slots);
        ml.seed(
            device,
            queue,
            &self.pipeline,
            &self.bind_group_layout,
            &self.oct_bind_group_layout,
            &self.spring_bind_group_layout,
            &self.oct_nodes_buf,
            fine_pos,
            &self.edge_offsets,
            &self.edge_neighbors,
            &self.virt_csr_buf,
            &self.virt_edge_offsets_buf,
            self.n_virtual,
            options,
        );
        self.multilevel = Some(ml);
    }

    /// Owned-mode variant: seeds the current "in" position buffer (pos_a).
    fn run_multilevel_seed_owned(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: &GpuForceOptions,
    ) {
        let ml = super::gpu_multilevel::GpuMultilevel::new(device, self.n_nodes, self.fine_directed_slots);
        let PositionsStorage::Owned { pos_a, .. } = &self.positions else {
            return;
        };
        ml.seed(
            device,
            queue,
            &self.pipeline,
            &self.bind_group_layout,
            &self.oct_bind_group_layout,
            &self.spring_bind_group_layout,
            &self.oct_nodes_buf,
            pos_a,
            &self.edge_offsets,
            &self.edge_neighbors,
            &self.virt_csr_buf,
            &self.virt_edge_offsets_buf,
            self.n_virtual,
            options,
        );
        self.multilevel = Some(ml);
    }

    fn write_params(&self, queue: &wgpu::Queue, opts: &GpuForceOptions, step_index: u32) {
        let raw = SimParamsRaw {
            repulsion: opts.repulsion,
            spring_k: opts.spring_k,
            spring_len: opts.spring_len,
            gravity: opts.gravity,
            damping: self.effective_damping,
            dt: opts.dt,
            cursor_radius: opts.cursor_radius,
            cursor_strength: opts.cursor_strength,
            cursor_pos: opts.cursor_pos,
            n_nodes: self.n_nodes,
            n_edges: self.n_edges,
            repulsion_radius: opts.repulsion_radius,
            repulsion_mode: opts.repulsion_mode.as_u32(),
            bh_theta: opts.theta.clamp(0.1, 2.0),
            // Rope walk cap for the force kernel. In BH mode this is the
            // octree node capacity (a static upper bound); the rope always
            // terminates at OCT_END within the exact GPU-computed count.
            n_octree: match opts.repulsion_mode {
                RepulsionMode::BarnesHut => self.oct_capacity_nodes,
                _ => 0,
            },
            repulsion_samples: opts.repulsion_samples.max(1),
            step_index,
            force_model: opts.force_model.as_u32(),
            tfdp_alpha: opts.tfdp_alpha.max(0.0),
            tfdp_beta: opts.tfdp_beta.max(0.0),
            tfdp_gamma: opts.tfdp_gamma.max(0.5),
            tfdp_k: opts.tfdp_k.max(0.0),
        };
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&raw));
    }

    /// Owned-mode "in/out" picker — both buffers live in PositionsStorage::Owned.
    fn owned_in_out(&self) -> (&wgpu::Buffer, &wgpu::Buffer) {
        let PositionsStorage::Owned { pos_a, pos_b } = &self.positions else {
            panic!("owned_in_out called on borrowed state");
        };
        if self.a_is_in {
            (pos_a, pos_b)
        } else {
            (pos_b, pos_a)
        }
    }

    /// Borrowed-mode "in/out" picker. The shared buffer (pos_a) is supplied
    /// by the caller; the internal pos_b lives in the state.
    fn borrowed_in_out<'a>(
        &'a self,
        shared: &'a wgpu::Buffer,
    ) -> (&'a wgpu::Buffer, &'a wgpu::Buffer) {
        let pos_b = self.positions.pos_b();
        if self.a_is_in {
            (shared, pos_b)
        } else {
            (pos_b, shared)
        }
    }

    fn make_bind_group(
        &self,
        device: &wgpu::Device,
        pos_in: &wgpu::Buffer,
        pos_out: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                buf_entry(0, pos_in),
                buf_entry(1, pos_out),
                buf_entry(2, &self.velocities),
                buf_entry(3, &self.edge_offsets),
                buf_entry(4, &self.edge_neighbors),
                buf_entry(5, &self.params_buf),
                buf_entry(6, &self.energy_buf),
            ],
        })
    }

    /// Direct dispatch — owns its own encoder and submits immediately.
    /// Used by the legacy `run()` path (owned mode only).
    fn dispatch_step_direct(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let (pos_in, pos_out) = self.owned_in_out();
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_cmd"),
        });
        let oct_bg = self.make_oct_bind_group(device);
        let spring_bg = self.make_spring_bind_group(device);
        self.encode_spring_step(&mut encoder, &bind_group, &oct_bg, &spring_bg);
        self.encode_compute(&mut encoder, &bind_group, &oct_bg, &spring_bg);
        queue.submit(Some(encoder.finish()));
    }

    /// Record dispatch into a caller-supplied encoder, reading/writing the
    /// borrowed shared buffer + internal pos_b.
    fn dispatch_borrowed_step(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shared: &wgpu::Buffer,
    ) {
        let (pos_in, pos_out) = self.borrowed_in_out(shared);
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        let oct_bg = self.make_oct_bind_group(device);
        let spring_bg = self.make_spring_bind_group(device);
        self.encode_spring_step(encoder, &bind_group, &oct_bg, &spring_bg);
        self.encode_compute(encoder, &bind_group, &oct_bg, &spring_bg);
    }

    fn encode_compute(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        oct_bg: &wgpu::BindGroup,
        spring_bg: &wgpu::BindGroup,
    ) {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("force_step_pass"),
            timestamp_writes: None,
        });
        cpass.set_pipeline(&self.pipeline);
        cpass.set_bind_group(0, bind_group, &[]);
        cpass.set_bind_group(1, oct_bg, &[]);
        cpass.set_bind_group(2, spring_bg, &[]);
        dispatch_1d(&mut cpass, self.n_nodes);
    }

    fn make_oct_bind_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_oct_bg"),
            layout: &self.oct_bind_group_layout,
            entries: &[buf_entry(1, &self.oct_nodes_buf)],
        })
    }

    fn make_spring_bind_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_spring_bg"),
            layout: &self.spring_bind_group_layout,
            entries: &[
                buf_entry(0, &self.virt_csr_buf),
                buf_entry(1, &self.virt_edge_offsets_buf),
                buf_entry(2, &self.spring_force_partial_buf),
            ],
        })
    }

    /// Record the hub-aware spring kernel — one thread per virtual vertex,
    /// writing per-virtual partials into `spring_force_partial`. Must run
    /// before `force_step` (which gathers the partials).
    fn encode_spring_step(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        oct_bg: &wgpu::BindGroup,
        spring_bg: &wgpu::BindGroup,
    ) {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("spring_step_pass"),
            timestamp_writes: None,
        });
        cpass.set_pipeline(&self.spring_pipeline);
        cpass.set_bind_group(0, bind_group, &[]);
        cpass.set_bind_group(1, oct_bg, &[]);
        cpass.set_bind_group(2, spring_bg, &[]);
        dispatch_1d(&mut cpass, self.n_virtual);
    }

    fn swap_position_buffers(&mut self) {
        self.a_is_in = !self.a_is_in;
    }

    /// If the previous frame's `energy_staging` map_async has completed, read
    /// out the per-node KE values, take their max, unmap the staging buffer,
    /// and reset the readback state to Idle. Returns Some(max_ke) if a value
    /// was consumed this call, None if no completed map was waiting.
    ///
    /// This is the "drain on next frame" half of the deferred-readback pattern
    /// — we never block. If the GPU/driver hasn't finished the map yet we
    /// just report None and try again next frame.
    fn drain_energy_readback(&self) -> Option<f32> {
        // Take the lock briefly to inspect state. We must NOT read the
        // mapped range while holding the mutex, because the buffer view
        // implicitly retains state inside wgpu and we want the lock dropped
        // before we touch wgpu APIs again.
        //
        // We also clear the state here (Done -> Idle) so the next round
        // can be scheduled cleanly. Buffer unmap happens after we drop the
        // lock and finish the read.
        let map_succeeded = {
            let mut guard = self.energy_readback.lock().ok()?;
            match &*guard {
                EnergyReadback::Done(Ok(())) => true,
                EnergyReadback::Done(Err(_e)) => {
                    // Map failures are rare and self-recovering — silently
                    // reset to Idle and try again next frame. No unmap
                    // needed (the buffer was never mapped).
                    *guard = EnergyReadback::Idle;
                    return None;
                }
                _ => return None, // Idle or Mapping: nothing to drain.
            }
        };
        if !map_succeeded {
            return None;
        }
        // Map succeeded — guarded by the variant we just matched. The lock
        // is dropped, so it's safe to enter wgpu again. The staging buffer
        // is mapped; read, reduce, unmap.
        let max = {
            let slice = self.energy_staging.slice(..);
            let view = slice.get_mapped_range();
            let floats: &[f32] = bytemuck::cast_slice(&view);
            let n = (self.n_nodes as usize).min(floats.len());
            let mut m = 0.0f32;
            for &v in &floats[..n] {
                if v.is_finite() && v > m {
                    m = v;
                }
            }
            // Drop the view BEFORE unmap — wgpu requires no outstanding
            // mapped ranges when unmap is called.
            drop(view);
            m
        };
        self.energy_staging.unmap();
        // Now that wgpu state is clean, flip back to Idle so the next
        // step_with_encoder can schedule a fresh readback.
        if let Ok(mut g) = self.energy_readback.lock() {
            *g = EnergyReadback::Idle;
        }
        Some(max)
    }

    /// Record `energy_buf -> energy_staging` copy and schedule the
    /// non-blocking map_async. Safe to call only when state is Idle —
    /// remapping a buffer that's still mapped panics in wgpu. Caller is
    /// responsible for that check.
    fn schedule_energy_copy(
        &self,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let n_bytes = (self.n_nodes as u64) * 4;
        if n_bytes == 0 {
            return;
        }
        encoder.copy_buffer_to_buffer(
            &self.energy_buf,
            0,
            &self.energy_staging,
            0,
            n_bytes,
        );
        // Park in CopyScheduled. We can't issue `map_async` here because
        // the encoder hasn't been submitted yet — on WASM the resulting
        // map happens immediately and that races the not-yet-submitted
        // copy ("Buffer used in submit while mapped"). The next
        // step_with_encoder entry sees CopyScheduled, knows the copy has
        // since been submitted by eframe, and issues the map_async then.
        if let Ok(mut g) = self.energy_readback.lock() {
            *g = EnergyReadback::CopyScheduled;
        }
    }

    /// Issue the `map_async` request on the energy_staging buffer.
    ///
    /// **Re-entrancy contract**: on WASM `map_async` invokes its callback
    /// synchronously from inside the queue submit codepath. The callback
    /// must therefore do nothing but flip the shared state — no wgpu
    /// access (no `get_mapped_range`, no `unmap`, no buffer methods at
    /// all), no allocation that could touch wgpu state. The actual buffer
    /// read happens in `drain_energy_readback` at the top of the *next*
    /// `step_with_encoder`, where no other wgpu code is in flight.
    fn issue_energy_map(&self) {
        // Flip to Mapping *before* we issue map_async. On WASM the
        // callback can fire synchronously inside this call (under the
        // queue submit codepath of an unrelated submit), so the state
        // must already be in Mapping when the callback's Done write lands
        // — otherwise the order Done -> Mapping would clobber the result.
        if let Ok(mut g) = self.energy_readback.lock() {
            *g = EnergyReadback::Mapping;
        }
        let shared = self.energy_readback.clone();
        let slice = self.energy_staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |res| {
            // Only mutate state. Do NOT touch any wgpu API here —
            // re-entering wgpu from inside the callback panics with
            // "Buffer is already mapped" / "recursive use of an object".
            if let Ok(mut g) = shared.lock() {
                *g = EnergyReadback::Done(res);
            }
        });
    }

    /// Owned-mode CPU readback of the latest positions.
    async fn read_positions_owned(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Vec<f32>, String> {
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| "no staging buffer (borrowed mode)".to_string())?;
        let (pos_in, _) = self.owned_in_out();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_readback"),
        });
        encoder.copy_buffer_to_buffer(pos_in, 0, staging, 0, self.pos_buf_size);
        queue.submit(Some(encoder.finish()));
        Self::map_and_read(staging, device).await
    }

    /// Borrowed-mode CPU readback. Allocates a temporary staging buffer.
    async fn read_positions_with_device(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions_readback_tmp"),
            size: self.pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_readback_borrowed"),
        });
        // Latest result lives on the shared buffer after
        // step_with_encoder ensures it's there.
        encoder.copy_buffer_to_buffer(shared, 0, &staging, 0, self.pos_buf_size);
        queue.submit(Some(encoder.finish()));
        Self::map_and_read(&staging, device).await
    }

    async fn map_and_read(staging: &wgpu::Buffer, device: &wgpu::Device) -> Result<Vec<f32>, String> {
        let slice = staging.slice(..);
        let (tx, rx) = futures_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            device.poll(wgpu::Maintain::Wait);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = device;
        let res = rx.recv().await;
        res.map_err(|_| "map channel dropped".to_string())?
            .map_err(|e| format!("buffer map failed: {e:?}"))?;
        let data = slice.get_mapped_range();
        let floats: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(floats)
    }
}

struct AuxBuffers {
    vel: wgpu::Buffer,
    off: wgpu::Buffer,
    neigh: wgpu::Buffer,
    /// Packs `node_to_virt_offsets` (length n+1) + `virt_real_idx`
    /// (length n_virtual) into one u32 buffer. See `build_aux_buffers`.
    virt_csr: wgpu::Buffer,
    virt_edge_offsets: wgpu::Buffer,
    spring_force_partial: wgpu::Buffer,
    n_virtual: u32,
    params: wgpu::Buffer,
    mass: wgpu::Buffer,
    energy: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
    oct_nodes: wgpu::Buffer,
    oct_nodes_capacity: u64,
}

/// Build the velocity, edge_offsets, edge_neighbors, params, mass, grid,
/// and energy buffers used by both owned and borrowed paths.
fn build_aux_buffers(device: &wgpu::Device, pc: &PreCompute) -> AuxBuffers {
    let vel = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("velocities"),
        contents: bytemuck::cast_slice(nonempty_f32(&pc.velocities)),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let off = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_offsets"),
        contents: bytemuck::cast_slice(nonempty_u32(&pc.edge_offsets)),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let neigh = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_neighbors"),
        contents: bytemuck::cast_slice(nonempty_u32(&pc.edge_neighbors)),
        usage: wgpu::BufferUsages::STORAGE,
    });
    // Hub-aware (Tigr) virtual-vertex CSR + per-virtual spring partials.
    //
    // `virt_csr` packs `node_to_virt_offsets` (length n_nodes+1) followed
    // by `virt_real_idx` (length n_virtual) into a single storage buffer
    // — saves one storage-binding slot per stage so the force_step
    // pipeline fits under Chrome's WebGPU per-stage cap of 10.
    //
    // WGSL access pattern:
    //   node_to_virt_offsets[i] → virt_csr[i]
    //   virt_real_idx[v]        → virt_csr[(n_nodes + 1u) + v]
    let mut virt_csr_packed: Vec<u32> =
        Vec::with_capacity(pc.node_to_virt_offsets.len() + pc.virt_real_idx.len());
    virt_csr_packed.extend_from_slice(&pc.node_to_virt_offsets);
    virt_csr_packed.extend_from_slice(&pc.virt_real_idx);
    let virt_csr = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("virt_csr"),
        contents: bytemuck::cast_slice(nonempty_u32(&virt_csr_packed)),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let virt_edge_offsets = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("virt_edge_offsets"),
        contents: bytemuck::cast_slice(nonempty_u32(&pc.virt_edge_offsets)),
        usage: wgpu::BufferUsages::STORAGE,
    });
    // Per-virtual partial spring forces — vec3<f32> stride = 16 bytes.
    let spring_partial_bytes = (pc.n_virtual.max(1) as u64) * 16;
    let spring_force_partial = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spring_force_partial"),
        size: spring_partial_bytes,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sim_params"),
        size: std::mem::size_of::<SimParamsRaw>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mass = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mass"),
        contents: bytemuck::cast_slice(nonempty_f32(&pc.mass)),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let n = pc.n_nodes.max(1) as u64;
    let energy_size = (n * 4).max(64);
    let energy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("energy"),
        size: energy_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let energy_staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("energy_staging"),
        size: energy_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    // Octree storage. Worst case is ~2N nodes (every body forces a leaf
    // subdivision); add small headroom for the root + sentinel slot.
    // OctNodeRaw is 48 bytes (3 vec4s).
    let oct_node_size = std::mem::size_of::<OctNodeRaw>() as u64;
    let oct_capacity_nodes = (pc.n_nodes as u64 * 2 + 16).max(16);
    let oct_nodes_capacity = (oct_capacity_nodes * oct_node_size).max(64);
    let oct_nodes = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oct_nodes"),
        size: oct_nodes_capacity,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    AuxBuffers {
        vel,
        off,
        neigh,
        virt_csr,
        virt_edge_offsets,
        spring_force_partial,
        n_virtual: pc.n_virtual,
        params,
        mass,
        energy,
        energy_staging,
        oct_nodes,
        oct_nodes_capacity,
    }
}

/// Compute workgroup size shared by every 1-D kernel in force.wgsl
/// (`@workgroup_size(64)`).
const WORKGROUP_SIZE: u32 = 64;

/// WebGPU's `maxComputeWorkgroupsPerDimension` (spec default and the
/// value `wgpu::Limits::downlevel_defaults()` requests). A 1-D dispatch
/// of `ceil(n / 64)` groups therefore validates only up to
/// 65535 × 64 = 4 194 240 invocations; above that the pass is rejected.
const MAX_WORKGROUPS_PER_DIM: u32 = 65535;

/// Dispatch `invocations` lanes of a 64-wide kernel, spilling into the Y
/// dimension once X would exceed the per-dimension cap. Kernels recover
/// their linear index via `linear_index()` in force.wgsl
/// (`gid.x + gid.y * num_workgroups.x * 64`).
fn dispatch_1d(cpass: &mut wgpu::ComputePass<'_>, invocations: u32) {
    let (x, y) = dispatch_grid(invocations);
    cpass.dispatch_workgroups(x, y, 1);
}

/// `(x, y)` workgroup counts for `dispatch_1d`. Split out so the
/// arithmetic is unit-testable without a device.
pub(crate) fn dispatch_grid(invocations: u32) -> (u32, u32) {
    let groups = invocations.div_ceil(WORKGROUP_SIZE).max(1);
    let x = groups.min(MAX_WORKGROUPS_PER_DIM);
    let y = groups.div_ceil(x);
    (x, y)
}

/// Device limits the force engine needs, derived from what the adapter
/// offers.
///
/// `wgpu::Limits::downlevel_defaults()` pins `max_storage_buffer_binding_size`
/// to 128 MiB and `max_buffer_size` to 256 MiB (the WebGPU spec defaults);
/// `using_resolution` only lifts texture dimensions. At 16 bytes per node
/// that caps the position buffers at 8 388 608 nodes and the CSR neighbour
/// buffer at 16.7 M undirected edges regardless of how much VRAM the GPU
/// has. Ask for the adapter's actual buffer limits instead; everything else
/// stays at the downlevel baseline so the same request validates in
/// browsers. Shared by the owned `run()` path here and the renderer-owned
/// device in `app/ui`.
pub fn gpu_force_device_limits(adapter: &wgpu::Limits) -> wgpu::Limits {
    let base = wgpu::Limits::downlevel_defaults().using_resolution(adapter.clone());
    wgpu::Limits {
        max_storage_buffers_per_shader_stage: base
            .max_storage_buffers_per_shader_stage
            .max(8)
            .min(adapter.max_storage_buffers_per_shader_stage),
        max_storage_buffer_binding_size: adapter
            .max_storage_buffer_binding_size
            .max(base.max_storage_buffer_binding_size),
        max_buffer_size: adapter.max_buffer_size.max(base.max_buffer_size),
        ..base
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn buf_entry(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}

// ---- minimal one-shot oneshot channel that's Send + works on wasm32 -------
//
// We avoid pulling in `futures` just for `oneshot`. This is enough for the
// "buffer map completed" callback path. The receiver is async and yields
// once the value arrives; on wasm32 the browser's microtask queue drives it,
// on native `device.poll(Wait)` runs the callback synchronously before we
// hit recv().

fn futures_channel() -> (OneshotTx, OneshotRx) {
    let inner = std::sync::Arc::new(OneshotInner {
        slot: std::sync::Mutex::new(None),
    });
    (
        OneshotTx {
            inner: inner.clone(),
        },
        OneshotRx { inner },
    )
}

struct OneshotInner {
    slot: std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>,
}

struct OneshotTx {
    inner: std::sync::Arc<OneshotInner>,
}
impl OneshotTx {
    fn send(self, v: Result<(), wgpu::BufferAsyncError>) {
        if let Ok(mut slot) = self.inner.slot.lock() {
            *slot = Some(v);
        }
    }
}

struct OneshotRx {
    inner: std::sync::Arc<OneshotInner>,
}
impl OneshotRx {
    async fn recv(self) -> Result<Result<(), wgpu::BufferAsyncError>, ()> {
        // Spin-yield until the slot is populated. On native, by the time we
        // arrive here `device.poll(Wait)` has already run the callback. On
        // wasm32 we yield to the event loop until the GPU job completes.
        loop {
            if let Some(v) = self.inner.slot.lock().map_err(|_| ())?.take() {
                return Ok(v);
            }
            yield_now().await;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn yield_now() {
    // No real async runtime assumed — just a single std::thread yield.
    // device.poll(Wait) means the callback already fired; this loop runs at
    // most a couple of times.
    std::thread::yield_now();
    // Cooperate with async runtimes by going through a manual yield future.
    YieldOnce { polled: false }.await;
}

#[cfg(target_arch = "wasm32")]
async fn yield_now() {
    YieldOnce { polled: false }.await;
}

struct YieldOnce {
    polled: bool,
}
impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.polled {
            std::task::Poll::Ready(())
        } else {
            self.polled = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

// ---------- Barnes-Hut octree (fully-GPU build) -----------------------------
//
// On-wire node layout, shared with `OctNode` in force.wgsl / octree.wgsl:
//   pos_size: (cx, cy, cz, half_extent)
//   com_mass: (com_x, com_y, com_z, mass)
//   meta:     (body_idx | OCT_BODY_INTERNAL, next_idx, skip_idx, child_count)
//
// next_idx / skip_idx form the stackless rope: next is the first child in
// DFS order (or OCT_END for leaves); skip is the next-sibling-or-uncle.
// Sentinel OCT_END = u32::MAX terminates the traversal.
//
// The tree is built entirely on the GPU (shaders/octree.wgsl); the host does
// no readback and no CPU tree walk. `OctNodeRaw` here only sizes the buffer
// and lets tests interpret the readback.
#[cfg(test)]
const OCT_END: u32 = u32::MAX;
#[cfg(test)]
const OCT_BODY_INTERNAL: u32 = u32::MAX;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct OctNodeRaw {
    pos_size: [f32; 4],
    com_mass: [f32; 4],
    meta: [u32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct OctBuildParamsRaw {
    n: u32,
    n_blocks: u32,
    cap: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanDimsRaw {
    len: u32,
    nblocks: u32,
    _pad0: u32,
    _pad1: u32,
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn oct_bgl(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    })
}

fn oct_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    entries: &[wgpu::BindGroupEntry],
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("octree_bg"),
        layout,
        entries,
    })
}

/// Record one 64-lane kernel dispatch in its own compute pass. Per-pass
/// boundaries give wgpu the storage-buffer barriers between build stages.
fn oct_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipe: &wgpu::ComputePipeline,
    bg0: &wgpu::BindGroup,
    bg1: &wgpu::BindGroup,
    x: u32,
    y: u32,
) {
    let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("octree_build_pass"),
        timestamp_writes: None,
    });
    cp.set_pipeline(pipe);
    cp.set_bind_group(0, bg0, &[]);
    cp.set_bind_group(1, bg1, &[]);
    cp.dispatch_workgroups(x, y, 1);
}

/// Fully-GPU Barnes-Hut octree build. Owns every scratch buffer and pipeline
/// (allocated once at construction) and records its dispatches into a
/// caller-supplied encoder each Barnes-Hut call. See shaders/octree.wgsl.
struct Octree {
    // Dispatch extents (body count, histogram length, n+1).
    n: u32,
    hist_len: u32,
    p1: u32,

    // Scratch buffers.
    bbox: wgpu::Buffer,
    world: wgpu::Buffer,
    keys_a: wgpu::Buffer,
    keys_b: wgpu::Buffer,
    ids_a: wgpu::Buffer,
    ids_b: wgpu::Buffer,
    histogram: wgpu::Buffer,
    com_prefix: wgpu::Buffer,
    flags: wgpu::Buffer,
    node_base: wgpu::Buffer,
    /// [0] = node count; [1 + i] = sorted rank of real body i. Readable by
    /// tests; not bound into force_step (Chrome per-stage buffer budget).
    oct_aux: wgpu::Buffer,
    block_sums_u32: wgpu::Buffer,
    block_sums_v4: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    dims_hist_buf: wgpu::Buffer,
    dims_p1_buf: wgpu::Buffer,

    // Group-1 bind group layouts.
    bgl_params: wgpu::BindGroupLayout,
    bgl_dims: wgpu::BindGroupLayout,
    // Group-0 bind group layouts (one per kernel binding-set).
    bgl_bbox_clear: wgpu::BindGroupLayout,
    bgl_bbox_reduce: wgpu::BindGroupLayout,
    bgl_bbox_finalize: wgpu::BindGroupLayout,
    bgl_morton: wgpu::BindGroupLayout,
    bgl_histogram: wgpu::BindGroupLayout,
    bgl_scatter: wgpu::BindGroupLayout,
    bgl_scan_u32: wgpu::BindGroupLayout,
    bgl_scan_serial_u32: wgpu::BindGroupLayout,
    bgl_scan_v4: wgpu::BindGroupLayout,
    bgl_scan_serial_v4: wgpu::BindGroupLayout,
    bgl_com: wgpu::BindGroupLayout,
    bgl_flags: wgpu::BindGroupLayout,
    bgl_ncount: wgpu::BindGroupLayout,
    bgl_emit: wgpu::BindGroupLayout,
    bgl_aux: wgpu::BindGroupLayout,

    // Pipelines.
    p_bbox_clear: wgpu::ComputePipeline,
    p_bbox_reduce: wgpu::ComputePipeline,
    p_bbox_finalize: wgpu::ComputePipeline,
    p_morton: wgpu::ComputePipeline,
    p_histogram: Vec<wgpu::ComputePipeline>,
    p_scatter: Vec<wgpu::ComputePipeline>,
    p_scan_local_u32: wgpu::ComputePipeline,
    p_scan_serial_u32: wgpu::ComputePipeline,
    p_scan_fixup_u32: wgpu::ComputePipeline,
    p_scan_local_v4: wgpu::ComputePipeline,
    p_scan_serial_v4: wgpu::ComputePipeline,
    p_scan_fixup_v4: wgpu::ComputePipeline,
    p_com: wgpu::ComputePipeline,
    p_flags: wgpu::ComputePipeline,
    p_ncount: wgpu::ComputePipeline,
    p_emit: wgpu::ComputePipeline,
    p_aux: wgpu::ComputePipeline,
}

impl Octree {
    fn new(device: &wgpu::Device, n_nodes: u32, cap: u32) -> Self {
        let nn = n_nodes.max(1);
        let n_blocks = nn.div_ceil(WORKGROUP_SIZE);
        let hist_len = 16 * n_blocks;
        let hist_blocks = hist_len.div_ceil(WORKGROUP_SIZE);
        let p1 = nn + 1;
        let p1_blocks = p1.div_ceil(WORKGROUP_SIZE);
        let bs_u32_len = hist_blocks.max(p1_blocks);

        let store = wgpu::BufferUsages::STORAGE;
        let mk = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size.max(4),
                usage,
                mapped_at_creation: false,
            })
        };
        let u4 = 4u64;
        let v16 = 16u64;
        let bbox = mk("oct_bbox", 6 * u4, store);
        let world = mk("oct_world", 2 * v16, store);
        let keys_a = mk("oct_keys_a", nn as u64 * u4, store);
        let keys_b = mk("oct_keys_b", nn as u64 * u4, store);
        let ids_a = mk("oct_ids_a", nn as u64 * u4, store);
        let ids_b = mk("oct_ids_b", nn as u64 * u4, store);
        let histogram = mk("oct_histogram", hist_len as u64 * u4, store);
        let com_prefix = mk("oct_com_prefix", p1 as u64 * v16, store);
        let flags = mk("oct_flags", nn as u64 * u4, store);
        let node_base = mk("oct_node_base", p1 as u64 * u4, store);
        let oct_aux = mk("oct_aux", p1 as u64 * u4, store | wgpu::BufferUsages::COPY_SRC);
        let block_sums_u32 = mk("oct_block_sums_u32", bs_u32_len as u64 * u4, store);
        let block_sums_v4 = mk("oct_block_sums_v4", p1_blocks as u64 * v16, store);

        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("oct_params"),
            contents: bytemuck::bytes_of(&OctBuildParamsRaw {
                n: n_nodes,
                n_blocks,
                cap,
                _pad: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let dims_hist_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("oct_dims_hist"),
            contents: bytemuck::bytes_of(&ScanDimsRaw {
                len: hist_len,
                nblocks: hist_blocks,
                _pad0: 0,
                _pad1: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let dims_p1_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("oct_dims_p1"),
            contents: bytemuck::bytes_of(&ScanDimsRaw {
                len: p1,
                nblocks: p1_blocks,
                _pad0: 0,
                _pad1: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("octree.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/octree.wgsl"))),
        });

        // Group-1 layouts.
        let bgl_params = oct_bgl(device, "oct_g1_params", &[uniform_entry(0)]);
        let bgl_dims = oct_bgl(device, "oct_g1_dims", &[uniform_entry(1)]);
        // Group-0 layouts. positions_in (binding 0) is read-only; every other
        // storage var is declared read_write in the shader, so their layout
        // entries must be read_write too.
        let s = |b: u32| storage_entry(b, false);
        let sr = storage_entry(0, true);
        let bgl_bbox_clear = oct_bgl(device, "oct_bbox_clear", &[s(1)]);
        let bgl_bbox_reduce = oct_bgl(device, "oct_bbox_reduce", &[sr, s(1)]);
        let bgl_bbox_finalize = oct_bgl(device, "oct_bbox_finalize", &[s(1), s(2)]);
        let bgl_morton = oct_bgl(device, "oct_morton", &[storage_entry(0, true), s(2), s(3), s(5)]);
        let bgl_histogram = oct_bgl(device, "oct_histogram", &[s(3), s(7)]);
        let bgl_scatter = oct_bgl(device, "oct_scatter", &[s(3), s(4), s(5), s(6), s(7)]);
        let bgl_scan_u32 = oct_bgl(device, "oct_scan_u32", &[s(13), s(14)]);
        let bgl_scan_serial_u32 = oct_bgl(device, "oct_scan_serial_u32", &[s(14)]);
        let bgl_scan_v4 = oct_bgl(device, "oct_scan_v4", &[s(15), s(16)]);
        let bgl_scan_serial_v4 = oct_bgl(device, "oct_scan_serial_v4", &[s(16)]);
        let bgl_com = oct_bgl(device, "oct_com", &[storage_entry(0, true), s(5), s(8)]);
        let bgl_flags = oct_bgl(device, "oct_flags", &[s(3), s(9)]);
        let bgl_ncount = oct_bgl(device, "oct_ncount", &[s(9), s(10)]);
        let bgl_emit = oct_bgl(device, "oct_emit", &[s(2), s(3), s(5), s(8), s(9), s(10), s(11)]);
        let bgl_aux = oct_bgl(device, "oct_aux_bgl", &[s(5), s(10), s(12)]);

        let mk_pipe = |name: &str, g0: &wgpu::BindGroupLayout, g1: &wgpu::BindGroupLayout, shift: Option<u32>| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[g0, g1],
                push_constant_ranges: &[],
            });
            let mut consts: HashMap<String, f64> = HashMap::new();
            if let Some(sh) = shift {
                consts.insert("PASS_SHIFT".to_string(), sh as f64);
            }
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(name),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &consts,
                    zero_initialize_workgroup_memory: true,
                },
                cache: None,
            })
        };

        let p_bbox_clear = mk_pipe("bbox_clear", &bgl_bbox_clear, &bgl_params, None);
        let p_bbox_reduce = mk_pipe("bbox_reduce", &bgl_bbox_reduce, &bgl_params, None);
        let p_bbox_finalize = mk_pipe("bbox_finalize", &bgl_bbox_finalize, &bgl_params, None);
        let p_morton = mk_pipe("morton_assign", &bgl_morton, &bgl_params, None);
        let mut p_histogram = Vec::with_capacity(8);
        let mut p_scatter = Vec::with_capacity(8);
        for pass in 0u32..8u32 {
            let shift = pass * 4;
            p_histogram.push(mk_pipe("radix_histogram", &bgl_histogram, &bgl_params, Some(shift)));
            p_scatter.push(mk_pipe("radix_scatter", &bgl_scatter, &bgl_params, Some(shift)));
        }
        let p_scan_local_u32 = mk_pipe("scan_local_u32", &bgl_scan_u32, &bgl_dims, None);
        let p_scan_serial_u32 = mk_pipe("scan_serial_u32", &bgl_scan_serial_u32, &bgl_dims, None);
        let p_scan_fixup_u32 = mk_pipe("scan_fixup_u32", &bgl_scan_u32, &bgl_dims, None);
        let p_scan_local_v4 = mk_pipe("scan_local_v4", &bgl_scan_v4, &bgl_dims, None);
        let p_scan_serial_v4 = mk_pipe("scan_serial_v4", &bgl_scan_serial_v4, &bgl_dims, None);
        let p_scan_fixup_v4 = mk_pipe("scan_fixup_v4", &bgl_scan_v4, &bgl_dims, None);
        let p_com = mk_pipe("com_input", &bgl_com, &bgl_params, None);
        let p_flags = mk_pipe("level_flags", &bgl_flags, &bgl_params, None);
        let p_ncount = mk_pipe("node_count", &bgl_ncount, &bgl_params, None);
        let p_emit = mk_pipe("node_emit", &bgl_emit, &bgl_params, None);
        let p_aux = mk_pipe("finalize_aux", &bgl_aux, &bgl_params, None);

        Self {
            n: n_nodes,
            hist_len,
            p1,
            bbox,
            world,
            keys_a,
            keys_b,
            ids_a,
            ids_b,
            histogram,
            com_prefix,
            flags,
            node_base,
            oct_aux,
            block_sums_u32,
            block_sums_v4,
            params_buf,
            dims_hist_buf,
            dims_p1_buf,
            bgl_params,
            bgl_dims,
            bgl_bbox_clear,
            bgl_bbox_reduce,
            bgl_bbox_finalize,
            bgl_morton,
            bgl_histogram,
            bgl_scatter,
            bgl_scan_u32,
            bgl_scan_serial_u32,
            bgl_scan_v4,
            bgl_scan_serial_v4,
            bgl_com,
            bgl_flags,
            bgl_ncount,
            bgl_emit,
            bgl_aux,
            p_bbox_clear,
            p_bbox_reduce,
            p_bbox_finalize,
            p_morton,
            p_histogram,
            p_scatter,
            p_scan_local_u32,
            p_scan_serial_u32,
            p_scan_fixup_u32,
            p_scan_local_v4,
            p_scan_serial_v4,
            p_scan_fixup_v4,
            p_com,
            p_flags,
            p_ncount,
            p_emit,
            p_aux,
        }
    }

    /// Record the whole build into `encoder`, reading `pos_in` and writing
    /// the OctNode array `oct_nodes`. No host readback; all state lives on
    /// the GPU.
    fn encode_build(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pos_in: &wgpu::Buffer,
        oct_nodes: &wgpu::Buffer,
    ) {
        if self.n == 0 {
            return;
        }
        let n = self.n;
        let hl = self.hist_len;
        let p1 = self.p1;

        // Group-1 bind groups.
        let bg_params = oct_bg(device, &self.bgl_params, &[buf_entry(0, &self.params_buf)]);
        let bg_dims_hist = oct_bg(device, &self.bgl_dims, &[buf_entry(1, &self.dims_hist_buf)]);
        let bg_dims_p1 = oct_bg(device, &self.bgl_dims, &[buf_entry(1, &self.dims_p1_buf)]);

        // Group-0 bind groups.
        let bg_bbox_clear = oct_bg(device, &self.bgl_bbox_clear, &[buf_entry(1, &self.bbox)]);
        let bg_bbox_reduce = oct_bg(
            device,
            &self.bgl_bbox_reduce,
            &[buf_entry(0, pos_in), buf_entry(1, &self.bbox)],
        );
        let bg_bbox_finalize = oct_bg(
            device,
            &self.bgl_bbox_finalize,
            &[buf_entry(1, &self.bbox), buf_entry(2, &self.world)],
        );
        let bg_morton = oct_bg(
            device,
            &self.bgl_morton,
            &[
                buf_entry(0, pos_in),
                buf_entry(2, &self.world),
                buf_entry(3, &self.keys_a),
                buf_entry(5, &self.ids_a),
            ],
        );
        let bg_hist_a = oct_bg(
            device,
            &self.bgl_histogram,
            &[buf_entry(3, &self.keys_a), buf_entry(7, &self.histogram)],
        );
        let bg_hist_b = oct_bg(
            device,
            &self.bgl_histogram,
            &[buf_entry(3, &self.keys_b), buf_entry(7, &self.histogram)],
        );
        let bg_scatter_ab = oct_bg(
            device,
            &self.bgl_scatter,
            &[
                buf_entry(3, &self.keys_a),
                buf_entry(4, &self.keys_b),
                buf_entry(5, &self.ids_a),
                buf_entry(6, &self.ids_b),
                buf_entry(7, &self.histogram),
            ],
        );
        let bg_scatter_ba = oct_bg(
            device,
            &self.bgl_scatter,
            &[
                buf_entry(3, &self.keys_b),
                buf_entry(4, &self.keys_a),
                buf_entry(5, &self.ids_b),
                buf_entry(6, &self.ids_a),
                buf_entry(7, &self.histogram),
            ],
        );
        let bg_scan_hist = oct_bg(
            device,
            &self.bgl_scan_u32,
            &[buf_entry(13, &self.histogram), buf_entry(14, &self.block_sums_u32)],
        );
        let bg_scan_nb = oct_bg(
            device,
            &self.bgl_scan_u32,
            &[buf_entry(13, &self.node_base), buf_entry(14, &self.block_sums_u32)],
        );
        let bg_scan_serial_u32 =
            oct_bg(device, &self.bgl_scan_serial_u32, &[buf_entry(14, &self.block_sums_u32)]);
        let bg_scan_com = oct_bg(
            device,
            &self.bgl_scan_v4,
            &[buf_entry(15, &self.com_prefix), buf_entry(16, &self.block_sums_v4)],
        );
        let bg_scan_serial_v4 =
            oct_bg(device, &self.bgl_scan_serial_v4, &[buf_entry(16, &self.block_sums_v4)]);
        let bg_com = oct_bg(
            device,
            &self.bgl_com,
            &[
                buf_entry(0, pos_in),
                buf_entry(5, &self.ids_a),
                buf_entry(8, &self.com_prefix),
            ],
        );
        let bg_flags = oct_bg(
            device,
            &self.bgl_flags,
            &[buf_entry(3, &self.keys_a), buf_entry(9, &self.flags)],
        );
        let bg_ncount = oct_bg(
            device,
            &self.bgl_ncount,
            &[buf_entry(9, &self.flags), buf_entry(10, &self.node_base)],
        );
        let bg_emit = oct_bg(
            device,
            &self.bgl_emit,
            &[
                buf_entry(2, &self.world),
                buf_entry(3, &self.keys_a),
                buf_entry(5, &self.ids_a),
                buf_entry(8, &self.com_prefix),
                buf_entry(9, &self.flags),
                buf_entry(10, &self.node_base),
                buf_entry(11, oct_nodes),
            ],
        );
        let bg_aux = oct_bg(
            device,
            &self.bgl_aux,
            &[
                buf_entry(5, &self.ids_a),
                buf_entry(10, &self.node_base),
                buf_entry(12, &self.oct_aux),
            ],
        );

        let (nx, ny) = dispatch_grid(n);
        let (hx, hy) = dispatch_grid(hl);
        let (px, py) = dispatch_grid(p1);

        // 1. Bounding box.
        oct_pass(encoder, &self.p_bbox_clear, &bg_bbox_clear, &bg_params, 1, 1);
        oct_pass(encoder, &self.p_bbox_reduce, &bg_bbox_reduce, &bg_params, nx, ny);
        oct_pass(encoder, &self.p_bbox_finalize, &bg_bbox_finalize, &bg_params, 1, 1);
        // 2. Morton keys into the a-buffers.
        oct_pass(encoder, &self.p_morton, &bg_morton, &bg_params, nx, ny);
        // 3. Radix sort, ping-ponging a<->b. 8 even passes end back in a.
        for pass in 0usize..8usize {
            let even = pass % 2 == 0;
            let bg_hist = if even { &bg_hist_a } else { &bg_hist_b };
            let bg_scatter = if even { &bg_scatter_ab } else { &bg_scatter_ba };
            oct_pass(encoder, &self.p_histogram[pass], bg_hist, &bg_params, nx, ny);
            oct_pass(encoder, &self.p_scan_local_u32, &bg_scan_hist, &bg_dims_hist, hx, hy);
            oct_pass(encoder, &self.p_scan_serial_u32, &bg_scan_serial_u32, &bg_dims_hist, 1, 1);
            oct_pass(encoder, &self.p_scan_fixup_u32, &bg_scan_hist, &bg_dims_hist, hx, hy);
            oct_pass(encoder, &self.p_scatter[pass], bg_scatter, &bg_params, nx, ny);
        }
        // 5. COM prefix sums over sorted bodies.
        oct_pass(encoder, &self.p_com, &bg_com, &bg_params, px, py);
        oct_pass(encoder, &self.p_scan_local_v4, &bg_scan_com, &bg_dims_p1, px, py);
        oct_pass(encoder, &self.p_scan_serial_v4, &bg_scan_serial_v4, &bg_dims_p1, 1, 1);
        oct_pass(encoder, &self.p_scan_fixup_v4, &bg_scan_com, &bg_dims_p1, px, py);
        // 4/6. Level boundary flags, per-body node counts, DFS base scan.
        oct_pass(encoder, &self.p_flags, &bg_flags, &bg_params, nx, ny);
        oct_pass(encoder, &self.p_ncount, &bg_ncount, &bg_params, px, py);
        oct_pass(encoder, &self.p_scan_local_u32, &bg_scan_nb, &bg_dims_p1, px, py);
        oct_pass(encoder, &self.p_scan_serial_u32, &bg_scan_serial_u32, &bg_dims_p1, 1, 1);
        oct_pass(encoder, &self.p_scan_fixup_u32, &bg_scan_nb, &bg_dims_p1, px, py);
        // 7/8. Emit nodes and the aux (body_rank + node count) buffer.
        oct_pass(encoder, &self.p_emit, &bg_emit, &bg_params, nx, ny);
        oct_pass(encoder, &self.p_aux, &bg_aux, &bg_params, nx, ny);
    }
}

// ---------- Tests ------------------------------------------------------------

#[cfg(all(test, not(target_arch = "wasm32")))]
// The GPU tests below hold `GPU_TEST_LOCK` across `.await` to serialize wgpu
// device use (see the static's doc). Each runs on its own current-thread
// runtime, so this is safe — the lint is a false positive here.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

    /// Serializes the GPU tests in THIS binary: running all six concurrently
    /// creates six wgpu devices at once, which trips Metal validation errors
    /// under load (an intermittent CI flake). cargo already serializes across
    /// test binaries, so a per-binary lock is sufficient. A plain
    /// `std::sync::Mutex` (not tokio's — its `sync` feature isn't enabled in
    /// dev-deps); held across `.await`, which is safe on a current-thread
    /// runtime and simply blocks the next test's harness thread. Poison is
    /// recovered so one failing GPU test doesn't cascade.
    static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn gpu_test_guard() -> std::sync::MutexGuard<'static, ()> {
        GPU_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn triangle() -> Graph {
        let mut g = Graph::new();
        g.add_node(Node::new("a"));
        g.add_node(Node::new("b"));
        g.add_node(Node::new("c"));
        g.add_edge(Edge::new("ab", "a", "b"));
        g.add_edge(Edge::new("bc", "b", "c"));
        g.add_edge(Edge::new("ca", "c", "a"));
        g
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_runs_and_moves_nodes() {
        let _gpu = gpu_test_guard();
        let mut graph = triangle();
        // Seed deterministic-ish initial positions.
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            if let Some(n) = graph.nodes.get_mut(*id) {
                n.position3 = Some([i as f32 * 10.0, 0.0, 0.0]);
            }
        }
        let initial: Vec<[f32; 3]> = ["a", "b", "c"]
            .iter()
            .map(|id| graph.nodes[*id].position3.unwrap())
            .collect();

        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 4,
            repulsion: 200.0,
            ..Default::default()
        });
        match layout.run(&mut graph).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        }

        // Every node must now have position3, and at least one must have moved.
        let mut any_moved = false;
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let p = graph.nodes[*id]
                .position3
                .expect("position3 must be set after run");
            let d = (p[0] - initial[i][0]).abs()
                + (p[1] - initial[i][1]).abs()
                + (p[2] - initial[i][2]).abs();
            if d > 1e-4 {
                any_moved = true;
            }
        }
        assert!(any_moved, "force step should have moved at least one node");
        assert_eq!(layout.node_count(), Some(3));
    }

    fn random_graph(n: usize, m: usize) -> Graph {
        let mut g = Graph::new();
        let mut s: u32 = 0xDEADBEEF;
        let mut rng = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            s
        };
        for i in 0..n {
            let mut node = Node::new(format!("{:06}", i));
            let r = 200.0;
            let x = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            let y = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            let z = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            node.position3 = Some([x, y, z]);
            g.add_node(node);
        }
        for k in 0..m {
            let a = (rng() as usize) % n;
            let b = (rng() as usize) % n;
            if a == b { continue; }
            g.add_edge(Edge::new(format!("e{}", k), format!("{:06}", a), format!("{:06}", b)));
        }
        g
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_exact_produces_reasonable_layout() {
        let _gpu = gpu_test_guard();
        // 100 random nodes, 200 random edges. Run 10 steps on the exact
        // O(n²) reference backend.
        let mut g = random_graph(100, 200);
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 10,
            repulsion_mode: RepulsionMode::Exact,
            repulsion_radius: 120.0,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping (no gpu adapter): {e}");
                return;
            }
        }
        // Verify all positions finite + non-degenerate spread.
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        let mut all_finite = true;
        for node in g.nodes.values() {
            let p = node.position3.expect("position3 set");
            for k in 0..3 {
                if !p[k].is_finite() { all_finite = false; }
                if p[k] < mn[k] { mn[k] = p[k]; }
                if p[k] > mx[k] { mx[k] = p[k]; }
            }
        }
        assert!(all_finite, "all positions must be finite");
        let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]);
        assert!(span > 50.0, "layout collapsed: span={span}");
    }

    /// Hub-aware spring kernel (Phase 0.3): a star graph with one degree-1000
    /// hub stresses Tigr virtualization — without splitting, the hub's lane
    /// would serially walk 1000 edges while sibling lanes finish instantly.
    /// Repulsion is disabled so this isolates the spring kernel: with real
    /// Barnes-Hut repulsion 1000 mutually-repelling leaves legitimately
    /// spread far past their springs (the BH path is validated by
    /// `unit_gpu_octree_*` / `unit_gpu_force_barnes_hut_runs_on_small_graph`).
    /// Asserts finite positions with leaves spring-bound near the hub.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_star_hub_stable() {
        let _gpu = gpu_test_guard();
        const N_LEAVES: usize = 1000;
        let mut g = Graph::new();
        let mut hub = Node::new("hub".to_string());
        hub.position3 = Some([0.0, 0.0, 0.0]);
        g.add_node(hub);
        // Spread leaves on a sphere so initial positions aren't degenerate.
        for i in 0..N_LEAVES {
            let mut n = Node::new(format!("l{:04}", i));
            let theta = (i as f32) * 0.137;
            let phi = (i as f32) * 0.071;
            let r = 50.0;
            n.position3 = Some([
                r * phi.cos() * theta.sin(),
                r * phi.sin() * theta.sin(),
                r * theta.cos(),
            ]);
            g.add_node(n);
            g.add_edge(Edge::new(
                format!("e{:04}", i),
                "hub".to_string(),
                format!("l{:04}", i),
            ));
        }
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 50,
            repulsion: 0.0,
            spring_k: 0.5,
            spring_len: 30.0,
            gravity: 0.05,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => { eprintln!("skipping (no gpu adapter): {e}"); return; }
        }
        let hub_pos = g.nodes["hub"].position3.expect("position3 set");
        assert!(hub_pos.iter().all(|v| v.is_finite()), "hub position non-finite");
        // Every leaf is spring-bound to the hub through a *virtual* vertex,
        // so after 50 steps no leaf may have escaped past a couple of rest
        // lengths, and the cloud must not have collapsed onto the hub.
        // (Where the hub itself settles relative to the origin depends on
        // the deliberately asymmetric seeding above and is not asserted.)
        let mut max_leaf_dist = 0.0f32;
        let mut min_leaf_dist = f32::INFINITY;
        for (id, node) in g.nodes.iter() {
            if id == "hub" { continue; }
            let p = node.position3.expect("position3 set");
            assert!(p.iter().all(|v| v.is_finite()), "leaf {id} non-finite");
            let d = ((p[0] - hub_pos[0]).powi(2)
                + (p[1] - hub_pos[1]).powi(2)
                + (p[2] - hub_pos[2]).powi(2))
            .sqrt();
            max_leaf_dist = max_leaf_dist.max(d);
            min_leaf_dist = min_leaf_dist.min(d);
        }
        assert!(max_leaf_dist < 60.0, "leaf escaped its hub spring: {max_leaf_dist}");
        assert!(max_leaf_dist > 1.0, "leaves collapsed onto the hub: {max_leaf_dist}");
        assert!(min_leaf_dist.is_finite());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_barnes_hut_runs_on_small_graph() {
        let _gpu = gpu_test_guard();
        // 4-node graph, BH mode. Verify it doesn't crash and produces
        // a sensible layout (all positions finite, some movement).
        let mut g = Graph::new();
        for i in 0..4 {
            let mut n = Node::new(format!("n{}", i));
            n.position3 = Some([i as f32 * 5.0, 0.0, 0.0]);
            g.add_node(n);
        }
        g.add_edge(Edge::new("e0", "n0", "n1"));
        g.add_edge(Edge::new("e1", "n1", "n2"));
        g.add_edge(Edge::new("e2", "n2", "n3"));
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 4,
            repulsion: 100.0,
            repulsion_mode: RepulsionMode::BarnesHut,
            theta: 0.7,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => { eprintln!("skipping (no gpu adapter): {e}"); return; }
        }
        let mut all_finite = true;
        for node in g.nodes.values() {
            let p = node.position3.expect("position3 set");
            for k in 0..3 { if !p[k].is_finite() { all_finite = false; } }
        }
        assert!(all_finite, "BH path produced non-finite positions");
    }

    // ---- SeedMode plumbing ------------------------------------------------
    //
    // These exercise `precompute` directly — the function the GPU sim calls
    // to produce its initial-position buffer. We *cannot* preset `position3`
    // on the nodes (which the other tests do for determinism) because that
    // bypasses the seeder. Build seederless ring graphs and read the buffer
    // back.

    fn seederless_ring(n: usize) -> Graph {
        let mut g = Graph::new();
        for i in 0..n {
            g.add_node(Node::new(format!("{:04}", i)));
        }
        for i in 0..n {
            g.add_edge(Edge::new(
                format!("e{i}"),
                format!("{:04}", i),
                format!("{:04}", (i + 1) % n),
            ));
        }
        g
    }

    fn stddev_per_axis(positions: &[f32], n: usize) -> [f32; 3] {
        // precompute returns vec4-padded `[x,y,z,0]` per node.
        let mut means = [0.0f32; 3];
        for i in 0..n {
            for c in 0..3 {
                means[c] += positions[4 * i + c];
            }
        }
        for c in 0..3 {
            means[c] /= n as f32;
        }
        let mut var = [0.0f32; 3];
        for i in 0..n {
            for c in 0..3 {
                let d = positions[4 * i + c] - means[c];
                var[c] += d * d;
            }
        }
        [
            (var[0] / n as f32).sqrt(),
            (var[1] / n as f32).sqrt(),
            (var[2] / n as f32).sqrt(),
        ]
    }

    #[test]
    fn precompute_random_seed_spreads_in_three_dimensions() {
        let g = seederless_ring(96);
        let opts = GpuForceOptions::default();
        assert!(matches!(opts.seed_mode, SeedMode::Random));
        let pc = precompute(&g, &opts.seed_mode, opts.spring_len);
        let sd = stddev_per_axis(&pc.initial_positions, pc.n_nodes as usize);
        assert!(sd[0] > 0.0 && sd[1] > 0.0 && sd[2] > 0.0);
        let xy_max = sd[0].max(sd[1]);
        assert!(
            sd[2] > 0.1 * xy_max,
            "Random seed flattened z: sx={} sy={} sz={}",
            sd[0],
            sd[1],
            sd[2]
        );
    }

    #[test]
    fn precompute_topo_fisheye_seed_spreads_in_three_dimensions() {
        let g = seederless_ring(128);
        let mut opts = GpuForceOptions::default();
        opts.seed_mode = SeedMode::TopoFisheye;
        let pc = precompute(&g, &opts.seed_mode, opts.spring_len);
        let sd = stddev_per_axis(&pc.initial_positions, pc.n_nodes as usize);
        assert!(sd[0] > 0.0 && sd[1] > 0.0 && sd[2] > 0.0);
        let xy_max = sd[0].max(sd[1]);
        assert!(
            sd[2] > 0.1 * xy_max,
            "TopoFisheye seed flattened z: sx={} sy={} sz={}",
            sd[0],
            sd[1],
            sd[2]
        );
    }

    #[test]
    fn precompute_seed_modes_produce_different_layouts() {
        let g = seederless_ring(96);
        let opts = GpuForceOptions::default();
        let pc_rand = precompute(&g, &SeedMode::Random, opts.spring_len);
        let pc_tf = precompute(&g, &SeedMode::TopoFisheye, opts.spring_len);
        assert_eq!(pc_rand.initial_positions.len(), pc_tf.initial_positions.len());
        // L2 between the two buffers must be substantial — otherwise the
        // seeder dispatch is a no-op and `SeedMode` does nothing.
        let l2_sq: f32 = pc_rand
            .initial_positions
            .iter()
            .zip(pc_tf.initial_positions.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        assert!(l2_sq.sqrt() > 1.0, "seed modes produced identical buffers");
    }

    #[test]
    fn seed_mode_serde_round_trip() {
        let mut opts = GpuForceOptions::default();
        opts.seed_mode = SeedMode::TopoFisheye;
        let json = serde_json::to_string(&opts).expect("serialize");
        assert!(
            json.contains("\"seed_mode\":\"topo_fisheye\""),
            "missing seed_mode field in serialized JSON: {json}"
        );
        let back: GpuForceOptions = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back.seed_mode, SeedMode::TopoFisheye));
    }

    #[test]
    fn dispatch_grid_spills_into_y_past_the_per_dimension_cap() {
        // Below the cap: plain 1-D.
        assert_eq!(dispatch_grid(0), (1, 1));
        assert_eq!(dispatch_grid(1), (1, 1));
        assert_eq!(dispatch_grid(64), (1, 1));
        assert_eq!(dispatch_grid(65), (2, 1));
        let cap = MAX_WORKGROUPS_PER_DIM * WORKGROUP_SIZE;
        assert_eq!(dispatch_grid(cap), (MAX_WORKGROUPS_PER_DIM, 1));
        // One lane past the cap needs a second row; every emitted grid
        // must cover the request and never exceed the cap per dimension.
        for n in [cap + 1, cap * 3 + 7, 50_000_000, u32::MAX / 64] {
            let (x, y) = dispatch_grid(n);
            assert!(x <= MAX_WORKGROUPS_PER_DIM && y <= MAX_WORKGROUPS_PER_DIM, "n={n}");
            let covered = (x as u64) * (y as u64) * (WORKGROUP_SIZE as u64);
            assert!(covered >= n as u64, "n={n} covered={covered}");
        }
    }

    #[test]
    fn unknown_backend_strings_fall_back_to_default_not_exact() {
        // A stale persisted "grid" (the retired voxel path) must land on the
        // default backend, never on the O(n²) one.
        assert_eq!(RepulsionMode::from_str("grid"), RepulsionMode::default());
        assert_eq!(RepulsionMode::from_str("bogus"), RepulsionMode::default());
        assert_eq!(RepulsionMode::from_str("exact"), RepulsionMode::Exact);
        assert_eq!(ForceModel::from_str("nope"), ForceModel::default());
        assert_eq!(ForceModel::from_str("t_fdp"), ForceModel::TFdp);
        let json = r#"{"repulsion_mode":"grid","force_model":"junk"}"#;
        let opts: GpuForceOptions = serde_json::from_str(json).expect("lenient");
        assert_eq!(opts.repulsion_mode, RepulsionMode::default());
        assert_eq!(opts.force_model, ForceModel::default());
    }

    #[test]
    fn force_model_serde_round_trip() {
        let mut opts = GpuForceOptions::default();
        opts.force_model = ForceModel::TFdp;
        opts.tfdp_k = 2.5;
        let json = serde_json::to_string(&opts).expect("serialize");
        let back: GpuForceOptions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.force_model, ForceModel::TFdp);
        assert_eq!(back.tfdp_k.to_bits(), 2.5f32.to_bits());
        assert!(opts.eq_ignoring_cursor(&back));
    }

    #[test]
    fn sim_params_uniform_is_sixteen_byte_rows() {
        // `SimParams` in force.wgsl is laid out as 16-byte rows; a Rust
        // field added off-row would silently shift every later uniform.
        assert_eq!(std::mem::size_of::<SimParamsRaw>() % 16, 0);
        assert_eq!(std::mem::size_of::<SimParamsRaw>(), 96);
    }

    /// SNAP-tFDP estimator on a two-clique graph: connected pairs must end
    /// up closer than pairs across the cliques, and nothing may go
    /// non-finite — the t-force is bounded, so a coincident start must
    /// still separate.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_tfdp_negative_sampling_separates_cliques() {
        let _gpu = gpu_test_guard();
        let mut g = Graph::new();
        let per = 12usize;
        for i in 0..per * 2 {
            g.add_node(Node::new(format!("n{i:03}")));
        }
        for c in 0..2 {
            for a in 0..per {
                for b in (a + 1)..per {
                    g.add_edge(Edge::new(
                        format!("e{c}_{a}_{b}"),
                        format!("n{:03}", c * per + a),
                        format!("n{:03}", c * per + b),
                    ));
                }
            }
        }
        // One bridge so the graph is connected.
        g.add_edge(Edge::new("bridge", "n000", format!("n{:03}", per)));
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 400,
            repulsion_mode: RepulsionMode::NegativeSampling,
            repulsion_samples: 8,
            force_model: ForceModel::TFdp,
            spring_len: 100.0,
            gravity: 0.0,
            energy_threshold: 0.0,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping (no gpu adapter): {e}");
                return;
            }
        }
        let pos = |i: usize| g.nodes[&format!("n{i:03}")].position3.expect("position3");
        let dist = |a: [f32; 3], b: [f32; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let mut intra = 0.0f32;
        let mut inter = 0.0f32;
        let mut n_intra = 0;
        let mut n_inter = 0;
        for a in 0..per * 2 {
            let pa = pos(a);
            assert!(pa.iter().all(|v| v.is_finite()), "non-finite position for n{a:03}");
            for b in (a + 1)..per * 2 {
                let d = dist(pa, pos(b));
                if (a < per) == (b < per) { intra += d; n_intra += 1; } else { inter += d; n_inter += 1; }
            }
        }
        let intra = intra / n_intra as f32;
        let inter = inter / n_inter as f32;
        assert!(intra > 1.0, "clique collapsed to a point: mean intra={intra}");
        assert!(inter > intra * 1.5, "cliques not separated: intra={intra} inter={inter}");
    }

    /// Above 65535 × 64 lanes a 1-D dispatch is rejected by WebGPU
    /// validation. Ignored by default: it allocates a 4.2 M-node graph
    /// (~1 GB host-side through `Graph`'s string-keyed maps). Run with
    /// `cargo test -p graph-layouts --release -- --ignored dispatch_past_cap`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn unit_gpu_force_dispatch_past_cap_validates() {
        let _gpu = gpu_test_guard();
        let n = (MAX_WORKGROUPS_PER_DIM * WORKGROUP_SIZE + 64) as usize;
        let mut g = Graph::new();
        for i in 0..n {
            g.add_node(Node::new(i.to_string()));
        }
        // Sparse ring so the CSR is tiny relative to the position buffers.
        for i in 0..n {
            g.add_edge(Edge::new(format!("e{i}"), i.to_string(), ((i + 1) % n).to_string()));
        }
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 1,
            repulsion_mode: RepulsionMode::NegativeSampling,
            repulsion_samples: 1,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping (no gpu adapter): {e}");
                return;
            }
        }
        // The last lane lives in the second dispatch row; it must have
        // been integrated (moved off its seed) and stayed finite.
        let last = g.nodes[&(n - 1).to_string()].position3.expect("position3");
        assert!(last.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn seed_mode_none_serde_round_trip() {
        let mut opts = GpuForceOptions::default();
        opts.seed_mode = SeedMode::None;
        let json = serde_json::to_string(&opts).expect("serialize");
        assert!(
            json.contains("\"seed_mode\":\"none\""),
            "missing seed_mode=none in serialized JSON: {json}"
        );
        let back: GpuForceOptions = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back.seed_mode, SeedMode::None));
        // Aliases also resolve to None.
        assert!(matches!(SeedMode::from_str("keep"), SeedMode::None));
        assert!(matches!(SeedMode::from_str("keep_current"), SeedMode::None));
    }

    #[test]
    fn precompute_none_seed_preserves_meaningful_positions() {
        // A graph whose nodes already carry meaningful `position3` (the
        // generated-sphere / applied-seed / resumed-sim case). Under
        // SeedMode::None, precompute's initial_positions must reproduce those
        // positions exactly — no random ball, no flattening.
        let mut g = seederless_ring(32);
        let n = g.nodes.len();
        let mut want: Vec<f32> = Vec::with_capacity(n * 3);
        for (i, id) in {
            let mut ids: Vec<String> = g.nodes.keys().cloned().collect();
            ids.sort();
            ids
        }
        .into_iter()
        .enumerate()
        {
            let p = [i as f32 * 1.5, i as f32 * -2.5, i as f32 * 3.5];
            want.extend_from_slice(&p);
            g.nodes.get_mut(&id).unwrap().position3 = Some(p);
        }
        let pc = precompute(&g, &SeedMode::None, GpuForceOptions::default().spring_len);
        assert_eq!(pc.initial_positions.len(), n * 4);
        for i in 0..n {
            assert_eq!(pc.initial_positions[4 * i], want[3 * i], "x[{i}]");
            assert_eq!(pc.initial_positions[4 * i + 1], want[3 * i + 1], "y[{i}]");
            assert_eq!(pc.initial_positions[4 * i + 2], want[3 * i + 2], "z[{i}]");
        }
    }

    #[test]
    fn precompute_none_seed_without_position3_is_zeros() {
        // No author positions + None mode => zero base. The GPU upload is
        // skipped in init_with_device for this mode, so the (degenerate) base
        // never reaches the buffer; this just pins the documented contract.
        let g = seederless_ring(16);
        let pc = precompute(&g, &SeedMode::None, GpuForceOptions::default().spring_len);
        // The .w slot of each vec4 carries packed node mass, so only assert the
        // x/y/z lanes are zero.
        for i in 0..pc.n_nodes as usize {
            assert_eq!(pc.initial_positions[4 * i], 0.0, "x[{i}]");
            assert_eq!(pc.initial_positions[4 * i + 1], 0.0, "y[{i}]");
            assert_eq!(pc.initial_positions[4 * i + 2], 0.0, "z[{i}]");
        }
    }

    #[test]
    fn precompute_csr_matches_graph_precompute() {
        // A 50-node graph with ids n000.. and a deterministic ring+chord edge
        // set. Building the index edge list from the SAME `g.edges` iteration
        // order the adapter uses keeps `edge_neighbors` byte-identical.
        let n: usize = 50;
        let mut g = Graph::new();
        for i in 0..n {
            g.add_node(Node::new(format!("n{:03}", i)));
        }
        for i in 0..n {
            g.add_edge(Edge::new(
                format!("r{i}"),
                format!("n{:03}", i),
                format!("n{:03}", (i + 1) % n),
            ));
        }
        for i in 0..n / 5 {
            let a = i * 5;
            let b = (i * 7 + 3) % n;
            if a != b {
                g.add_edge(Edge::new(
                    format!("c{i}"),
                    format!("n{:03}", a),
                    format!("n{:03}", b),
                ));
            }
        }

        let mut ids: Vec<String> = g.nodes.keys().cloned().collect();
        ids.sort();
        let id_to_idx: std::collections::HashMap<&str, u32> = ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i as u32))
            .collect();
        let mut edges: Vec<u32> = Vec::new();
        for e in g.edges.values() {
            let (Some(&s), Some(&t)) = (
                id_to_idx.get(e.source.as_str()),
                id_to_idx.get(e.target.as_str()),
            ) else {
                continue;
            };
            edges.push(s);
            edges.push(t);
        }

        let seed = SeedMode::Random;
        let len = GpuForceOptions::default().spring_len;
        let pc_csr = precompute_csr(n as u32, &edges, None, &seed, len);
        let pc_graph = precompute(&g, &seed, len);
        assert_eq!(pc_csr.edge_offsets, pc_graph.edge_offsets, "edge_offsets");
        assert_eq!(pc_csr.edge_neighbors, pc_graph.edge_neighbors, "edge_neighbors");
        assert_eq!(pc_csr.mass, pc_graph.mass, "mass");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_run_csr_ring_finite() {
        let _gpu = gpu_test_guard();
        // 200-node ring, fed as an index edge list. run_csr must produce
        // finite, non-degenerate positions of length 3*n with no Graph.
        let n: u32 = 200;
        let mut edges: Vec<u32> = Vec::with_capacity(n as usize * 2);
        for i in 0..n {
            edges.push(i);
            edges.push((i + 1) % n);
        }
        let input = CsrInput {
            n_nodes: n,
            edges: &edges,
            positions: None,
        };
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 10,
            repulsion_mode: RepulsionMode::Exact,
            repulsion_radius: 120.0,
            ..Default::default()
        });
        let mut out: Vec<f32> = Vec::new();
        match layout.run_csr(&input, &mut out).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping (no gpu adapter): {e}");
                return;
            }
        }
        assert_eq!(out.len(), (n as usize) * 3, "out must be [x,y,z]*n");
        let mut all_finite = true;
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for p in out.chunks_exact(3) {
            for k in 0..3 {
                if !p[k].is_finite() {
                    all_finite = false;
                }
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        assert!(all_finite, "all positions must be finite");
        let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]);
        assert!(span > 1.0, "layout degenerate: span={span}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_init_with_device_csr_borrowed_step_finite() {
        let _gpu = gpu_test_guard();
        // Borrowed-buffer CSR path: init_with_device_csr then one
        // step_with_encoder must leave finite positions in the shared buffer.
        let n: u32 = 64;
        let mut edges: Vec<u32> = Vec::with_capacity(n as usize * 2);
        for i in 0..n {
            edges.push(i);
            edges.push((i + 1) % n);
        }

        // Acquire a device the way `run` does; skip cleanly if none.
        let instance = wgpu::Instance::default();
        let Some(adapter) = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
        else {
            eprintln!("skipping (no gpu adapter)");
            return;
        };
        let (device, queue) = match adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("test/gpu_force_csr"),
                    required_features: wgpu::Features::empty(),
                    required_limits: gpu_force_device_limits(&adapter.limits()),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };

        let pos_buf_size = (n as u64) * VEC3_STRIDE;
        let positions_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test_shared_positions"),
            size: pos_buf_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let input = CsrInput {
            n_nodes: n,
            edges: &edges,
            positions: None,
        };
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 1,
            repulsion_mode: RepulsionMode::Exact,
            repulsion_radius: 120.0,
            ..Default::default()
        });
        layout
            .init_with_device_csr(&device, &queue, &input, &positions_buffer)
            .expect("init csr");

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test_step"),
        });
        layout.step_with_encoder(&device, &queue, &mut encoder, &positions_buffer);
        queue.submit(Some(encoder.finish()));

        let positions = layout
            .read_back_positions(&device, &queue, &positions_buffer)
            .await
            .expect("readback");
        assert_eq!(positions.len(), (n as usize) * 4);
        for p in positions.chunks_exact(4).take(n as usize) {
            for k in 0..3 {
                assert!(p[k].is_finite(), "position must be finite");
            }
        }
    }

    // ---- Compact-seed stability ------------------------------------------
    //
    // Regression for "screen turns black on energy_threshold=0" and
    // "force-directed physics isn't working". The other tests in this
    // module either lower `repulsion` to 50-200 (`unit_gpu_force_*`) or
    // start with a wide-spread position field (`random_graph`), so the
    // catastrophic compact-seed × default-repulsion regime was never
    // exercised. These two tests *do* exercise it.

    fn ring_topology_graph(n: usize, spread_radius: f32) -> Graph {
        // Compact pre-seeded positions to mimic a TopoFisheye §5 layout
        // (multilevel coarsen + relax produces a tight cluster). Ring
        // edges so every node has degree 2 — non-degenerate but cheap.
        let mut g = Graph::new();
        for i in 0..n {
            let mut node = Node::new(format!("n{:04}", i));
            let theta = (i as f32) / (n as f32) * std::f32::consts::TAU;
            node.position3 = Some([
                spread_radius * theta.cos(),
                spread_radius * theta.sin(),
                spread_radius * 0.1 * (i as f32 * 0.37).sin(),
            ]);
            g.add_node(node);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            g.add_edge(Edge::new(
                format!("e{i}"),
                format!("n{:04}", i),
                format!("n{:04}", j),
            ));
        }
        g
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_compact_seed_does_not_explode() {
        let _gpu = gpu_test_guard();
        // 100 nodes packed in a ~5-unit ball — the same regime a fresh
        // TopoFisheye seed puts the layout in for a sub-100-node vault.
        // Run the *default* GpuForceOptions (repulsion=4000, dt=0.1) for
        // 32 steps and verify positions stay finite and bounded.
        let mut graph = ring_topology_graph(100, 5.0);
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 32,
            ..Default::default()
        });
        if let Err(e) = layout.run(&mut graph).await {
            eprintln!("skipping: {e}");
            return;
        }
        let mut max_mag: f32 = 0.0;
        let mut all_finite = true;
        for id in graph.nodes.keys().cloned().collect::<Vec<_>>() {
            let p = graph.nodes[&id].position3.expect("position3 set");
            for c in p {
                if !c.is_finite() {
                    all_finite = false;
                }
                max_mag = max_mag.max(c.abs());
            }
        }
        assert!(all_finite, "non-finite positions after compact-seed run");
        // The regression this guards is NaN propagation ("screen turns
        // black") and a runaway to infinity, so `all_finite` above is the
        // core check. Under the now-correct Barnes-Hut default the huge
        // repulsion=4000 legitimately blasts a compact 100-node ring
        // (spring_len=400) out to several thousand units before damping
        // reins it in — the old <1e4 bound pinned the broken weak-BH
        // default. Keep a generous ceiling that still rules out the 1e10+
        // divergence failure mode.
        assert!(
            max_mag < 1e6,
            "compact seed diverged: max|p|={max_mag} (expected finite, < 1e6)"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_coincident_seed_recovers() {
        let _gpu = gpu_test_guard();
        // Tightest stress on the dist² floor + velocity clamp: every node
        // starts at the origin. Whatever forces fire on step 1 must not
        // produce NaN/Inf; the layout should spread to a non-trivial
        // configuration within the integration budget.
        let mut graph = ring_topology_graph(64, 0.0);
        // Manually zero every position3 to be exact.
        for id in graph.nodes.keys().cloned().collect::<Vec<_>>() {
            if let Some(n) = graph.nodes.get_mut(&id) {
                n.position3 = Some([0.0, 0.0, 0.0]);
            }
        }
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 32,
            ..Default::default()
        });
        if let Err(e) = layout.run(&mut graph).await {
            eprintln!("skipping: {e}");
            return;
        }
        let mut all_finite = true;
        let mut max_mag: f32 = 0.0;
        for id in graph.nodes.keys().cloned().collect::<Vec<_>>() {
            let p = graph.nodes[&id].position3.expect("position3 set");
            for c in p {
                if !c.is_finite() {
                    all_finite = false;
                }
                max_mag = max_mag.max(c.abs());
            }
        }
        assert!(
            all_finite,
            "coincident seed produced NaN/Inf — dist² floor / velocity clamp insufficient"
        );
        assert!(
            max_mag < 1e4,
            "coincident seed exploded: max|p|={max_mag}"
        );
    }

    /// Copy a GPU buffer back to the CPU as raw bytes (native tests only).
    async fn read_back_bytes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::Buffer,
        size: u64,
    ) -> Vec<u8> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oct_readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("oct_readback_enc"),
        });
        enc.copy_buffer_to_buffer(src, 0, &staging, 0, size);
        queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = futures_channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv().await.expect("map channel").expect("map ok");
        let view = slice.get_mapped_range();
        let data = view.to_vec();
        drop(view);
        staging.unmap();
        data
    }

    /// The GPU octree build must produce a well-formed rope: a bounded node
    /// count, a root mass equal to the total body mass, rope indices that
    /// stay in range, and a walk that visits every single-body leaf once.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_octree_build_structure() {
        let _gpu = gpu_test_guard();
        let mut g = random_graph(5000, 10000);
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 1,
            repulsion_mode: RepulsionMode::BarnesHut,
            theta: 0.7,
            ..Default::default()
        });
        if let Err(e) = layout.run(&mut g).await {
            eprintln!("skipping (no gpu adapter): {e}");
            return;
        }
        let state = layout.state.as_ref().unwrap();
        let od = layout.owned_device.as_ref().unwrap();
        let n = state.n_nodes;

        // oct_aux[0] = node count; [1 + i] = sorted rank of real body i.
        let aux_bytes =
            read_back_bytes(&od.device, &od.queue, &state.octree.oct_aux, state.octree.p1 as u64 * 4)
                .await;
        let aux: &[u32] = bytemuck::cast_slice(&aux_bytes);
        let count = aux[0];
        assert!(count > 0, "GPU octree produced no nodes");
        assert!(count <= 2 * n + 16, "node count {count} exceeds 2N+16 (N={n})");

        let node_bytes = read_back_bytes(
            &od.device,
            &od.queue,
            &state.oct_nodes_buf,
            count as u64 * std::mem::size_of::<OctNodeRaw>() as u64,
        )
        .await;
        let nodes: &[OctNodeRaw] = bytemuck::cast_slice(&node_bytes);

        // Root mass == sum of body masses (positions[i].w carries mass).
        let positions = state
            .read_positions_owned(&od.device, &od.queue)
            .await
            .expect("readback");
        let mut total_mass = 0.0f64;
        for i in 0..n as usize {
            total_mass += positions[4 * i + 3] as f64;
        }
        let root_mass = nodes[0].com_mass[3] as f64;
        let rel = (root_mass - total_mass).abs() / total_mass.max(1e-6);
        assert!(rel < 1e-2, "root mass {root_mass} vs total {total_mass} (rel {rel})");

        // Every rope index is in range or the sentinel.
        for (k, node) in nodes.iter().enumerate() {
            let next = node.meta[1];
            let skip = node.meta[2];
            assert!(next == OCT_END || next < count, "node {k} next {next} >= count {count}");
            assert!(skip == OCT_END || skip < count, "node {k} skip {skip} >= count {count}");
        }

        let single_leaves = nodes
            .iter()
            .filter(|nd| nd.meta[0] != OCT_BODY_INTERNAL && nd.meta[3] == 1)
            .count();

        // Walk the rope from the root: internal -> next, leaf -> skip.
        let mut seen = std::collections::HashSet::new();
        let mut idx = 0u32;
        let mut steps = 0u32;
        let cap_steps = count.saturating_mul(8).saturating_add(64);
        while idx != OCT_END {
            assert!(steps < cap_steps, "rope did not terminate");
            steps += 1;
            let node = nodes[idx as usize];
            let body = node.meta[0];
            let cnt = node.meta[3];
            if body != OCT_BODY_INTERNAL {
                if cnt == 1 {
                    assert!(seen.insert(body), "single-body leaf {body} visited twice");
                }
                idx = node.meta[2];
            } else {
                idx = node.meta[1];
            }
        }
        assert_eq!(
            seen.len(),
            single_leaves,
            "rope visited {} single-body leaves, tree has {single_leaves}",
            seen.len()
        );
    }

    /// A fully coincident body cloud (all 64 nodes at the origin) collapses
    /// into one deep chain ending in a multi-body max-depth leaf. The GPU
    /// build must not hang and the force step must stay finite.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_octree_coincident_bodies_stay_finite() {
        let _gpu = gpu_test_guard();
        let mut g = Graph::new();
        for i in 0..64 {
            g.add_node(Node::new(format!("n{i:02}")));
        }
        for i in 0..64 {
            g.add_edge(Edge::new(
                format!("e{i}"),
                format!("n{i:02}"),
                format!("n{:02}", (i + 1) % 64),
            ));
        }
        for id in g.nodes.keys().cloned().collect::<Vec<_>>() {
            g.nodes.get_mut(&id).unwrap().position3 = Some([0.0, 0.0, 0.0]);
        }
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 4,
            repulsion: 100.0,
            repulsion_mode: RepulsionMode::BarnesHut,
            theta: 0.7,
            ..Default::default()
        });
        if let Err(e) = layout.run(&mut g).await {
            eprintln!("skipping (no gpu adapter): {e}");
            return;
        }
        for node in g.nodes.values() {
            let p = node.position3.expect("position3 set");
            assert!(
                p.iter().all(|v| v.is_finite()),
                "coincident BH build produced a non-finite position"
            );
        }
    }

    /// `SeedMode` string round trip for the device-multilevel variant.
    #[test]
    fn unit_seed_mode_gpu_multilevel_round_trip() {
        assert_eq!(SeedMode::from_str("gpu_multilevel"), SeedMode::GpuMultilevel);
        assert_eq!(SeedMode::from_str("multilevel"), SeedMode::GpuMultilevel);
        assert_eq!(SeedMode::from_str("ml"), SeedMode::GpuMultilevel);
        assert_eq!(SeedMode::from_str("GPU_Multilevel"), SeedMode::GpuMultilevel);
        assert_eq!(SeedMode::GpuMultilevel.to_str(), "gpu_multilevel");
        assert_eq!(
            SeedMode::from_str(SeedMode::GpuMultilevel.to_str()),
            SeedMode::GpuMultilevel
        );
    }

    async fn acquire_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await?;
        adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("test/gpu_multilevel"),
                    required_features: wgpu::Features::empty(),
                    required_limits: gpu_force_device_limits(&adapter.limits()),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .ok()
    }

    /// 20,000-node "ring of rings": 200 rings of 100 nodes, each ring a cycle,
    /// consecutive rings joined through their node 0. After the device
    /// multilevel seed with ZERO fine steps the layout must be finite, spread
    /// wider than 10 spring-lengths, and place graph-adjacent nodes far closer
    /// than random pairs — a quality proxy a random seed fails.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_multilevel_seed_ring_of_rings_quality() {
        let _gpu = gpu_test_guard();
        let rings = 200usize;
        let per = 100usize;
        let n = rings * per;
        let idx = |r: usize, k: usize| (r * per + k) as u32;
        let mut edges: Vec<u32> = Vec::with_capacity(n * 2 + rings * 2);
        for r in 0..rings {
            for k in 0..per {
                edges.push(idx(r, k));
                edges.push(idx(r, (k + 1) % per));
            }
            let nr = (r + 1) % rings;
            edges.push(idx(r, 0));
            edges.push(idx(nr, 0));
        }

        let Some((device, queue)) = acquire_test_device().await else {
            eprintln!("skipping (no gpu adapter)");
            return;
        };
        let positions_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ml_ring_shared"),
            size: (n as u64) * VEC3_STRIDE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let opts = GpuForceOptions {
            seed_mode: SeedMode::GpuMultilevel,
            ..GpuForceOptions::for_n_nodes(n)
        };
        let spring_len = opts.spring_len;
        let mut layout = GpuForceLayout::new(opts);
        let input = CsrInput { n_nodes: n as u32, edges: &edges, positions: None };
        layout
            .init_with_device_csr(&device, &queue, &input, &positions_buffer)
            .expect("init csr");

        // ZERO fine steps: read straight back after the seed.
        let positions = layout
            .read_back_positions(&device, &queue, &positions_buffer)
            .await
            .expect("readback");
        assert_eq!(positions.len(), n * 4);

        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for p in positions.chunks_exact(4).take(n) {
            for k in 0..3 {
                assert!(p[k].is_finite(), "seed produced a non-finite position");
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        let span = (0..3).map(|k| mx[k] - mn[k]).fold(0.0f32, f32::max);
        assert!(
            span > 10.0 * spring_len,
            "span {span} should exceed 10*spring_len {}",
            10.0 * spring_len
        );

        let pos_of = |i: usize| [positions[i * 4], positions[i * 4 + 1], positions[i * 4 + 2]];
        let dist = |a: [f32; 3], b: [f32; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let mut adj_sum = 0.0f64;
        let mut adj_n = 0u64;
        for r in 0..rings {
            for k in 0..per {
                let a = idx(r, k) as usize;
                let b = idx(r, (k + 1) % per) as usize;
                adj_sum += dist(pos_of(a), pos_of(b)) as f64;
                adj_n += 1;
            }
        }
        let adj_mean = adj_sum / adj_n as f64;

        let mut s: u32 = 0x1234_5678;
        let mut rng = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            s
        };
        let pairs = 2000u32;
        let mut rnd_sum = 0.0f64;
        for _ in 0..pairs {
            let a = (rng() as usize) % n;
            let b = (rng() as usize) % n;
            rnd_sum += dist(pos_of(a), pos_of(b)) as f64;
        }
        let rnd_mean = rnd_sum / pairs as f64;
        assert!(
            adj_mean < 0.25 * rnd_mean,
            "ring-adjacent mean {adj_mean} should be < 0.25 * random-pair mean {rnd_mean}"
        );
    }

    /// A 64-node path graph must produce a cascade of at least two levels
    /// (fine + coarse) whose coarsest level is <= 1000 nodes and strictly
    /// smaller than the fine level. Level sizes come from the readback-only
    /// test helper on `GpuMultilevel`.
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_multilevel_cascade_path64_levels() {
        let _gpu = gpu_test_guard();
        let n: u32 = 64;
        let mut edges: Vec<u32> = Vec::with_capacity((n as usize - 1) * 2);
        for i in 0..n - 1 {
            edges.push(i);
            edges.push(i + 1);
        }
        let Some((device, queue)) = acquire_test_device().await else {
            eprintln!("skipping (no gpu adapter)");
            return;
        };
        let positions_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ml_path_shared"),
            size: (n as u64) * VEC3_STRIDE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            seed_mode: SeedMode::GpuMultilevel,
            ..GpuForceOptions::for_n_nodes(64)
        });
        let input = CsrInput { n_nodes: n, edges: &edges, positions: None };
        layout
            .init_with_device_csr(&device, &queue, &input, &positions_buffer)
            .expect("init csr");
        let counts = layout
            .state
            .as_ref()
            .unwrap()
            .multilevel
            .as_ref()
            .expect("multilevel seed retained")
            .level_node_counts(&device, &queue)
            .await;
        assert!(
            counts.len() >= 2,
            "cascade must have >= 2 levels (fine + coarse), got {counts:?}"
        );
        let coarsest = *counts.last().unwrap();
        assert!(coarsest <= 1000, "coarsest level {coarsest} must be <= 1000 ({counts:?})");
        assert!(
            coarsest < counts[0],
            "coarsest {coarsest} should reduce below fine {} ({counts:?})",
            counts[0]
        );
    }
}

// ---------------------------------------------------------------------------
// PhysicsLayout trait impl — registers gpu-force into the layout registry.
// ---------------------------------------------------------------------------

impl crate::layout::layout_trait::PhysicsLayout for GpuForceLayout {
    type Settings = GpuForceOptions;

    fn descriptor() -> crate::layout::layout_trait::LayoutDescriptor {
        crate::layout::layout_trait::LayoutDescriptor {
            id: "gpu-force",
            kind: crate::layout::layout_trait::LayoutKind::Physics,
            display_name: "GPU force-directed",
            description:
                "wgpu compute repulsion + spring + gravity (Grid / BH / NS backends)",
            requirements: crate::layout::layout_trait::LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self {
        GpuForceLayout::new(settings)
    }

    fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &crate::types::Graph,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        GpuForceLayout::init_with_device(self, device, queue, graph, positions_buf)
    }

    fn init_with_device_csr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &CsrInput<'_>,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        GpuForceLayout::init_with_device_csr(self, device, queue, input, positions_buf)
    }

    fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        GpuForceLayout::step_with_encoder(self, device, queue, encoder, positions_buf)
    }

    fn set_settings(&mut self, settings: Self::Settings) {
        self.set_options(settings)
    }

    fn settings(&self) -> &Self::Settings {
        self.options()
    }

    fn is_halted(&self) -> bool {
        GpuForceLayout::is_halted(self)
    }

    fn last_max_ke(&self) -> f32 {
        GpuForceLayout::last_max_ke(self)
    }

    fn wake(&mut self) {
        GpuForceLayout::wake(self)
    }
}
