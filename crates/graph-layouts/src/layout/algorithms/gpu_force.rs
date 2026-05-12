//! WebGPU compute-shader force-directed layout.
//!
//! Runs natively (vulkan/metal/dx12 via wgpu defaults) and in browsers
//! (wgpu's WebGPU backend). No rendering — this is a layout engine; the
//! consumer reads positions out and renders them however it likes.
//!
//! Algorithm: O(n^2) repulsion + CSR-adjacency springs + gravity + cursor.
//! Verlet-ish integration with velocity damping. Designed to step
//! incrementally — caller picks `steps_per_call` and runs `run()` each
//! frame (or as desired).

use crate::types::Graph;
use std::borrow::Cow;
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

/// Repulsion backend selection. The grid path is the legacy 27-cell
/// uniform-voxel sweep; the Barnes-Hut path walks a host-built octree
/// with stackless rope traversal in the WGSL shader.
///
/// Default = Grid: BH only wins decisively at N≥50k or in highly
/// clustered graphs where one voxel collects hundreds of bodies. At
/// N≤10k uniform synthetic vaults the grid is competitive. Flip the
/// default once benchmarks on real Obsidian vaults justify it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepulsionMode {
    Grid,
    BarnesHut,
    /// DRGraph-style stochastic repulsion: each node samples K random
    /// others per step instead of visiting spatial neighbors. Skips the
    /// grid build entirely → ~3-5× cheaper per step at N ≥ 10k. Higher
    /// per-step variance, converges in ~2× the iterations, but per-iter
    /// is so much cheaper that wall time wins. arxiv.org/abs/2008.07799
    NegativeSampling,
}

impl Default for RepulsionMode {
    // BH is the default: it gives the best visual result on general
    // clustered graphs (the common case for Obsidian vaults — a few hubs
    // + long sparse tails). Grid is fine for dense small graphs but
    // collapses voxels on highly-clustered inputs; NS converges faster
    // on huge graphs but adds visible per-step variance. BH is the
    // honest middle ground for a fresh-install default.
    fn default() -> Self { RepulsionMode::BarnesHut }
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
            _ => SeedMode::Random,
        }
    }
    fn to_str(&self) -> &'static str {
        match self {
            SeedMode::Random => "random",
            SeedMode::TopoFisheye => "topo_fisheye",
        }
    }
}

impl RepulsionMode {
    fn as_u32(self) -> u32 {
        match self {
            RepulsionMode::Grid => 0,
            RepulsionMode::BarnesHut => 1,
            RepulsionMode::NegativeSampling => 2,
        }
    }
    fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "barneshut" | "barnes_hut" | "bh" => RepulsionMode::BarnesHut,
            "negativesampling" | "negative_sampling" | "ns" => RepulsionMode::NegativeSampling,
            _ => RepulsionMode::Grid,
        }
    }
    fn to_str(self) -> &'static str {
        match self {
            RepulsionMode::Grid => "grid",
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
    /// attractor at infinity). With grid_enabled this also lets us skip
    /// far-cell pairs cheaply. Default = 4 * spring_len = 120.
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
    /// Whether to use the spatial-hash grid. Default true. Disable for
    /// correctness comparison or for tiny graphs where the grid build
    /// dominates. Ignored when `repulsion_mode == BarnesHut`.
    pub grid_enabled: bool,
    /// Repulsion backend. Default Grid for back-compat; BarnesHut wins
    /// on large clustered graphs where one voxel collects hundreds of
    /// bodies (real Obsidian vaults with hub neighborhoods).
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
            grid_enabled,
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
            grid_enabled: o_grid_enabled,
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
            && grid_enabled               == o_grid_enabled
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
        o
    }
}

impl Default for GpuForceOptions {
    fn default() -> Self {
        Self {
            // Spread-friendly defaults: real Obsidian vaults are big
            // (10k+ nodes, dense hub clusters) so the sim needs strong
            // repulsion + long springs to keep communities legible.
            // repulsion_radius scales with spring_len so the spatial-hash
            // grid actually exposes the long-range repulsion the layout
            // needs (4× spring_len = 4 voxels of reach).
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
            grid_enabled: true,
            repulsion_mode: RepulsionMode::default(),
            seed_mode: SeedMode::default(),
            theta: 0.7,
            repulsion_samples: 8,
        }
    }
}


// Hand-rolled serde so callers can pass JSON through the WASM bridge
// without dragging serde derives onto wgpu types.
impl serde::Serialize for GpuForceOptions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("GpuForceOptions", 19)?;
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
        st.serialize_field("grid_enabled", &self.grid_enabled)?;
        st.serialize_field("repulsion_mode", self.repulsion_mode.to_str())?;
        st.serialize_field("seed_mode", self.seed_mode.to_str())?;
        st.serialize_field("theta", &self.theta)?;
        st.serialize_field("repulsion_samples", &self.repulsion_samples)?;
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
            grid_enabled: Option<bool>,
            #[serde(default)]
            repulsion_mode: Option<String>,
            #[serde(default)]
            seed_mode: Option<String>,
            #[serde(default)]
            theta: Option<f32>,
            #[serde(default)]
            repulsion_samples: Option<u32>,
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
            grid_enabled: r.grid_enabled.unwrap_or(def.grid_enabled),
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
        })
    }
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
            // Acquire our own device/queue if we haven't already.
            if self.owned_device.is_none() {
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
                            required_limits: wgpu::Limits::downlevel_defaults()
                                .using_resolution(adapter.limits()),
                            memory_hints: wgpu::MemoryHints::Performance,
                        },
                        None,
                    )
                    .await
                    .map_err(|e| format!("request_device failed: {e}"))?;
                self.owned_device = Some(OwnedDevice { device, queue });
            }
            let od = self.owned_device.as_ref().unwrap();
            self.state = Some(GpuState::new_owned(&od.device, graph, &self.options)?);
        }

        let od = self
            .owned_device
            .as_ref()
            .ok_or_else(|| "owned device missing".to_string())?;
        let state = self.state.as_mut().unwrap();
        // First-call init for cooling. `effective_damping` is constructed
        // at 1.0 in `GpuState::new_*`; the `<= 0.0` branch also catches the
        // edge case where someone explicitly set `damping = 0.0` ("freeze
        // immediately"), in which case `effective_damping` stays 0.0 (the
        // re-init writes 0.0 back). Idempotent — no oscillation.
        // TODO(cooling): if a "freeze immediately" mode is ever a real UX
        // affordance, replace this branch with a separate `frozen` flag.
        if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
            state.effective_damping = self.options.damping;
        }
        // Cool damping per call. See `GpuForceOptions::cooling_alpha` for
        // the formula and the rationale behind the inner `floor.min(damping)`
        // (it is *not* a typo for `.max` — see doc-comment).
        let alpha = self.options.cooling_alpha.clamp(0.5, 1.0);
        let floor = self.options.cooling_floor.clamp(0.0, 1.0);
        state.effective_damping = (state.effective_damping * alpha).max(floor.min(self.options.damping));

        // Negative sampling skips the grid build entirely (no bbox, no
        // bucket sort). That elision is the headline cost win.
        let use_grid = matches!(self.options.repulsion_mode, RepulsionMode::Grid)
            && self.options.grid_enabled;
        if use_grid {
            state.rebuild_and_upload_grid(&od.device, &od.queue, &self.options);
        }
        if matches!(self.options.repulsion_mode, RepulsionMode::BarnesHut) {
            state.rebuild_and_upload_octree(&od.queue);
        } else {
            state.n_octree_used = 0;
        }
        let mut steps_done = 0u32;
        let total_steps = self.options.steps_per_call.max(1);
        for s in 0..total_steps {
            // Re-write params per step so step_index advances under
            // negative sampling (the WGSL PRNG keys off it). Grid path
            // ignores step_index but the write is cheap.
            state.write_params(&od.queue, &self.options, self.step_index);
            self.step_index = self.step_index.wrapping_add(1);
            let build_grid = s == 0 && use_grid;
            state.dispatch_step_direct(&od.device, &od.queue, build_grid);
            state.swap_position_buffers();
            steps_done += 1;
        }
        let _ = steps_done;
        let positions = state.read_positions_owned(&od.device, &od.queue).await?;
        // Mirror back into our CPU position cache for the next grid build.
        if positions.len() == state.cpu_positions.len() {
            state.cpu_positions.copy_from_slice(&positions);
        }
        // Write back into the graph in the same id-order we built the buffer.
        for (id, p) in state.node_order.iter().zip(positions.chunks_exact(4)) {
            if let Some(node) = graph.nodes.get_mut(id) {
                node.position3 = Some([p[0], p[1], p[2]]);
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
        let state = GpuState::new_borrowed(device, graph, positions_buffer, &self.options)?;
        state.upload_initial_positions_to(queue, positions_buffer);
        self.state = Some(state);
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

        // Negative sampling skips the grid + bucket sort entirely (no
        // bbox, no atomics phase). That elision is the cost win.
        let use_grid = matches!(self.options.repulsion_mode, RepulsionMode::Grid)
            && self.options.grid_enabled;
        if use_grid {
            state.rebuild_and_upload_grid(device, queue, &self.options);
        }
        // Build the BH octree CPU-side once per call (matches the grid's
        // "build once per call" cadence). The shader sees a freshly-uploaded
        // tree in `oct_nodes_buf` and `params.n_octree`. v2 will move this
        // to GPU via the build kernels in shaders/octree.wgsl.
        if matches!(self.options.repulsion_mode, RepulsionMode::BarnesHut) {
            state.rebuild_and_upload_octree(queue);
        } else {
            state.n_octree_used = 0;
        }
        // First write_params with the *current* step_index — re-written
        // per inner step below so the WGSL PRNG advances under negative
        // sampling.
        state.write_params(queue, &self.options, self.step_index);
        if use_grid {
            // GPU-side bucket sort of positions into spatial-hash cells.
            // Done once per call (not per step), reading from whichever
            // buffer is currently the "in" side of the ping-pong.
            state.encode_grid_build_borrowed(device, encoder, shared_buffer);
        }
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

// ---------- Internal GPU state ----------------------------------------------

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
    grid_cell_size: f32,
    grid_enabled: u32,

    grid_origin: [f32; 3],
    n_cells: u32,

    grid_dim: [u32; 3],
    repulsion_mode: u32,

    bh_theta: f32,
    n_octree: u32,
    repulsion_samples: u32,   // K — only consulted when repulsion_mode == 2.
    step_index: u32,          // PRNG seed component for negative sampling.
}

// Each vec3<f32> in a storage buffer occupies 16 bytes (vec3 has stride/align
// of 16 in WGSL). We use a 4-component layout on the CPU side to match.
const VEC3_STRIDE: u64 = 16;

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

    /// Pipelines for the GPU-side spatial-grid bucket sort. All four share
    /// `gb_bind_group_layout`.
    gb_clear_pipeline: wgpu::ComputePipeline,
    gb_count_pipeline: wgpu::ComputePipeline,
    gb_scan_pipeline: wgpu::ComputePipeline,
    gb_scatter_pipeline: wgpu::ComputePipeline,
    gb_bind_group_layout: wgpu::BindGroupLayout,

    positions: PositionsStorage,
    /// True while pos_a is the "in" and pos_b is the "out" buffer.
    a_is_in: bool,
    velocities: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    edge_neighbors: wgpu::Buffer,
    /// Hub-aware (Tigr) virtual-vertex CSR + per-virtual spring partials.
    /// Built once at GpuState init from `edge_offsets`/`edge_neighbors`. See
    /// `HUB_THRESHOLD` and `spring_step` in shaders/force.wgsl.
    virt_real_idx_buf: wgpu::Buffer,
    virt_edge_offsets_buf: wgpu::Buffer,
    node_to_virt_offsets_buf: wgpu::Buffer,
    spring_force_partial_buf: wgpu::Buffer,
    n_virtual: u32,
    spring_bind_group_layout: wgpu::BindGroupLayout,
    spring_pipeline: wgpu::ComputePipeline,
    params_buf: wgpu::Buffer,
    /// Per-node mass (1 + log2(degree)). Static once built.
    mass_buf: wgpu::Buffer,
    /// Spatial-hash cells. (Re)allocated when capacity grows.
    cell_offsets_buf: wgpu::Buffer,
    cell_offsets_capacity: u64, // bytes
    cell_nodes_buf: wgpu::Buffer,
    cell_nodes_capacity: u64,
    /// Per-cell atomic counts (filled by count_cells, scanned into offsets).
    cell_counts_buf: wgpu::Buffer,
    cell_counts_capacity: u64,
    /// Per-cell atomic write cursor used by scatter_cells.
    cell_write_cursor_buf: wgpu::Buffer,
    cell_write_cursor_capacity: u64,
    /// Per-node KE proxy = |vel|^2 written by the shader; CPU reads back
    /// (small) for energy_threshold checks.
    energy_buf: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
    /// Barnes-Hut octree storage. Sized for ≤2N+8 OctNode slots so a
    /// pathological build (one body per leaf) still fits. Built CPU-side
    /// in v1; v2 will populate via the GPU build kernels in octree.wgsl.
    oct_nodes_buf: wgpu::Buffer,
    oct_nodes_capacity: u64,
    /// Group(2) BGL referenced by `force_step` whenever BH mode is
    /// active. Bound even in Grid mode (the shader has the binding
    /// declared, so it must be present in the bind group) — we just
    /// fill it with a 1-slot sentinel buffer.
    oct_bind_group_layout: wgpu::BindGroupLayout,
    /// Number of valid octree slots populated last build. 0 = no tree.
    n_octree_used: u32,
    /// Reusable CPU build scratch — kept across frames to avoid
    /// per-frame allocations during the per-step octree rebuild.
    oct_build: OctreeBuild,
    /// Staging buffer for CPU readback. Only allocated in the owned path
    /// and on-demand for the borrowed path's `read_back_positions`.
    staging: Option<wgpu::Buffer>,

    n_nodes: u32,
    n_edges: u32,
    pos_buf_size: u64,

    /// Initial (CPU-built) positions, kept around so the borrowed-mode path
    /// can seed the shared buffer via `queue.write_buffer` after init.
    initial_positions: Vec<f32>,

    /// CPU-side mirror of latest positions, used to rebuild the grid each
    /// step without a GPU readback.
    cpu_positions: Vec<f32>,
    /// CPU-side mirror of per-node mass (1 + log2(degree)). Used by the
    /// CPU octree builder; kept here so we don't have to read back from
    /// the `mass_buf` GPU buffer each frame.
    cpu_mass: Vec<f32>,

    /// Last-built grid metadata (mirrored into params each step).
    grid_origin: [f32; 3],
    grid_cell_size: f32,
    grid_dim: [u32; 3],
    n_cells: u32,

    /// Stable node-id ordering used to interpret the position buffer.
    node_order: Vec<String>,

    /// Effective damping currently in use; cooled per call.
    effective_damping: f32,

    /// Async energy-readback state. Shared with the wgpu map_async callback.
    /// On native, drained inside `step_with_encoder` after `device.poll(Poll)`;
    /// on WASM, the browser drives the callback between rAF ticks.
    energy_readback: Arc<Mutex<EnergyReadback>>,
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
    node_order: Vec<String>,
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

fn precompute(graph: &Graph, seed_mode: &SeedMode, spring_len: f32) -> PreCompute {
    let n_nodes = graph.nodes.len() as u32;
    let n_edges = graph.edges.len() as u32;

    let mut node_order: Vec<String> = graph.nodes.keys().cloned().collect();
    node_order.sort();
    let id_to_idx: std::collections::HashMap<&str, u32> = node_order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i as u32))
        .collect();

    let radius = ((n_nodes as f32).max(1.0).sqrt()) * 5.0;
    // Compute the seed in flat xyz form (the seeders share a buffer format),
    // then expand to vec4-padded `[x,y,z,0]` for the GPU. Nodes with an
    // author-supplied `position3` override the seeder for that slot.
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
            // Build a flat undirected edge list in id-sorted node order.
            let mut edges: Vec<u32> = Vec::with_capacity(graph.edges.len() * 2);
            for e in graph.edges.values() {
                let (Some(&s), Some(&t)) = (
                    id_to_idx.get(e.source.as_str()),
                    id_to_idx.get(e.target.as_str()),
                ) else {
                    continue;
                };
                if s == t {
                    continue;
                }
                edges.push(s);
                edges.push(t);
            }
            crate::layout::topo_fisheye::seed_positions(
                n_nodes as usize,
                &edges,
                spring_len.max(1.0),
                0x9E37_79B1,
                &crate::layout::topo_fisheye::CoarsenParams::default(),
            )
        }
    };
    // Defensive: if the seeder returned the wrong length (e.g. empty graph
    // edge case), fall back to a zero ball so downstream sizing stays sane.
    if seeded.len() != 3 * n_nodes as usize {
        seeded = vec![0.0f32; 3 * n_nodes as usize];
    }

    let mut positions: Vec<f32> = Vec::with_capacity(n_nodes as usize * 4);
    for (idx, id) in node_order.iter().enumerate() {
        let n = &graph.nodes[id];
        let p = n.position3.unwrap_or_else(|| {
            [
                seeded[3 * idx],
                seeded[3 * idx + 1],
                seeded[3 * idx + 2],
            ]
        });
        positions.extend_from_slice(&[p[0], p[1], p[2], 0.0]);
    }
    let velocities: Vec<f32> = vec![0.0; n_nodes as usize * 4];

    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n_nodes as usize];
    for e in graph.edges.values() {
        let (Some(&s), Some(&t)) = (
            id_to_idx.get(e.source.as_str()),
            id_to_idx.get(e.target.as_str()),
        ) else {
            continue;
        };
        if s == t {
            continue;
        }
        adj[s as usize].push(t);
        adj[t as usize].push(s);
    }
    let mut edge_offsets: Vec<u32> = Vec::with_capacity(n_nodes as usize + 1);
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
    let mut virt_real_idx: Vec<u32> = Vec::with_capacity(n_nodes as usize);
    let mut virt_edge_offsets: Vec<u32> = Vec::with_capacity(n_nodes as usize + 1);
    let mut node_to_virt_offsets: Vec<u32> = Vec::with_capacity(n_nodes as usize + 1);
    virt_edge_offsets.push(0);
    node_to_virt_offsets.push(0);
    let mut virt_count: u32 = 0;
    for i in 0..n_nodes as usize {
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
        n_edges,
        node_order,
        initial_positions: positions,
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

// ---------- Spatial-hash grid (CPU build) -----------------------------------

/// Build a uniform 3D voxel grid over `positions` (length n*4, padded vec4).
/// Returns (origin, cell_size, dim, n_cells, cell_offsets, cell_nodes).
/// Caps `dim` at 64 per axis so memory stays bounded for crazy bboxes.
fn build_grid(
    positions: &[f32],
    n_nodes: u32,
    cell_size_in: f32,
) -> (
    [f32; 3],
    f32,
    [u32; 3],
    u32,
    Vec<u32>,
    Vec<u32>,
) {
    let n = n_nodes as usize;
    if n == 0 {
        return ([0.0; 3], 1.0, [1, 1, 1], 1, vec![0, 0], vec![0]);
    }
    // 1. bbox
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for k in 0..3 {
            let v = positions[i * 4 + k];
            if v < mn[k] { mn[k] = v; }
            if v > mx[k] { mx[k] = v; }
        }
    }
    if !mn[0].is_finite() {
        mn = [-1.0; 3];
        mx = [1.0; 3];
    }
    // pad bbox slightly so points on the max edge still land in the last cell
    let pad = (cell_size_in.max(1.0)) * 0.5;
    let origin = [mn[0] - pad, mn[1] - pad, mn[2] - pad];
    let extent = [
        (mx[0] - mn[0]) + 2.0 * pad,
        (mx[1] - mn[1]) + 2.0 * pad,
        (mx[2] - mn[2]) + 2.0 * pad,
    ];
    let cell_size = cell_size_in.max(1.0);
    const MAX_DIM: u32 = 64;
    let mut dim = [
        (((extent[0] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
        (((extent[1] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
        (((extent[2] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
    ];
    // If we capped, expand effective cell size so all points still fit.
    let mut eff_cell = cell_size;
    for k in 0..3 {
        let needed = (extent[k] / dim[k] as f32).max(1e-3);
        if needed > eff_cell {
            eff_cell = needed;
        }
    }
    // recompute dims with eff_cell to keep grid covering bbox precisely
    for k in 0..3 {
        dim[k] = (((extent[k] / eff_cell).ceil()) as u32).max(1).min(MAX_DIM);
    }
    let n_cells = dim[0] * dim[1] * dim[2];
    let inv = 1.0 / eff_cell;

    // 2. count per cell
    let mut counts = vec![0u32; n_cells as usize];
    let mut node_cell = vec![0u32; n];
    for i in 0..n {
        let mut ix = ((positions[i * 4] - origin[0]) * inv) as i32;
        let mut iy = ((positions[i * 4 + 1] - origin[1]) * inv) as i32;
        let mut iz = ((positions[i * 4 + 2] - origin[2]) * inv) as i32;
        if ix < 0 { ix = 0; } else if ix >= dim[0] as i32 { ix = dim[0] as i32 - 1; }
        if iy < 0 { iy = 0; } else if iy >= dim[1] as i32 { iy = dim[1] as i32 - 1; }
        if iz < 0 { iz = 0; } else if iz >= dim[2] as i32 { iz = dim[2] as i32 - 1; }
        let cell =
            ix as u32 + iy as u32 * dim[0] + iz as u32 * dim[0] * dim[1];
        node_cell[i] = cell;
        counts[cell as usize] += 1;
    }
    // 3. prefix sum
    let mut cell_offsets = vec![0u32; n_cells as usize + 1];
    let mut acc = 0u32;
    for c in 0..n_cells as usize {
        cell_offsets[c] = acc;
        acc += counts[c];
    }
    cell_offsets[n_cells as usize] = acc;
    // 4. scatter
    let mut cursor = cell_offsets.clone();
    let mut cell_nodes = vec![0u32; n];
    for i in 0..n {
        let c = node_cell[i] as usize;
        cell_nodes[cursor[c] as usize] = i as u32;
        cursor[c] += 1;
    }

    (origin, eff_cell, dim, n_cells, cell_offsets, cell_nodes)
}

struct ForcePipelines {
    force_step: wgpu::ComputePipeline,
    force_bgl: wgpu::BindGroupLayout,
    gb_clear: wgpu::ComputePipeline,
    gb_count: wgpu::ComputePipeline,
    gb_scan: wgpu::ComputePipeline,
    gb_scatter: wgpu::ComputePipeline,
    gb_bgl: wgpu::BindGroupLayout,
    /// Group(2) for force_step: the octree storage buffer. Must be bound
    /// for both Grid and BarnesHut paths (WGSL requires every declared
    /// binding to be present); the Grid path simply doesn't touch it.
    oct_bgl: wgpu::BindGroupLayout,
    /// Group(3): hub-aware (Tigr) virtual-vertex CSR + per-virtual spring
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
        storage_entry(6, true),  // cell_offsets
        storage_entry(7, true),  // cell_nodes
        storage_entry(8, true),  // mass
        storage_entry(9, false), // energy_out
    ];
    let bind_group_layout = device.create_bind_group_layout(
        &wgpu::BindGroupLayoutDescriptor {
            label: Some("gpu_force_bgl"),
            entries: &bgl_entries,
        },
    );

    // Grid-build BGL (group 1 in shader). All four build entry points share
    // it; bindings an entry point doesn't reference are simply unused.
    let gb_bgl_entries = [
        storage_entry(0, true), // gb_positions_in
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        storage_entry(2, false), // gb_cell_counts (atomic rw)
        storage_entry(3, false), // gb_cell_cursor (atomic rw)
        storage_entry(4, false), // gb_cell_offsets (rw u32)
        storage_entry(5, false), // gb_cell_nodes   (rw u32)
    ];
    let gb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_grid_build_bgl"),
        entries: &gb_bgl_entries,
    });
    // Empty BGL placeholder at group(0) for build pipelines (the build
    // entry points only reference @group(1) bindings).
    let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_grid_build_empty_bgl"),
        entries: &[],
    });

    // Group 2: octree storage. Single read-only storage buffer at @binding(1)
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
    // Empty BGL placeholder for group(1) when binding force_step (which
    // declares both group(0) [main] and group(2) [octree], plus group(1)
    // bindings the grid-build pipelines own).
    let force_empty_gb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_force_empty_gb_bgl"),
        entries: &[],
    });

    // Group 3: hub-aware virtual-vertex CSR + per-virtual spring partials.
    // Bound by both `spring_step` (writes partials) and `force_step` (reads).
    let spring_bgl_entries = [
        storage_entry(0, true),  // virt_real_idx
        storage_entry(1, true),  // virt_edge_offsets
        storage_entry(2, false), // spring_force_partial (rw)
        storage_entry(3, true),  // node_to_virt_offsets
    ];
    let spring_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_spring_bgl"),
        entries: &spring_bgl_entries,
    });

    let pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_pl"),
            // group 0 = main, group 1 = placeholder, group 2 = octree,
            // group 3 = hub-aware spring partials.
            bind_group_layouts: &[
                &bind_group_layout,
                &force_empty_gb_bgl,
                &oct_bgl,
                &spring_bgl,
            ],
            push_constant_ranges: &[],
        },
    );
    let force_step = device.create_compute_pipeline(
        &wgpu::ComputePipelineDescriptor {
            label: Some("force_step"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("force_step"),
            compilation_options: Default::default(),
            cache: None,
        },
    );

    let gb_pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_grid_build_pl"),
            bind_group_layouts: &[&empty_bgl, &gb_bgl],
            push_constant_ranges: &[],
        },
    );
    let mk = |name: &'static str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(name),
            layout: Some(&gb_pipeline_layout),
            module: &shader,
            entry_point: Some(name),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let gb_clear = mk("clear_cell_counts");
    let gb_count = mk("count_cells");
    let gb_scan = mk("scan_cell_offsets");
    let gb_scatter = mk("scatter_cells");

    // Spring kernel pipeline. Reuses the main BGL at group(0) (uses
    // positions_in/edge_neighbors/params; other entries unused). Octree
    // BGL at group(2) is unused but must match the layout — we still bind
    // the octree buffer at dispatch time.
    let spring_pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_spring_pl"),
            bind_group_layouts: &[
                &bind_group_layout,
                &force_empty_gb_bgl,
                &oct_bgl,
                &spring_bgl,
            ],
            push_constant_ranges: &[],
        },
    );
    let spring_step = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("spring_step"),
        layout: Some(&spring_pipeline_layout),
        module: &shader,
        entry_point: Some("spring_step"),
        compilation_options: Default::default(),
        cache: None,
    });

    ForcePipelines {
        force_step,
        force_bgl: bind_group_layout,
        gb_clear,
        gb_count,
        gb_scan,
        gb_scatter,
        gb_bgl,
        oct_bgl,
        spring_bgl,
        spring_step,
    }
}

impl GpuState {
    /// Build state with caller-supplied device + owned positions buffers.
    fn new_owned(
        device: &wgpu::Device,
        graph: &Graph,
        options: &GpuForceOptions,
    ) -> Result<Self, String> {
        let pc = precompute(graph, &options.seed_mode, options.spring_len);
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        let pos_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_a"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_b"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
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
        let cpu_positions = pc.initial_positions.clone();
        let cpu_mass = pc.mass.clone();

        Ok(Self {
            pipeline: pipelines.force_step,
            bind_group_layout: pipelines.force_bgl,
            gb_clear_pipeline: pipelines.gb_clear,
            gb_count_pipeline: pipelines.gb_count,
            gb_scan_pipeline: pipelines.gb_scan,
            gb_scatter_pipeline: pipelines.gb_scatter,
            gb_bind_group_layout: pipelines.gb_bgl,
            positions: PositionsStorage::Owned { pos_a, pos_b },
            a_is_in: true,
            velocities: aux.vel,
            edge_offsets: aux.off,
            edge_neighbors: aux.neigh,
            virt_real_idx_buf: aux.virt_real_idx,
            virt_edge_offsets_buf: aux.virt_edge_offsets,
            node_to_virt_offsets_buf: aux.node_to_virt_offsets,
            spring_force_partial_buf: aux.spring_force_partial,
            n_virtual: aux.n_virtual,
            spring_bind_group_layout: pipelines.spring_bgl,
            spring_pipeline: pipelines.spring_step,
            params_buf: aux.params,
            mass_buf: aux.mass,
            cell_offsets_buf: aux.cell_offsets,
            cell_offsets_capacity: aux.cell_offsets_capacity,
            cell_nodes_buf: aux.cell_nodes,
            cell_nodes_capacity: aux.cell_nodes_capacity,
            cell_counts_buf: aux.cell_counts,
            cell_counts_capacity: aux.cell_counts_capacity,
            cell_write_cursor_buf: aux.cell_write_cursor,
            cell_write_cursor_capacity: aux.cell_write_cursor_capacity,
            energy_buf: aux.energy,
            energy_staging: aux.energy_staging,
            oct_nodes_buf: aux.oct_nodes,
            oct_nodes_capacity: aux.oct_nodes_capacity,
            oct_bind_group_layout: pipelines.oct_bgl,
            n_octree_used: 0,
            oct_build: OctreeBuild::default(),
            staging: Some(staging),
            n_nodes: pc.n_nodes,
            n_edges: pc.n_edges,
            pos_buf_size,
            initial_positions: pc.initial_positions,
            cpu_positions,
            cpu_mass,
            grid_origin: [0.0; 3],
            grid_cell_size: 1.0,
            grid_dim: [1, 1, 1],
            n_cells: 1,
            node_order: pc.node_order,
            effective_damping: 1.0,
            energy_readback: Arc::new(Mutex::new(EnergyReadback::Idle)),
        })
    }

    /// Build state against caller-supplied device + a borrowed positions
    /// storage buffer (typically owned by the renderer). We don't take the
    /// queue here — the caller passes it to `upload_initial_positions` and
    /// `step_with_encoder`. This avoids cloning wgpu::Queue.
    fn new_borrowed(
        device: &wgpu::Device,
        graph: &Graph,
        positions_buffer: &wgpu::Buffer,
        options: &GpuForceOptions,
    ) -> Result<Self, String> {
        let pc = precompute(graph, &options.seed_mode, options.spring_len);
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        // Internal ping-pong target. COPY_SRC so we can copy back to the
        // shared buffer; COPY_DST so we can seed it.
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_internal_b"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let aux = build_aux_buffers(device, &pc);
        let pipelines = build_pipeline(device);

        let _ = positions_buffer; // sized check happens via caller usage
        let cpu_positions = pc.initial_positions.clone();
        let cpu_mass = pc.mass.clone();

        Ok(Self {
            pipeline: pipelines.force_step,
            bind_group_layout: pipelines.force_bgl,
            gb_clear_pipeline: pipelines.gb_clear,
            gb_count_pipeline: pipelines.gb_count,
            gb_scan_pipeline: pipelines.gb_scan,
            gb_scatter_pipeline: pipelines.gb_scatter,
            gb_bind_group_layout: pipelines.gb_bgl,
            positions: PositionsStorage::Borrowed { pos_b },
            a_is_in: true,
            velocities: aux.vel,
            edge_offsets: aux.off,
            edge_neighbors: aux.neigh,
            virt_real_idx_buf: aux.virt_real_idx,
            virt_edge_offsets_buf: aux.virt_edge_offsets,
            node_to_virt_offsets_buf: aux.node_to_virt_offsets,
            spring_force_partial_buf: aux.spring_force_partial,
            n_virtual: aux.n_virtual,
            spring_bind_group_layout: pipelines.spring_bgl,
            spring_pipeline: pipelines.spring_step,
            params_buf: aux.params,
            mass_buf: aux.mass,
            cell_offsets_buf: aux.cell_offsets,
            cell_offsets_capacity: aux.cell_offsets_capacity,
            cell_nodes_buf: aux.cell_nodes,
            cell_nodes_capacity: aux.cell_nodes_capacity,
            cell_counts_buf: aux.cell_counts,
            cell_counts_capacity: aux.cell_counts_capacity,
            cell_write_cursor_buf: aux.cell_write_cursor,
            cell_write_cursor_capacity: aux.cell_write_cursor_capacity,
            energy_buf: aux.energy,
            energy_staging: aux.energy_staging,
            oct_nodes_buf: aux.oct_nodes,
            oct_nodes_capacity: aux.oct_nodes_capacity,
            oct_bind_group_layout: pipelines.oct_bgl,
            n_octree_used: 0,
            oct_build: OctreeBuild::default(),
            staging: None,
            n_nodes: pc.n_nodes,
            n_edges: pc.n_edges,
            pos_buf_size,
            initial_positions: pc.initial_positions,
            cpu_positions,
            cpu_mass,
            grid_origin: [0.0; 3],
            grid_cell_size: 1.0,
            grid_dim: [1, 1, 1],
            n_cells: 1,
            node_order: pc.node_order,
            effective_damping: 1.0,
            energy_readback: Arc::new(Mutex::new(EnergyReadback::Idle)),
        })
    }

    /// Seed the shared (borrowed) positions buffer with our initial values.
    /// Caller must supply the same shared buffer that was passed to
    /// `new_borrowed`.
    fn upload_initial_positions_to(&self, queue: &wgpu::Queue, shared: &wgpu::Buffer) {
        queue.write_buffer(shared, 0, bytemuck::cast_slice(&self.initial_positions));
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
            grid_cell_size: self.grid_cell_size,
            grid_enabled: if opts.grid_enabled { 1 } else { 0 },
            grid_origin: self.grid_origin,
            n_cells: self.n_cells,
            grid_dim: self.grid_dim,
            repulsion_mode: opts.repulsion_mode.as_u32(),
            bh_theta: opts.theta.clamp(0.1, 2.0),
            n_octree: self.n_octree_used,
            repulsion_samples: opts.repulsion_samples.max(1),
            step_index,
        };
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&raw));
    }

    /// Build the spatial-hash grid from `cpu_positions`, (re)allocate the
    /// cell buffers if needed, and upload to GPU.
    fn rebuild_and_upload_grid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        opts: &GpuForceOptions,
    ) {
        let _ = queue;
        let cell_size_target = if opts.repulsion_radius > 0.0 {
            opts.repulsion_radius
        } else {
            (opts.spring_len * 4.0).max(1.0)
        };
        // Bbox + dims still computed CPU-side from the (possibly stale)
        // cpu_positions mirror — same as before this refactor. The
        // count+scatter use the *fresh* GPU positions buffer, so a slightly
        // stale bbox just means a slightly wider grid, which is harmless
        // (the in-shader clamp keeps every node in a valid cell).
        let (origin, cell_size, dim, n_cells, _co, _cn) =
            build_grid(&self.cpu_positions, self.n_nodes, cell_size_target);
        self.grid_origin = origin;
        self.grid_cell_size = cell_size;
        self.grid_dim = dim;
        self.n_cells = n_cells;

        // (Re)allocate cell-* buffers if the grid grew. We size everything
        // to (n_cells + 1) * 4 — cell_offsets needs the +1 sentinel; the
        // count/cursor buffers don't but it's fine to oversize.
        let needed_off_bytes = ((n_cells as u64 + 1) * 4).max(64);
        if needed_off_bytes > self.cell_offsets_capacity {
            let cap = needed_off_bytes * 2;
            self.cell_offsets_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_offsets"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_offsets_capacity = cap;
            // counts/cursor track cell_offsets capacity so all three resize
            // together — the atomic buffers can't be smaller than n_cells*4.
            self.cell_counts_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_counts"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.cell_counts_capacity = cap;
            self.cell_write_cursor_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_write_cursor"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.cell_write_cursor_capacity = cap;
        }
        let needed_nodes_bytes = (self.n_nodes.max(1) as u64 * 4).max(64);
        if needed_nodes_bytes > self.cell_nodes_capacity {
            let cap = needed_nodes_bytes * 2;
            self.cell_nodes_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_nodes"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_nodes_capacity = cap;
        }
    }

    /// Build the gb_* bind group used by the four grid-build entry points.
    /// `pos_in` is the same positions buffer the upcoming `force_step` will
    /// read — that way the grid is built from the same positions force_step
    /// sees (no one-frame stale grid).
    fn make_grid_build_bg(
        &self,
        device: &wgpu::Device,
        pos_in: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_grid_build_bg"),
            layout: &self.gb_bind_group_layout,
            entries: &[
                buf_entry(0, pos_in),
                buf_entry(1, &self.params_buf),
                buf_entry(2, &self.cell_counts_buf),
                buf_entry(3, &self.cell_write_cursor_buf),
                buf_entry(4, &self.cell_offsets_buf),
                buf_entry(5, &self.cell_nodes_buf),
            ],
        })
    }

    /// Record the four grid-build dispatches (clear → count → scan → scatter)
    /// into `encoder`. After this returns, cell_offsets + cell_nodes hold a
    /// fresh bucket sort of `pos_in`. Each pass is its own compute pass so
    /// wgpu inserts the necessary storage-buffer barriers between them.
    fn encode_grid_build(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bg: &wgpu::BindGroup,
    ) {
        let cells_groups = (self.n_cells + 63) / 64;
        let nodes_groups = (self.n_nodes + 63) / 64;
        // 1. clear counts + cursor + offsets
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_clear_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_clear_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(cells_groups.max(1), 1, 1);
        }
        // 2. count per cell (atomic add)
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_count_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_count_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(nodes_groups.max(1), 1, 1);
        }
        // 3. exclusive prefix sum, single workgroup, single thread
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_scan_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_scan_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(1, 1, 1);
        }
        // 4. scatter node indices into cell_nodes via cursor atomicAdd
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_scatter_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_scatter_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(nodes_groups.max(1), 1, 1);
        }
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
                buf_entry(6, &self.cell_offsets_buf),
                buf_entry(7, &self.cell_nodes_buf),
                buf_entry(8, &self.mass_buf),
                buf_entry(9, &self.energy_buf),
            ],
        })
    }

    /// Direct dispatch — owns its own encoder and submits immediately.
    /// Used by the legacy `run()` path (owned mode only). `build_grid`
    /// controls whether the grid bucket sort runs in the same submit
    /// (called once for the first step per call; subsequent steps within
    /// the same `run()` reuse the grid for symmetry with the borrowed
    /// path's "build once per call").
    fn dispatch_step_direct(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        build_grid: bool,
    ) {
        let (pos_in, pos_out) = self.owned_in_out();
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_cmd"),
        });
        if build_grid {
            let gb_bg = self.make_grid_build_bg(device, pos_in);
            self.encode_grid_build(&mut encoder, &gb_bg);
        }
        let oct_bg = self.make_oct_bind_group(device);
        let spring_bg = self.make_spring_bind_group(device);
        self.encode_spring_step(&mut encoder, &bind_group, &oct_bg, &spring_bg);
        self.encode_compute(&mut encoder, &bind_group, &oct_bg, &spring_bg);
        queue.submit(Some(encoder.finish()));
    }

    /// Record dispatch into a caller-supplied encoder, reading/writing the
    /// borrowed shared buffer + internal pos_b. Caller is responsible for
    /// invoking `encode_grid_build_borrowed` before this if grid is enabled
    /// — `step_with_encoder` does that once per call (not per step).
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

    /// Borrowed-mode wrapper around `encode_grid_build` — builds the bind
    /// group bound to whichever position buffer is currently the "in" side.
    fn encode_grid_build_borrowed(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shared: &wgpu::Buffer,
    ) {
        let (pos_in, _pos_out) = self.borrowed_in_out(shared);
        let bg = self.make_grid_build_bg(device, pos_in);
        self.encode_grid_build(encoder, &bg);
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
        // group(1) unused by force_step.
        cpass.set_bind_group(2, oct_bg, &[]);
        cpass.set_bind_group(3, spring_bg, &[]);
        let groups = (self.n_nodes + 63) / 64;
        cpass.dispatch_workgroups(groups.max(1), 1, 1);
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
                buf_entry(0, &self.virt_real_idx_buf),
                buf_entry(1, &self.virt_edge_offsets_buf),
                buf_entry(2, &self.spring_force_partial_buf),
                buf_entry(3, &self.node_to_virt_offsets_buf),
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
        cpass.set_bind_group(2, oct_bg, &[]);
        cpass.set_bind_group(3, spring_bg, &[]);
        let groups = (self.n_virtual + 63) / 64;
        cpass.dispatch_workgroups(groups.max(1), 1, 1);
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
    virt_real_idx: wgpu::Buffer,
    virt_edge_offsets: wgpu::Buffer,
    node_to_virt_offsets: wgpu::Buffer,
    spring_force_partial: wgpu::Buffer,
    n_virtual: u32,
    params: wgpu::Buffer,
    mass: wgpu::Buffer,
    cell_offsets: wgpu::Buffer,
    cell_offsets_capacity: u64,
    cell_nodes: wgpu::Buffer,
    cell_nodes_capacity: u64,
    cell_counts: wgpu::Buffer,
    cell_counts_capacity: u64,
    cell_write_cursor: wgpu::Buffer,
    cell_write_cursor_capacity: u64,
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
        contents: bytemuck::cast_slice(&pc.velocities),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let off = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_offsets"),
        contents: bytemuck::cast_slice(&pc.edge_offsets),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let neigh = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_neighbors"),
        contents: bytemuck::cast_slice(&pc.edge_neighbors),
        usage: wgpu::BufferUsages::STORAGE,
    });
    // Hub-aware (Tigr) virtual-vertex CSR + per-virtual spring partials.
    let virt_real_idx = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("virt_real_idx"),
        contents: bytemuck::cast_slice(&pc.virt_real_idx),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let virt_edge_offsets = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("virt_edge_offsets"),
        contents: bytemuck::cast_slice(&pc.virt_edge_offsets),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let node_to_virt_offsets = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("node_to_virt_offsets"),
        contents: bytemuck::cast_slice(&pc.node_to_virt_offsets),
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
        contents: bytemuck::cast_slice(&pc.mass),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    // Initial capacity: enough for a 1-cell grid + the n nodes. Will grow.
    let init_cell_offsets = vec![0u32, pc.n_nodes];
    let cell_offsets_capacity =
        (init_cell_offsets.len() as u64 * 4).max(64);
    let cell_offsets = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_offsets"),
        size: cell_offsets_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let n = pc.n_nodes.max(1) as u64;
    let cell_nodes_capacity = (n * 4).max(64);
    let cell_nodes = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_nodes"),
        size: cell_nodes_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // GPU-side bucket-sort scratch: per-cell atomic counts + write cursor.
    // Sized to match cell_offsets at construction; both grow alongside it.
    let cell_counts_capacity = cell_offsets_capacity;
    let cell_counts = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_counts"),
        size: cell_counts_capacity,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let cell_write_cursor_capacity = cell_offsets_capacity;
    let cell_write_cursor = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_write_cursor"),
        size: cell_write_cursor_capacity,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
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
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    AuxBuffers {
        vel,
        off,
        neigh,
        virt_real_idx,
        virt_edge_offsets,
        node_to_virt_offsets,
        spring_force_partial,
        n_virtual: pc.n_virtual,
        params,
        mass,
        cell_offsets,
        cell_offsets_capacity,
        cell_nodes,
        cell_nodes_capacity,
        cell_counts,
        cell_counts_capacity,
        cell_write_cursor,
        cell_write_cursor_capacity,
        energy,
        energy_staging,
        oct_nodes,
        oct_nodes_capacity,
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

// ---------- Barnes-Hut octree (CPU build, v1) -------------------------------
//
// 4-byte-per-field layout matching `OctNode` in force.wgsl. We use three
// vec4 chunks for predictable WGSL alignment (each vec4 is 16-byte aligned):
//
//   pos_size: (cx, cy, cz, half_extent)
//   com_mass: (com_x, com_y, com_z, mass)
//   meta:     (body_idx | OCT_BODY_INTERNAL, next_idx, skip_idx, child_count)
//
// next_idx / skip_idx form the "rope": next is the first child in DFS order
// (or OCT_END for leaves); skip is the next-sibling-or-uncle to jump to once
// the subtree has been processed (or accepted under the BH criterion).
// Sentinel OCT_END = u32::MAX terminates the traversal.
//
// v1 build is recursive on CPU. v2 will move to the GPU build kernels in
// shaders/octree.wgsl (bbox_reduce → morton_assign → octree_build →
// com_aggregate). The on-wire layout is shared so v2 only changes who
// fills the buffer, not what the shader reads.
const OCT_END: u32 = u32::MAX;
const OCT_BODY_INTERNAL: u32 = u32::MAX;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct OctNodeRaw {
    pos_size: [f32; 4],
    com_mass: [f32; 4],
    meta: [u32; 4],
}

/// Per-octree-build scratch. Reused across frames to avoid per-step
/// allocations during the rebuild.
#[derive(Default)]
struct OctreeBuild {
    nodes: Vec<OctNodeRaw>,
    /// children indices for each internal node (8 per node, OCT_END = empty).
    children: Vec<[u32; 8]>,
    /// upload staging — cleared and refilled each rebuild.
    upload: Vec<OctNodeRaw>,
}

impl OctreeBuild {
    /// Build the octree in-place from `positions` (padded vec4 stride),
    /// `mass`, and the body count. Returns the number of populated nodes.
    /// On overflow (capacity exceeded) returns 0 and leaves nodes empty —
    /// the shader sees an empty tree and reads no force.
    fn rebuild(
        &mut self,
        positions: &[f32],
        mass: &[f32],
        n_bodies: u32,
        max_nodes: u32,
    ) -> u32 {
        self.nodes.clear();
        self.children.clear();
        self.upload.clear();
        if n_bodies == 0 || positions.len() < (n_bodies as usize) * 4 {
            return 0;
        }
        // 1. Compute world bbox.
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for i in 0..n_bodies as usize {
            for k in 0..3 {
                let v = positions[i * 4 + k];
                if !v.is_finite() { continue; }
                if v < mn[k] { mn[k] = v; }
                if v > mx[k] { mx[k] = v; }
            }
        }
        if !mn[0].is_finite() {
            mn = [-1.0; 3];
            mx = [1.0; 3];
        }
        let center = [
            0.5 * (mn[0] + mx[0]),
            0.5 * (mn[1] + mx[1]),
            0.5 * (mn[2] + mx[2]),
        ];
        let mut half = ((mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2])) * 0.5;
        if !half.is_finite() || half <= 0.0 { half = 1.0; }
        // pad slightly so points on the bbox edge land inside the root.
        half *= 1.05_f32.max(1.0);

        // 2. Allocate root.
        let push_internal = |nodes: &mut Vec<OctNodeRaw>, children: &mut Vec<[u32; 8]>, c: [f32; 3], h: f32| -> u32 {
            let idx = nodes.len() as u32;
            nodes.push(OctNodeRaw {
                pos_size: [c[0], c[1], c[2], h],
                com_mass: [0.0, 0.0, 0.0, 0.0],
                meta: [OCT_BODY_INTERNAL, OCT_END, OCT_END, 0],
            });
            children.push([OCT_END; 8]);
            idx
        };
        push_internal(&mut self.nodes, &mut self.children, center, half);

        // 3. Insert each body.
        // Iterative insert to avoid recursion-depth pitfalls on degenerate
        // (collinear) input.
        for body in 0..n_bodies {
            let bx = positions[body as usize * 4];
            let by = positions[body as usize * 4 + 1];
            let bz = positions[body as usize * 4 + 2];
            let bm = mass.get(body as usize).copied().unwrap_or(1.0).max(1e-3);
            if !(bx.is_finite() && by.is_finite() && bz.is_finite()) { continue; }
            if self.insert_body(0, [bx, by, bz], body, bm, max_nodes).is_err() {
                // overflow: stop here; partial tree is still valid (just
                // misses some bodies, which means slightly weaker repulsion
                // for them — better than crashing).
                break;
            }
        }

        // 4. Aggregate COM/mass + assign next/skip ropes via iterative
        // post-order DFS. We do COM first (post-order: children before
        // parents) and then ropes (pre-order with sibling stack).
        self.aggregate_com_postorder();
        self.assign_ropes();

        // 5. Pack into upload buffer (it IS our nodes vec — but assert
        // capacity).
        let n = self.nodes.len() as u32;
        if n > max_nodes { return 0; }
        n
    }

    /// Octant index 0..=7 from sign bits (x=lsb, y, z=msb).
    fn octant_for(center: &[f32; 4], p: [f32; 3]) -> u32 {
        let mut o = 0u32;
        if p[0] >= center[0] { o |= 1; }
        if p[1] >= center[1] { o |= 2; }
        if p[2] >= center[2] { o |= 4; }
        o
    }
    fn child_center(parent_center: &[f32; 4], oct: u32) -> ([f32; 3], f32) {
        let h = parent_center[3] * 0.5;
        let cx = parent_center[0] + if (oct & 1) != 0 { h } else { -h };
        let cy = parent_center[1] + if (oct & 2) != 0 { h } else { -h };
        let cz = parent_center[2] + if (oct & 4) != 0 { h } else { -h };
        ([cx, cy, cz], h)
    }

    fn insert_body(
        &mut self,
        root: u32,
        p: [f32; 3],
        body_idx: u32,
        body_mass: f32,
        max_nodes: u32,
    ) -> Result<(), ()> {
        let mut node_idx = root;
        // Bounded depth: octree half-extent halves per level; at f32
        // precision ~24 bits we lose meaning past ~30 levels. Cap to keep
        // the loop finite even on perfectly coincident points.
        for _depth in 0..32 {
            let center = self.nodes[node_idx as usize].pos_size;
            let oct = Self::octant_for(&center, p);
            let child_idx = self.children[node_idx as usize][oct as usize];

            if child_idx == OCT_END {
                // Empty slot — drop a leaf here.
                if (self.nodes.len() as u32) >= max_nodes { return Err(()); }
                let (cc, hh) = Self::child_center(&center, oct);
                let new_idx = self.nodes.len() as u32;
                self.nodes.push(OctNodeRaw {
                    pos_size: [cc[0], cc[1], cc[2], hh],
                    com_mass: [p[0], p[1], p[2], body_mass],
                    meta: [body_idx, OCT_END, OCT_END, 0],
                });
                self.children.push([OCT_END; 8]);
                self.children[node_idx as usize][oct as usize] = new_idx;
                self.nodes[node_idx as usize].meta[3] += 1;
                return Ok(());
            }

            // Slot occupied.
            let child_meta_x = self.nodes[child_idx as usize].meta[0];
            if child_meta_x == OCT_BODY_INTERNAL {
                // Descend into existing internal node.
                node_idx = child_idx;
                continue;
            }
            // Existing leaf — promote it to an internal node so we can
            // host both bodies underneath. Re-insert the previous body
            // first, then loop-continue to insert ours under the same
            // (now-internal) node.
            let prev_com = self.nodes[child_idx as usize].com_mass;
            let prev_body = self.nodes[child_idx as usize].meta[0];
            // Convert child_idx to an internal node in-place. Keep its
            // pos_size (center+half-extent) — those are the cell bounds.
            self.nodes[child_idx as usize].com_mass = [0.0, 0.0, 0.0, 0.0];
            self.nodes[child_idx as usize].meta[0] = OCT_BODY_INTERNAL;
            self.nodes[child_idx as usize].meta[3] = 0;
            // Re-insert the displaced body underneath child_idx.
            // NB: if the displaced body has the exact same position as the
            // new one we'd loop forever; the depth cap (32) breaks out.
            self.insert_body(
                child_idx,
                [prev_com[0], prev_com[1], prev_com[2]],
                prev_body,
                prev_com[3].max(1e-3),
                max_nodes,
            )?;
            // Now retry insertion of OUR body at this level — child_idx
            // is internal, so the next iteration will descend into it.
            node_idx = child_idx;
        }
        Ok(())
    }

    /// Iterative post-order traversal computing COM/mass on internal nodes
    /// from children. Leaves already have com_mass set at insertion.
    fn aggregate_com_postorder(&mut self) {
        // Stack of (node_idx, child_cursor); when child_cursor == 8 we pop
        // and aggregate. Iterative form to dodge stack overflow on tall
        // trees.
        if self.nodes.is_empty() { return; }
        let mut stack: Vec<(u32, u32)> = Vec::with_capacity(64);
        stack.push((0, 0));
        while let Some(&(idx, cursor)) = stack.last() {
            if self.nodes[idx as usize].meta[0] != OCT_BODY_INTERNAL {
                // Leaf — already has com_mass.
                stack.pop();
                continue;
            }
            if cursor < 8 {
                // Bump cursor and try to descend into this child.
                stack.last_mut().unwrap().1 = cursor + 1;
                let ch = self.children[idx as usize][cursor as usize];
                if ch != OCT_END {
                    stack.push((ch, 0));
                }
                continue;
            }
            // All children visited — aggregate.
            let mut total_mass = 0.0f32;
            let mut com = [0.0f32; 3];
            for k in 0..8 {
                let ch = self.children[idx as usize][k];
                if ch == OCT_END { continue; }
                let cm = self.nodes[ch as usize].com_mass;
                total_mass += cm[3];
                com[0] += cm[0] * cm[3];
                com[1] += cm[1] * cm[3];
                com[2] += cm[2] * cm[3];
            }
            if total_mass > 0.0 {
                com[0] /= total_mass;
                com[1] /= total_mass;
                com[2] /= total_mass;
            }
            self.nodes[idx as usize].com_mass = [com[0], com[1], com[2], total_mass];
            stack.pop();
        }
    }

    /// Pre-order DFS that fills next_idx (first child in DFS order) and
    /// skip_idx (next-sibling-or-uncle). Sentinel OCT_END terminates.
    /// This is the rope that lets the WGSL traversal be stackless.
    fn assign_ropes(&mut self) {
        if self.nodes.is_empty() { return; }
        // Walk pre-order using an explicit stack of (idx, parent_skip).
        // For each node we need to know its parent's skip target so we can
        // set our own skip when we have no more siblings. We also need to
        // know what "siblings" we have left at the parent level.
        //
        // Simpler approach: build the DFS order list with `skip_idx` as
        // "what to jump to after my entire subtree". For internal nodes,
        // next = first DFS child; skip = same as parent's skip (initially)
        // but corrected to point at the next sibling that exists.
        struct Frame {
            #[allow(dead_code)]
            node: u32,
            children_left: [u32; 8], // OCT_END for visited or empty
            // The skip target *if no more children remain at this parent*.
            outer_skip: u32,
        }
        // Compute skip for each node: do iterative traversal.
        let n = self.nodes.len();
        let mut skip = vec![OCT_END; n];
        let mut next = vec![OCT_END; n];

        let mut stack: Vec<Frame> = Vec::with_capacity(64);
        // Push the root with outer_skip = OCT_END.
        stack.push(Frame {
            node: 0,
            children_left: if self.nodes[0].meta[0] == OCT_BODY_INTERNAL {
                self.children[0]
            } else {
                [OCT_END; 8]
            },
            outer_skip: OCT_END,
        });

        // To assign ropes correctly we DFS-visit in order and remember the
        // most recently visited node, then patch its next_idx to the
        // current node when we descend.
        let mut prev_visited: Option<u32> = None;

        while let Some(_) = stack.last() {
            // First: emit the top-of-stack node if not yet emitted. We use
            // a side flag via `prev_visited` plus checking if `next` for
            // the top has been written. Simpler: on first peek, if
            // top.outer_skip is "fresh", emit.
            // We use the convention: a node is "emitted" the first time
            // we push it (handled below).
            let top = stack.last_mut().unwrap();
            // Find next live child.
            let mut next_child = OCT_END;
            for k in 0..8 {
                if top.children_left[k] != OCT_END {
                    next_child = top.children_left[k];
                    top.children_left[k] = OCT_END;
                    break;
                }
            }
            if next_child != OCT_END {
                // Patch the previously-visited node to point at this child
                // as its DFS-next. (It's either our previous sibling or our
                // parent — the rope says "after you, go here".)
                if let Some(prev) = prev_visited {
                    if next[prev as usize] == OCT_END {
                        next[prev as usize] = next_child;
                    }
                }
                // Determine outer_skip for this child: scan remaining
                // siblings; first non-empty is our skip, else the parent's
                // outer_skip.
                let mut child_outer_skip = top.outer_skip;
                for k in 0..8 {
                    if top.children_left[k] != OCT_END {
                        child_outer_skip = top.children_left[k];
                        break;
                    }
                }
                // Emit child: record its skip; descend into it.
                skip[next_child as usize] = child_outer_skip;
                let is_internal = self.nodes[next_child as usize].meta[0] == OCT_BODY_INTERNAL;
                stack.push(Frame {
                    node: next_child,
                    children_left: if is_internal { self.children[next_child as usize] } else { [OCT_END; 8] },
                    outer_skip: child_outer_skip,
                });
                prev_visited = Some(next_child);
                continue;
            }
            // No more children — this subtree is done. Pop.
            stack.pop();
        }

        // Patch root's skip + last-node's next.
        skip[0] = OCT_END;
        // Any node whose next is still OCT_END — meaning either a leaf
        // *or* an internal with no children (rare/impossible after our
        // build) — defaults to its skip (so traversal still terminates
        // cleanly).
        // Wait: leaves SHOULD have next = OCT_END. The traversal in WGSL
        // only follows next when descending into an internal node, and
        // leaves are always handled by skip. So leaves' next can stay
        // OCT_END.

        // Write back into nodes[].meta[1..3].
        for i in 0..n {
            self.nodes[i].meta[1] = next[i];
            self.nodes[i].meta[2] = skip[i];
        }
    }
}

impl GpuState {
    /// Build the BH octree from `cpu_positions` and upload it to
    /// `oct_nodes_buf`. Updates `n_octree_used` so the next params write
    /// reflects the new tree size. Caller should only invoke this when
    /// `repulsion_mode == BarnesHut` to avoid the per-step build cost.
    ///
    /// TODO(perf/correctness): `cpu_positions` is only refreshed by the
    /// legacy `run()` path's blocking readback; the renderer hot path
    /// (`step_with_encoder`) leaves it at the initial seed. This means
    /// the BH octree is rebuilt from stale data and forces are computed
    /// against the seed layout, which causes the sim to settle into a
    /// degenerate configuration almost immediately. Either (a) move the
    /// build to GPU (see `shaders/octree.wgsl`) or (b) schedule a periodic
    /// async readback. Until then BH is not a viable default — see
    /// `RepulsionMode::default()`.
    fn rebuild_and_upload_octree(
        &mut self,
        queue: &wgpu::Queue,
    ) {
        let n_node_size = std::mem::size_of::<OctNodeRaw>() as u64;
        let max_nodes = (self.oct_nodes_capacity / n_node_size) as u32;
        let used = self.oct_build.rebuild(
            &self.cpu_positions,
            &self.cpu_mass,
            self.n_nodes,
            max_nodes,
        );
        self.n_octree_used = used;
        if used == 0 {
            // Leave the buffer with whatever stale data is there — the
            // shader sees n_octree=0 from the params and the traversal
            // walk_cap immediately exits at the first iteration (root
            // body == OCT_BODY_INTERNAL with mass=0).
            return;
        }
        let bytes = bytemuck::cast_slice(&self.oct_build.nodes);
        queue.write_buffer(&self.oct_nodes_buf, 0, bytes);
    }
}

// ---------- Tests ------------------------------------------------------------

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

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
    async fn unit_gpu_force_grid_produces_reasonable_layout() {
        // 100 random nodes, 200 random edges. Run 10 steps with grid on.
        let mut g = random_graph(100, 200);
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 10,
            grid_enabled: true,
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
    /// Asserts the run produces finite positions and the hub is roughly
    /// centered relative to its leaves (springs pull leaves toward the hub
    /// and gravity pulls everything to origin, so the hub stays near origin).
    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_star_hub_stable() {
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
            repulsion: 50.0,
            spring_k: 0.5,
            spring_len: 30.0,
            gravity: 0.05,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => { eprintln!("skipping (no gpu adapter): {e}"); return; }
        }
        let mut all_finite = true;
        let mut hub_pos = [0.0f32; 3];
        let mut leaf_extent = 0.0f32;
        for (id, node) in g.nodes.iter() {
            let p = node.position3.expect("position3 set");
            for k in 0..3 { if !p[k].is_finite() { all_finite = false; } }
            if id == "hub" {
                hub_pos = p;
            } else {
                let r = (p[0]*p[0] + p[1]*p[1] + p[2]*p[2]).sqrt();
                if r > leaf_extent { leaf_extent = r; }
            }
        }
        assert!(all_finite, "star-hub run produced non-finite positions");
        // Hub should be near origin (gravity + balanced spring pulls).
        let hub_r = (hub_pos[0]*hub_pos[0] + hub_pos[1]*hub_pos[1] + hub_pos[2]*hub_pos[2]).sqrt();
        assert!(hub_r < leaf_extent * 0.5 + 5.0,
                "hub drifted too far: hub_r={hub_r} leaf_extent={leaf_extent}");
        // Leaves should occupy a non-degenerate volume.
        assert!(leaf_extent > 1.0, "leaves collapsed: extent={leaf_extent}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_barnes_hut_runs_on_small_graph() {
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
