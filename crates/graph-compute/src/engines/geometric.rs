//! Geometric constraint layout engine (`"geometric"`).
//!
//! A force-directed solver whose forces are framed as **geometric constraints**
//! rather than the usual uniform spring/repulsion pair. It is deliberately
//! generic: a single set of kernels, parameterized by how each node's and edge's
//! geometric role is *resolved*. A molecular force field (bonds + bond-angles +
//! Lennard-Jones) is one instantiation of this engine — `node_class = element`,
//! the angle table = hybridization, `edge_len` = bond length — but nothing here
//! mentions chemistry; the vocabulary is purely geometric.
//!
//! ## The four geometric ingredients
//!
//! | Ingredient | Geometric meaning | Force |
//! |---|---|---|
//! | **Edge length** | a target distance `d₀` + stiffness per edge | harmonic spring toward `d₀` |
//! | **Coordination** | a node's preferred angle between its neighbours (degree 2 → 180°, 3 → 120°, 4 → 109.5°, …) | angle constraint over neighbour pairs |
//! | **Class** | a node's exclusion radius + an inter-class affinity matrix | soft short-range exclusion + long-range attract/repel |
//! | **Mass** | a node's inertia + gravity coupling | mass-scaled pull to the origin + integration inertia |
//!
//! What "crystallizes" is decided entirely by how these are mapped onto the
//! graph. Triangles of degree-3 nodes lock into 120° planar rings; cliques
//! become geometrically frustrated; classes with negative cross-affinity
//! phase-separate; heavy (high-centrality) nodes sink to the core. The *look* is
//! a function of the mapping, not of the kernels.
//!
//! ## Composable resolution (the "lens")
//!
//! Each ingredient is resolved from a pluggable [`source`](GeometricSettings):
//!
//!   - **Structural** sources are computed on this worker straight from the CSR
//!     topology — [`ClassSource::Community`] (label propagation),
//!     [`CoordinationSource::Degree`], [`MassSource::PageRank`] /
//!     [`MassSource::Degree`]. No metadata required; these honour "community =
//!     who my neighbours are".
//!   - **Injected** sources read frontend-resolved vectors from
//!     [`CsrShard::attributes`] ([`GraphAttributes`]). This is how
//!     *semantic* mappings reach the topology-only backend — "community = the
//!     `folder` field", "edge length = `weight`", "class = node `type`". The
//!     frontend (which alone has that metadata) resolves the user's choice into
//!     compact numeric vectors and ships them raw.
//!
//! Because the source is per-ingredient, a single subscription can mix them
//! freely: e.g. injected `class` (from a tag) + structural `coordination` (from
//! degree) + structural `mass` (from PageRank) + injected `edge_len` (from
//! weight).
//!
//! ## Scope / scaling
//!
//! This is a **CPU** engine: exclusion is the dominant cost at `O(n²)` per step
//! (same brute-force class as `fa2-brute`), with a distance cutoff to skip
//! far pairs and a per-node cap on angle-pair work so a high-degree hub can't
//! blow up the `O(deg²)` angle term. Both the exclusion and angle passes are
//! embarrassingly parallel and map onto the existing `octree.wgsl` / a WGSL
//! port the same way `fa2-bh` accelerates `fa2-brute`; that GPU path is the
//! documented follow-up (`docs/layout-algorithms.md` §1/§4). Keeping the first
//! cut on the CPU makes the force math unit-testable on headless hosts.

use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};

use super::{CsrShard, EngineCtx, GraphAttributes, LayoutEngine, StepOutput};
use crate::sim::CsrGraph;

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "geometric";

// ---------------------------------------------------------------------------
// Composable attribute sources (the "lens")
// ---------------------------------------------------------------------------

/// How each node's geometric **class** (exclusion radius + affinity) is chosen.
/// Class ids index the class table in [`GeometricSettings`]; ids beyond the
/// table fall back to the default radius and neutral affinity, so a structural
/// source that yields more communities than the table describes still works.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClassSource {
    /// Every node is class 0.
    Uniform,
    /// Bucket nodes by degree: class = number of `thresholds` the node's degree
    /// meets or exceeds (so `thresholds = [4, 16]` ⇒ classes {0:deg<4, 1:4..16,
    /// 2:≥16}). Structural — derived from the CSR.
    Degree { thresholds: Vec<u32> },
    /// Detect communities by label propagation (`passes` sweeps) and use the
    /// community id as the class. Structural — "community = my neighbourhood".
    Community { passes: u32 },
    /// Read `node_class` from the injected [`GraphAttributes`]. Semantic — the
    /// frontend resolved community/tag/type into class ids.
    Injected,
}

impl Default for ClassSource {
    fn default() -> Self {
        ClassSource::Uniform
    }
}

/// How each node's **coordination geometry** (preferred neighbour angle) is
/// chosen. The resolved id indexes [`GeometricSettings::coordination_angles`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinationSource {
    /// Coordination id = node degree (clamped to the angle table). The natural
    /// structural default: degree picks the geometry (2→linear, 3→trigonal, …).
    Degree,
    /// Every node uses the same coordination `bucket`.
    Uniform { bucket: u32 },
    /// Read `node_coordination` from the injected [`GraphAttributes`].
    Injected,
}

impl Default for CoordinationSource {
    fn default() -> Self {
        CoordinationSource::Degree
    }
}

/// How each node's **mass** (gravity coupling + inertia) is chosen. Resolved
/// mass is normalized into `[mass_min, mass_max]` for the structural sources.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MassSource {
    /// Every node has unit mass.
    Uniform,
    /// Mass scales with degree (structural). High-degree nodes are heavier →
    /// sink to the core under gravity.
    Degree,
    /// Mass scales with PageRank centrality (structural, `iters` power
    /// iterations at the given damping ×1000). Reveals core/periphery.
    PageRank { damping_milli: u32, iters: u32 },
    /// Read `node_mass` from the injected [`GraphAttributes`].
    Injected,
}

impl Default for MassSource {
    fn default() -> Self {
        MassSource::Uniform
    }
}

/// How each node's **director** (the per-node unit orientation that drives the
/// patchy / orientation-dependent cohesion well) is initialised.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DirectorSource {
    /// Random unit vectors drawn from the engine's seeded RNG (a disordered
    /// orientation field — the natural seed for a self-assembly run, where the
    /// rotational thermostat then drives them toward an aligned ground state).
    Random,
    /// Every node's director points along `+z` (a pre-aligned field — useful to
    /// seed a configuration that is already nematic, e.g. for stability tests).
    AlignedZ,
    /// Read per-node directors (interleaved x,y,z, length `3n`) from the injected
    /// [`GraphAttributes`]. Semantic — the frontend resolved an orientation field.
    Injected,
}

impl Default for DirectorSource {
    fn default() -> Self {
        DirectorSource::Random
    }
}

/// How each edge's **target length** is chosen.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeLengthSource {
    /// Every edge targets [`GeometricSettings::edge_rest_len`].
    Uniform,
    /// Read `edge_len` (parallel to `neighbors`) from the injected
    /// [`GraphAttributes`]. Semantic — frontend resolved weight/type → length.
    Injected,
}

impl Default for EdgeLengthSource {
    fn default() -> Self {
        EdgeLengthSource::Uniform
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Tunables for the geometric constraint solver. Serde-roundtrippable so they
/// ride on the wire as `google.protobuf.Struct` (ADR-002). Every field has a
/// default, so a `Subscribe` may set only the few knobs it cares about.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GeometricSettings {
    // --- the composable lens ---
    pub class_source: ClassSource,
    pub coordination_source: CoordinationSource,
    pub mass_source: MassSource,
    pub edge_length_source: EdgeLengthSource,
    /// How each node's orientation **director** is initialised (patchy well).
    pub director_source: DirectorSource,

    // --- edge length constraint ---
    /// Target length for [`EdgeLengthSource::Uniform`] (and the fallback for any
    /// non-finite injected length).
    pub edge_rest_len: f32,
    /// Harmonic stiffness of the edge-length spring.
    pub edge_stiffness: f32,

    // --- angle (coordination) constraint ---
    /// Preferred neighbour–neighbour angle in **degrees**, indexed by
    /// coordination id (e.g. degree). The resolved id is clamped to this table.
    pub coordination_angles: Vec<f32>,
    /// Stiffness of the angle constraint. `0` disables the angle term entirely
    /// (pure spring + exclusion, i.e. a classic force layout).
    pub angle_stiffness: f32,
    /// Cap on neighbour *pairs* considered per node for the angle term, to keep
    /// a high-degree hub's `O(deg²)` cost bounded. Nodes with more pairs sample
    /// a deterministic stride. `0` ⇒ unlimited.
    pub max_angle_pairs: u32,

    // --- class exclusion + affinity ---
    /// Per-class exclusion radius. The exclusion onset distance for a pair is
    /// the sum of the two endpoints' radii; ids beyond this table use
    /// `default_radius`.
    pub class_radius: Vec<f32>,
    /// Fallback radius for class ids beyond `class_radius` (e.g. when a
    /// structural source yields more communities than the table covers).
    pub default_radius: f32,
    /// Row-major `n×n` inter-class affinity matrix (n = `class_affinity_dim`).
    /// `affinity > 0` ⇒ the two classes **attract**; `< 0` ⇒ **repel**; `0` ⇒
    /// neutral. Pairs whose class id is outside the matrix are neutral.
    pub class_affinity: Vec<f32>,
    /// Side length `n` of the (flattened) `class_affinity` matrix. `0` ⇒ no
    /// affinity matrix (all neutral).
    pub class_affinity_dim: u32,
    /// Strength multiplier on the short-range exclusion (overlap-prevention).
    pub exclusion_strength: f32,
    /// Strength multiplier on the long-range class affinity term.
    pub affinity_strength: f32,
    /// Exclusion / affinity ignored beyond `cutoff_scale × (rᵢ+rⱼ)`. Bounds the
    /// constant in the `O(n²)` pair scan. (The attractive well extends the
    /// effective cutoff to `max(cutoff_scale·σ, σ + well_width)` so its tail is
    /// never clipped — see [`well_depth`](Self::well_depth).)
    pub cutoff_scale: f32,

    // --- tunable-range attractive well (Cooke–Deserno cohesion) ---
    /// Depth `ε` of the soft attractive well that sits *outside* the short-range
    /// exclusion: WCA-style repulsion up to contact `σ = rᵢ+rⱼ`, then a cosine²
    /// attractive tail reaching width [`well_width`](Self::well_width). `0` ⇒ OFF
    /// (no cohesion term ⇒ default-settings behaviour is byte-identical, the
    /// golden-master regression is untouched). `> 0` lets monomers *condense*:
    /// unlike the constant-magnitude `class_affinity`, this is a **clean
    /// potential** (force is its exact negative gradient), so it folds into
    /// [`EnergyBreakdown::cohesion`] and `observe()` tracks it. The energy minimum
    /// of repulsion + well sits at contact `σ`, so a bound pair relaxes to `≈σ`.
    /// This is the Cooke–Deserno mechanism whose *width* is the dominant knob for
    /// self-assembly (see `docs/self-assembly-plan.md` §4).
    pub well_depth: f32,
    /// Width `w_c` of the attractive well: the cosine² tail spans `σ … σ+w_c`,
    /// decaying from `−ε` at contact to `0` at `σ+w_c`. The single dominant
    /// control parameter for the fluid-membrane regime (wider ⇒ longer-range
    /// cohesion). Ignored when [`well_depth`](Self::well_depth) `== 0`.
    pub well_width: f32,

    // --- mass / gravity / integration ---
    /// Lower bound of the normalized mass range (structural mass sources).
    pub mass_min: f32,
    /// Upper bound of the normalized mass range (structural mass sources).
    pub mass_max: f32,
    /// Linear pull toward the origin, scaled by node mass. Keeps the layout
    /// compact and lets heavy nodes settle central.
    pub gravity: f32,
    /// Velocity damping per step in `[0,1]` (1 = frictionless, 0 = fully
    /// damped). Keeps the explicit integrator stable. Doubles as the Langevin
    /// **friction** coefficient: with `temperature > 0`, the fluctuation term is
    /// balanced against this dissipation so the steady state is a thermal
    /// ensemble (fluctuation–dissipation).
    pub damping: f32,
    /// Integration time step.
    pub time_step: f32,
    /// Hard cap on per-step displacement magnitude per node (after integration),
    /// a stability guard against transient large forces. `0` ⇒ uncapped.
    pub max_step: f32,

    // --- Langevin thermostat (Brownian motion) ---
    /// Thermal energy `kT` in reduced units. `0` ⇒ the engine is a pure damped
    /// **minimizer** (descends to the nearest equilibrium — the historical
    /// behaviour, byte-identical to before this knob existed). `> 0` adds an
    /// Ornstein–Uhlenbeck velocity kick each step so the dynamics sample a
    /// **thermal ensemble** — Brownian motion — and structure can *emerge* from
    /// disorder instead of freezing into the seed's nearest minimum. This is the
    /// keystone for self-assembly (see `docs/self-assembly-plan.md`). At steady
    /// state a free particle obeys equipartition: `⟨½ m v²⟩ = ½ kT` per DOF.
    pub temperature: f32,
    /// Seed for the thermostat's deterministic RNG. Fixing it makes a
    /// `temperature > 0` run reproducible (so the statistical canaries are
    /// stable). Unused when `temperature == 0`.
    pub rng_seed: u64,

    // --- per-node orientation (director) + patchy pair term ---
    /// Anisotropy strength of the orientation-dependent (patchy) cohesion well.
    /// `0` ⇒ OFF: the well is fully **isotropic** (orientation factor ≡ 1), so
    /// default-settings behaviour — and the golden-master regression — is
    /// byte-identical to before directors existed. `> 0` makes the attractive
    /// well **orientation-dependent**: the effective depth of a pair's well is
    /// scaled by `1 + anisotropy_strength·(nᵢ·nⱼ)` (clamped ≥ 0), so pairs whose
    /// directors are **aligned** (`nᵢ·nⱼ → +1`) attract *more* and **anti-aligned**
    /// pairs attract *less* (or not at all). With the directors free to rotate
    /// under rotational Brownian motion, the system's low-energy state is a
    /// mutually-aligned (nematic) aggregate — a flat bilayer-like sheet — which is
    /// the prerequisite for the later curvature coupling (tube/vesicle). See
    /// `docs/self-assembly-plan.md` §5. Gated on [`well_depth`](Self::well_depth) `> 0`
    /// (no well ⇒ nothing to modulate).
    pub anisotropy_strength: f32,
    /// Rotational diffusion coefficient for the per-node director's Brownian
    /// motion, as a fraction of the *translational* thermal scale. At
    /// `temperature > 0` each director takes a small random rotation per step
    /// whose angular variance is `rotational_diffusion · kT · dt` (an
    /// Ornstein–Uhlenbeck-style step on the orientation), then is renormalised to
    /// a unit vector. `0` (or `temperature == 0`) ⇒ directors are **static** (the
    /// orientation field is frozen at its initial values, preserving determinism).
    /// This is the rotational analogue of the translational thermostat; it lets
    /// orientations explore so a patchy aggregate can find its aligned ground
    /// state rather than freezing into the random seed.
    pub rotational_diffusion: f32,

    // --- bending rigidity + spontaneous curvature (Phase C) ---
    //
    // NORMAL CONVENTION (pinned once, here): the per-node director `nᵢ` is the
    // membrane **NORMAL** — perpendicular to the local sheet, *not* tangent to
    // it. Every bending term below is written in that frame: a flat sheet has all
    // normals parallel; a uniformly curved sheet has neighbouring normals tilted
    // by a fixed angle about their in-plane separation axis.
    /// Splay-bend stiffness — the **bending modulus** knob (κ). `0` ⇒ OFF: no
    /// bending torque is applied, so the director field evolves exactly as it did
    /// before this knob existed (default-settings behaviour and the golden master
    /// are byte-identical; the term is gated on both `kappa_bend > 0` *and*
    /// `well_depth > 0`, since with no cohesion well there is no membrane to bend).
    /// `> 0` adds, inside [`integrate_directors`], a torque that penalises
    /// *misalignment of neighbouring normals away from the preferred relative tilt*
    /// — a quadratic-in-local-curvature cost, i.e. a genuine bending rigidity. It
    /// is a **torque only**: it never enters [`compute_forces`] /
    /// [`potential_energy`], so the residual invariant `−∇E == compute_forces` and
    /// the golden master are untouched (bending is validated via order parameters
    /// and relaxation stability, not via the energy scalar — see
    /// `docs/self-assembly-plan.md` §4's JCP checklist). An `EnergyBreakdown::bending`
    /// field with a matching conservative force is a deferred, non-golden-critical
    /// increment.
    pub kappa_bend: f32,
    /// Spontaneous curvature `c₀` — the preferred **relative tilt** between
    /// neighbouring normals (Proposal 2's curvature knob, expressed as a director
    /// tilt rather than bead geometry). `0` ⇒ parallel normals are the ground
    /// state ⇒ the relaxed sheet is **flat**. `> 0` ⇒ each neighbour pair wants its
    /// normals tilted by `c₀` (radians, small-angle) about their in-plane
    /// separation axis, so a relaxed sheet is uniformly **curved** with radius
    /// `R ≈ (neighbour spacing)/c₀` — the flat→tube→vesicle selector. Only acts
    /// when `kappa_bend > 0` (no bending stiffness ⇒ no curvature preference to
    /// impose). Sign is applied consistently across each pair (the partner sees the
    /// separation axis negated) so `c₀` imposes one coherent curvature sense.
    pub spont_curvature_c0: f32,
    /// Flat-membrane (Gay–Berne-style side-by-side) selector. `0` ⇒ OFF: the well
    /// depth is unmodified (byte-identical default). `> 0` biases a pair's well
    /// depth toward **side-by-side** packing — neighbours sitting in each other's
    /// tangent plane (`r̂ ⊥ normal`) — by the factor
    /// `1 + gb_side_strength·(1 − (nᵢ·r̂)²)·(1 − (nⱼ·r̂)²)`. This rewards spreading
    /// into a self-limiting lamella instead of stacking into the mildly-anisotropic
    /// nematic droplet the bare patchy well condenses to. It is applied ONLY as a
    /// per-pair **depth scalar** at the single `orientation_factor` chokepoint, so
    /// the radial force law and its energy integral both see the same scalar and the
    /// `−∇E == compute_forces` relationship is preserved exactly. The *tangential*
    /// derivative of this depth bias is intentionally NOT added to
    /// [`compute_forces`] in the first cut (the bias acts through the well's radial
    /// magnitude only) — consistent with how the patchy anisotropy factor already
    /// omits its angular translational force.
    pub gb_side_strength: f32,
    // --- dynamic bonding (self-assembly P1) ---
    /// Master switch for the dynamic-bond stage. `false` ⇒ OFF (the default): no
    /// dynamic edges are ever created, the bond stage never runs, and the engine
    /// behaves *byte-identically* to before this feature existed — the geometric
    /// golden master and every canary stay green and unchanged. `true` ⇒ every
    /// [`bond_every`](Self::bond_every) steps the engine runs a uniform cell-list
    /// neighbour search and adds/removes **dynamic edges** (bonds) under a
    /// proximity + class-compatibility constraint, so the graph topology *evolves*
    /// and self-assembly (chains → sheets → tubes → vesicles) can emerge from
    /// Brownian motion. The dynamic edges are consumed by the SAME harmonic
    /// edge-length spring the static edges use, *in addition to* the static edges
    /// (the static topology is never mutated; bonds are a parallel, churny edge
    /// set). See `docs/dynamic-edge-bonding-plan.md` §2.
    pub bonding_enabled: bool,
    /// Bond **creation** cutoff: an unbonded, class-compatible pair within this
    /// distance becomes a dynamic bond. Ignored when `bonding_enabled == false`.
    /// Sensible to set near the cohesion-well contact `σ` so a cohering pair bonds.
    pub r_bond: f32,
    /// Bond **break** cutoff (hysteresis): an existing dynamic bond whose length
    /// exceeds this is removed. Should be `≈ 1.2–1.5 · r_bond` so a bond near
    /// contact does not flicker create/break each rebuild. When set `≤ r_bond` (or
    /// non-finite) the engine falls back to `1.3 · r_bond`. The uniform cell-list's
    /// cell size is `r_break` (so the 27-cell stencil covers every candidate pair).
    pub r_break: f32,
    /// Harmonic stiffness of the dynamic-bond spring (the same spring law as
    /// [`edge_stiffness`](Self::edge_stiffness), applied to dynamic edges). The
    /// dynamic bond's rest length is `r_bond` (it relaxes a fresh bond toward the
    /// creation distance). Ignored when `bonding_enabled == false`.
    pub bond_stiffness: f32,
    /// Rebuild cadence: the bond stage (cell-list build + add/remove sweep) runs
    /// every `bond_every` steps (clamped to `≥ 1`). Between rebuilds the existing
    /// dynamic edges are held fixed and only their springs are integrated — the
    /// Verlet-style amortisation from the design (`docs/dynamic-edge-bonding-plan.md`
    /// §1). Ignored when `bonding_enabled == false`.
    pub bond_every: u32,

    // --- dynamic bonding (self-assembly P2): valence cap + bond angle ---
    /// Per-**class** maximum dynamic-bond **valence** (coordination cap): a node of
    /// class `c` accepts at most `max_valence[c]` dynamic bonds. This is the knob
    /// that selects morphology together with [`bond_target_angle`](Self::bond_target_angle):
    /// `2 ⇒ chain`, `3 ⇒ honeycomb sheet`, `4 ⇒ square net` (design §0). When a bond
    /// would push *either* endpoint past its cap it is **rejected**; the cap is
    /// enforced conflict-free and deterministically (sorted candidate ordering +
    /// per-node valence counters + accept/reject — WebGPU-safe, design §2.4). Class
    /// ids beyond this table fall back to [`default_max_valence`](Self::default_max_valence).
    /// **Empty ⇒ no cap** (every class uses the fallback). At the default
    /// `bonding_enabled == false` this is never read, so the default behaviour stays
    /// byte-identical.
    pub max_valence: Vec<u32>,
    /// Fallback valence cap for class ids beyond [`max_valence`](Self::max_valence)
    /// (and for *every* class when that table is empty). `0` ⇒ **uncapped** — the
    /// P1 behaviour (no valence limit), so an empty `max_valence` + a `0` fallback
    /// is byte-identical to P1. Set this (e.g. `2`) to cap a uniform soup into
    /// chains without per-class tables.
    pub default_max_valence: u32,
    /// Per-**class** preferred **dynamic-bond angle** in **degrees** — the target
    /// angle the coordination constraint drives a node's *bonded* neighbour pairs
    /// toward (`180 ⇒ chain`, `120 ⇒ honeycomb sheet`, `90 ⇒ square net`). This is
    /// the bond-geometry half of the morphology ladder: the valence cap sets *how
    /// many* bonds, this sets the *angle between* them. Indexed by the node's class
    /// id, clamped to the table; empty ⇒ [`default_bond_angle`](Self::default_bond_angle)
    /// for every class. The angle term over dynamic bonds reuses the SAME
    /// harmonic-angle force the static coordination constraint uses
    /// ([`accumulate_angle_forces`]), at [`angle_stiffness`](Self::angle_stiffness),
    /// but acts on the *dynamic-bond* adjacency rather than the static CSR. Active
    /// only when `bonding_enabled` AND `angle_stiffness != 0`; never read at the
    /// default `bonding_enabled == false` (byte-identical default).
    pub bond_target_angle: Vec<f32>,
    /// Fallback dynamic-bond angle (degrees) for class ids beyond
    /// [`bond_target_angle`](Self::bond_target_angle) (and for every class when that
    /// table is empty). Defaults to `180°` (a straight chain) — the natural
    /// valence-2 geometry.
    pub default_bond_angle: f32,

    /// Director→position **tilt-coupling** stiffness — the term that makes the
    /// membrane GEOMETRY follow the director (normal) field, so a flat sheet,
    /// hollow tube and closed vesicle become *spontaneous* rather than detector-
    /// only (Phase C3). `0` ⇒ OFF: no positional force from the directors ⇒
    /// default-settings behaviour and the golden master are byte-identical (the
    /// term is gated on both `tilt_coupling_strength > 0` *and* `well_depth > 0`).
    ///
    /// Unlike [`gb_side_strength`](Self::gb_side_strength) (a *radial-depth* bias
    /// that barely reshapes the condensate), this is a genuine **clean potential**
    /// whose negative gradient is added to [`compute_forces`] and whose integral is
    /// folded into [`EnergyBreakdown::tilt`], so `−∇E == compute_forces` holds
    /// exactly (verified by the finite-difference canary). For a cohering pair with
    /// unit separation `r̂` (from `i`→`j`) it penalises the deviation of each
    /// normal's projection onto `r̂` from the spontaneous-curvature target:
    ///   `V = ½·k·w_c(d)·[ (nᵢ·r̂ − c₀/2)² + (nⱼ·r̂ + c₀/2)² ]`
    /// where `w_c(d)` is the SAME cosine² cohesion weight the well uses (so the
    /// coupling is active exactly where particles cohere). At `c₀ = 0` the target
    /// is `nᵢ·r̂ = nⱼ·r̂ = 0`, i.e. neighbours are driven **side-by-side in each
    /// other's tangent plane** — a genuinely FLAT bilayer (one collapsed gyration
    /// axis), the upgrade from the bare patchy well's nematic droplet. At `c₀ > 0`
    /// the targets `±c₀/2` make `i`'s normal lean toward `j` and `j`'s away, so the
    /// relaxed sheet acquires a uniform curvature of sense set by `c₀` — rolling a
    /// sheet into a TUBE at intermediate `c₀` and closing it into a VESICLE at
    /// higher `c₀`. The director field is steered in parallel by the splay-bend
    /// torque ([`kappa_bend`](Self::kappa_bend) / [`spont_curvature_c0`](Self::spont_curvature_c0));
    /// this term is what converts that orientational preference into real geometry.
    pub tilt_coupling_strength: f32,
}

impl Default for GeometricSettings {
    fn default() -> Self {
        Self {
            class_source: ClassSource::default(),
            coordination_source: CoordinationSource::default(),
            mass_source: MassSource::default(),
            edge_length_source: EdgeLengthSource::default(),
            director_source: DirectorSource::default(),

            edge_rest_len: 1.0,
            edge_stiffness: 0.3,

            // Indexed by coordination id = degree (clamped). 0/1 are terminal
            // (no angle is applied for degree < 2); 2→linear, 3→trigonal,
            // 4→tetrahedral, 5→trigonal-bipyramidal-ish, 6+→octahedral-ish.
            coordination_angles: vec![180.0, 180.0, 180.0, 120.0, 109.47, 90.0, 90.0],
            angle_stiffness: 0.1,
            max_angle_pairs: 64,

            class_radius: Vec::new(),
            default_radius: 0.5,
            class_affinity: Vec::new(),
            class_affinity_dim: 0,
            exclusion_strength: 1.0,
            affinity_strength: 0.0,
            cutoff_scale: 6.0,

            // Default OFF: ε=0 ⇒ no cohesion term ⇒ default behaviour is
            // byte-identical (golden-master regression unaffected). well_width is
            // a sensible non-zero default so enabling the well only takes setting
            // well_depth.
            well_depth: 0.0,
            well_width: 1.0,

            mass_min: 1.0,
            mass_max: 1.0,
            gravity: 0.02,
            damping: 0.9,
            time_step: 1.0,
            max_step: 10.0,

            // Default OFF: the engine stays a deterministic minimizer unless a
            // caller dials in a temperature. Keeps every zero-temperature canary
            // and the golden-master regression byte-identical.
            temperature: 0.0,
            rng_seed: 0x5EED_1234_ABCD_F00D,

            // Default OFF: anisotropy 0 ⇒ the well is isotropic (orientation
            // factor ≡ 1) ⇒ default behaviour is byte-identical (golden-master
            // unaffected). rotational_diffusion has a sensible non-zero default so
            // enabling the patchy well only takes setting anisotropy_strength (the
            // directors then explore toward their aligned ground state).
            anisotropy_strength: 0.0,
            rotational_diffusion: 1.0,

            // Default OFF: κ=0 ⇒ no bending torque ⇒ the director field evolves
            // exactly as before ⇒ default behaviour byte-identical (golden master
            // unaffected). c₀=0 ⇒ flat is the ground state; gb_side=0 ⇒ the well
            // depth is unmodified (orientation_factor returns its pre-Phase-C value
            // exactly).
            kappa_bend: 0.0,
            spont_curvature_c0: 0.0,
            gb_side_strength: 0.0,
            // Default OFF: no director→position coupling ⇒ positions evolve exactly
            // as before ⇒ default behaviour byte-identical (golden master unaffected).
            tilt_coupling_strength: 0.0,

            // Default OFF: bonding_enabled = false ⇒ no dynamic edges are ever
            // created and the bond stage never runs ⇒ default behaviour is
            // byte-identical (golden master + every canary unaffected). The other
            // knobs carry sensible non-zero defaults so enabling dynamic bonding
            // only takes flipping `bonding_enabled`.
            bonding_enabled: false,
            r_bond: 1.0,
            // r_break ≤ r_bond is invalid (no hysteresis band) ⇒ the engine falls
            // back to 1.3·r_bond; this default already encodes that ratio.
            r_break: 1.3,
            bond_stiffness: 0.3,
            bond_every: 8,

            // Default OFF for P2: an empty `max_valence` + a `0` fallback ⇒ no
            // valence cap (the P1 add/remove behaviour). `bond_target_angle` empty
            // ⇒ `default_bond_angle` (180°, a chain) for every class — but the
            // dynamic-bond angle term only acts when `bonding_enabled` AND
            // `angle_stiffness != 0`, and `bonding_enabled` is false by default, so
            // none of this is ever read at the default ⇒ byte-identical.
            max_valence: Vec::new(),
            default_max_valence: 0,
            bond_target_angle: Vec::new(),
            default_bond_angle: 180.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Resolved per-node / per-edge state
// ---------------------------------------------------------------------------

/// A unique undirected edge with its resolved geometry, built once at `init`.
#[derive(Clone, Debug)]
pub struct ResolvedEdge {
    pub a: u32,
    pub b: u32,
    pub target_len: f32,
}

/// Everything the force kernels need, resolved from the chosen sources once at
/// `init` (so `step` is pure arithmetic over fixed arrays).
///
/// Public so the GPU engine ([`super::geometric_gpu`]) can share the exact same
/// source-resolution path via [`GeometricEngine::resolve`] instead of
/// re-implementing it (and silently defaulting structural sources to bucket 0 /
/// unit values). The fields are the four per-node / per-edge vectors the kernels
/// consume on either backend.
pub struct Resolved {
    /// Per-node class id (already mapped into the class/affinity tables' domain
    /// by lookup-time fallback, so this may exceed the table sizes).
    pub class: Vec<u32>,
    /// Per-node coordination id (clamped to the angle table at lookup).
    pub coordination: Vec<u32>,
    /// Per-node mass (> 0).
    pub mass: Vec<f32>,
    /// Unique undirected edges (a < b) with target lengths.
    pub edges: Vec<ResolvedEdge>,
}

struct State {
    n: usize,
    /// Interleaved x,y,z positions, length `3n`.
    positions: Vec<f32>,
    /// Interleaved x,y,z velocities, length `3n`.
    velocities: Vec<f32>,
    /// CSR adjacency (owned copy) for the per-node angle pass.
    graph: CsrGraph,
    resolved: Resolved,
    /// Per-node unit directors (interleaved x,y,z, length `3n`) — the orientation
    /// field the patchy cohesion well couples to. Rotated by `integrate_directors`
    /// under rotational Brownian motion; static at `temperature == 0`.
    directors: Vec<f32>,
    /// Langevin thermostat RNG state (SplitMix64). Advanced once per stochastic
    /// degree of freedom in `integrate`; inert when `temperature == 0`.
    rng: u64,
    /// Independent RNG stream for the *rotational* thermostat (director Brownian
    /// motion). Kept separate from `rng` so the director integration never
    /// perturbs the translational thermostat's stream — every existing
    /// temperature>0 canary (equipartition, ideal-chain) stays byte-identical.
    rot_rng: u64,
    /// Dynamic bonds (self-assembly): the evolving edge set the bond stage
    /// add/removes each rebuild. Consumed by the harmonic edge spring *in addition
    /// to* `resolved.edges` (the static topology is never mutated). Empty unless
    /// `bonding_enabled`. Each entry is canonical (`a < b`) with rest length
    /// `r_bond`. Stored sorted by `(a, b)` so the force pass is deterministic.
    dynamic_edges: Vec<ResolvedEdge>,
    /// Set of currently-bonded canonical pairs `(a, b)` with `a < b`, mirroring
    /// `dynamic_edges` — the fast membership test the bond stage uses to avoid
    /// double-bonding a pair. Kept in sync with `dynamic_edges`.
    bonded: std::collections::HashSet<(u32, u32)>,
    /// Steps taken so far (drives the `bond_every` rebuild cadence).
    step_count: u64,
}

/// Geometric constraint engine. Uninitialized until [`LayoutEngine::init`].
pub struct GeometricEngine {
    descriptor: LayoutDescriptor,
    settings: GeometricSettings,
    state: Option<State>,
}

impl GeometricEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: GeometricSettings::default(),
            state: None,
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "Geometric constraints",
            description: "Generic geometric constraint solver: edge-length springs + \
                          neighbour-angle (coordination) constraints + per-class exclusion/\
                          affinity + mass-scaled gravity. Each role (class, coordination, \
                          mass, edge length) is resolved from a composable source — \
                          structural (degree / label-propagation community / PageRank, \
                          derived from topology) or injected (frontend-resolved tag / type / \
                          weight). Motifs 'crystallize' into preferred geometries; classes \
                          phase-separate. A molecular force field is one instantiation. \
                          CPU engine; GPU (octree) port is a follow-up.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }

    /// Resolve every source into concrete per-node / per-edge vectors. Pulls
    /// from injected [`GraphAttributes`] for `Injected` sources (erroring if the
    /// required vector is absent or the wrong length) and computes structural
    /// sources from the CSR.
    ///
    /// Public so the GPU engine can call the *same* resolver — both backends must
    /// honour structural sources (degree / community / PageRank) identically, not
    /// just injected attributes. The CPU engine's [`init`](LayoutEngine::init)
    /// behaviour is unchanged.
    pub fn resolve(
        settings: &GeometricSettings,
        graph: &CsrGraph,
        attrs: Option<&GraphAttributes>,
    ) -> Result<Resolved, String> {
        let n = graph.n_nodes as usize;
        if let Some(a) = attrs {
            a.validate(graph)?;
        }

        let degree = compute_degree(graph);

        // ---- class -------------------------------------------------------
        let class = match &settings.class_source {
            ClassSource::Uniform => vec![0u32; n],
            ClassSource::Degree { thresholds } => degree
                .iter()
                .map(|&d| bucket_by_thresholds(d, thresholds))
                .collect(),
            ClassSource::Community { passes } => label_propagation(graph, *passes),
            ClassSource::Injected => attrs.and_then(|a| a.node_class.clone()).ok_or_else(|| {
                "class_source = injected but GraphAttributes.node_class is absent".to_string()
            })?,
        };

        // ---- coordination ------------------------------------------------
        let coordination = match &settings.coordination_source {
            CoordinationSource::Degree => degree.clone(),
            CoordinationSource::Uniform { bucket } => vec![*bucket; n],
            CoordinationSource::Injected => attrs
                .and_then(|a| a.node_coordination.clone())
                .ok_or_else(|| {
                    "coordination_source = injected but GraphAttributes.node_coordination is absent"
                        .to_string()
                })?,
        };

        // ---- mass --------------------------------------------------------
        let mass = match &settings.mass_source {
            MassSource::Uniform => vec![1.0f32; n],
            MassSource::Degree => normalize_to_range(
                &degree.iter().map(|&d| d as f32).collect::<Vec<_>>(),
                settings.mass_min,
                settings.mass_max,
            ),
            MassSource::PageRank {
                damping_milli,
                iters,
            } => {
                let damping = (*damping_milli as f32 / 1000.0).clamp(0.0, 0.999);
                let pr = pagerank(graph, damping, (*iters).max(1));
                normalize_to_range(&pr, settings.mass_min, settings.mass_max)
            }
            MassSource::Injected => attrs.and_then(|a| a.node_mass.clone()).ok_or_else(|| {
                "mass_source = injected but GraphAttributes.node_mass is absent".to_string()
            })?,
        };
        // Mass must be strictly positive (it divides force in integration).
        let mass: Vec<f32> = mass
            .into_iter()
            .map(|m| if m.is_finite() && m > 1e-4 { m } else { 1.0 })
            .collect();

        // ---- edges (unique, a<b) with target lengths ---------------------
        let injected_len = match settings.edge_length_source {
            EdgeLengthSource::Uniform => None,
            EdgeLengthSource::Injected => {
                Some(attrs.and_then(|a| a.edge_len.clone()).ok_or_else(|| {
                    "edge_length_source = injected but GraphAttributes.edge_len is absent"
                        .to_string()
                })?)
            }
        };
        let edges = build_unique_edges(graph, settings.edge_rest_len, injected_len.as_deref());

        Ok(Resolved {
            class,
            coordination,
            mass,
            edges,
        })
    }
}

impl Default for GeometricEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Potential energy of the engine's *conservative* terms, decomposed by source.
///
/// The negative gradient of each field below is exactly the force the integrator
/// applies, so a relaxing layout drives [`total`](Self::total) toward a local
/// minimum. One deliberate omission: **class affinity** (a constant-magnitude,
/// hard-cutoff attraction) is not a clean potential, so it is excluded from the
/// energy scalar — it is inactive at the default `affinity_strength = 0`, and
/// even when enabled it still contributes to the residual force, which is what
/// the convergence check actually keys on.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EnergyBreakdown {
    /// Harmonic edge-length springs: `Σ ½·k·(‖edge‖ − target)²`.
    pub edge: f32,
    /// Neighbour-angle (coordination) constraints: `Σ ½·k·(θ − ideal)²`.
    pub angle: f32,
    /// Short-range class exclusion (soft overlap penalty), integrated.
    pub exclusion: f32,
    /// Tunable-range attractive well (Cooke–Deserno cohesion): the cosine²
    /// attractive tail just outside contact. A *clean* potential (unlike the old
    /// constant-magnitude affinity), so `−∇(cohesion) == its force`. `≤ 0`
    /// (attractive lowers energy); always `0` at the default `well_depth = 0`.
    pub cohesion: f32,
    /// Mass-scaled pull toward the origin: `Σ ½·gravity·mᵢ·‖rᵢ‖²`.
    pub gravity: f32,
    /// Director→position **tilt-coupling** potential (Phase C3): the cohesion-
    /// weighted penalty `Σ ½·k·w_c·[(nᵢ·r̂−c₀/2)² + (nⱼ·r̂+c₀/2)²]` that makes the
    /// membrane geometry follow the normals (flat at `c₀=0`, curved at `c₀>0`). A
    /// *clean* potential — its negative gradient is exactly the force added in
    /// [`accumulate_exclusion_affinity`], so `−∇E == compute_forces`. `≥ 0`; always
    /// `0` at the default `tilt_coupling_strength = 0` (byte-identical default).
    pub tilt: f32,
}

impl EnergyBreakdown {
    /// Total conservative potential energy.
    pub fn total(&self) -> f32 {
        self.edge + self.angle + self.exclusion + self.cohesion + self.gravity + self.tilt
    }
}

/// A non-destructive snapshot of the solver's current state — the geometric
/// analogue of reading the energy + forces of a molecular configuration without
/// stepping the dynamics. At a *solved* (equilibrium) layout the residual force
/// `‖∇E‖ → 0` and the potential sits at a local minimum; that is precisely the
/// signal the canary / regression / performance harness keys on.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct GeometricObservables {
    /// Number of nodes.
    pub n: usize,
    /// Decomposed conservative potential energy.
    pub energy: EnergyBreakdown,
    /// `energy.total()`, hoisted for convenience.
    pub potential: f32,
    /// Kinetic energy `Σ ½·mᵢ·‖vᵢ‖²`. Trends to 0 as a damped layout settles.
    pub kinetic: f32,
    /// Largest per-node net force magnitude `maxᵢ ‖Fᵢ‖` — the strictest
    /// convergence signal (everything is at rest only when this is ~0).
    pub max_residual: f32,
    /// Root-mean-square per-node net force magnitude. Less sensitive to a single
    /// frustrated node than [`max_residual`](Self::max_residual).
    pub rms_residual: f32,
}

/// Order-parameter observables that **detect the emergent self-assembly level**
/// of the current particle cloud (Phase O — see `docs/self-assembly-plan.md` §6).
///
/// These are *order parameters*, not phase labels: each is a continuous scalar
/// read non-destructively from the live positions / directors. They are what the
/// statistical-mechanics canaries (Phase S) and the renderer HUD key on to tell a
/// monomer soup from a chain from a sheet from a closed vesicle, without meshing
/// and without hard-coding phase thresholds (that framing was refuted in the
/// research — report the continuous values and let the caller interpret them).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AssemblyObservables {
    /// Number of nodes.
    pub n: usize,
    /// **Nematic order parameter** `S = (3/2)·λ_max(Q)`, where `Q = ⟨n⊗n⟩ − I/3`
    /// is the traceless second-moment tensor of the per-node directors. Continuous
    /// in `[0, 1]`: `0` isotropic (random orientations), `1` perfectly aligned. It
    /// is head–tail symmetric (does not distinguish `n` from `−n`), the physically
    /// correct nematic measure. **Not** binned into phase thresholds — reported raw.
    pub nematic_s: f32,
    /// Number of connected components ("clusters") under a contact graph: two
    /// nodes are linked when their separation is `≤ contact_scale·(rᵢ+rⱼ)`. Many
    /// singletons (a monomer gas) ⇒ `≈ n`; one condensed aggregate ⇒ `1`.
    pub cluster_count: usize,
    /// Size (node count) of the **largest** cluster. The CMC analogue: it jumps
    /// from `1` (dispersed) toward `n` (a single aggregate) as cohesion condenses
    /// the soup.
    pub largest_cluster: usize,
    /// Fraction of nodes in the largest cluster, `largest_cluster / n` in `[0, 1]`.
    pub largest_cluster_frac: f32,
    /// **Closure metric** for the largest cluster: the fraction of solid angle
    /// (around the cluster centroid) that is *covered* by a particle, in `[0, 1]`.
    /// A hollow shell wrapping the centroid covers nearly the whole sphere
    /// (`→ 1` ⇒ **closed**, e.g. a vesicle); a flat disk or open sheet leaves the
    /// two faces of the plane uncovered (`≈ 0.5` or less ⇒ **open**). See
    /// [`is_closed`](Self::is_closed) for the heuristic's documented limits.
    pub closure: f32,
    /// Radius of gyration of the largest cluster (its spatial extent). `0` for a
    /// single-particle cluster. Reported for scale context alongside `closure`.
    pub largest_cluster_rg: f32,
}

impl AssemblyObservables {
    /// Heuristic verdict from [`closure`](Self::closure): the largest cluster
    /// **encloses** its centroid (a closed vesicle/shell) when solid-angle
    /// coverage clears a high bar. The bar is intentionally a *caller-side*
    /// interpretation, not baked into the observable.
    ///
    /// Limits of the heuristic (documented honestly): coverage is estimated by
    /// bucketing each particle's direction-from-centroid into a fixed angular grid
    /// and asking what fraction of buckets are hit. It therefore (a) needs enough
    /// particles to populate the grid — a tiny cluster reads as "open" regardless;
    /// (b) cannot distinguish a *thick* filled ball from a hollow shell (both cover
    /// the full sphere) — but in self-assembly the alternatives are open sheets vs.
    /// closed shells, where coverage cleanly separates the two; (c) a sheet folded
    /// past a hemisphere but not closed reads as a marginal value. It is a cheap,
    /// mesh-free screen, not a genus computation.
    pub fn is_closed(&self) -> bool {
        self.closure >= 0.85
    }
}

impl GeometricEngine {
    /// Inspect the current layout without advancing it: decomposed potential
    /// energy, kinetic energy, and the residual force `‖∇E‖`. Returns `None`
    /// before a successful [`init`](LayoutEngine::init).
    ///
    /// This is the observable the geometric-solver test framework is built on —
    /// it lets a test assert *convergence to a solved structure* (residual below
    /// a tolerance, energy at its floor) rather than only the qualitative
    /// "did it move the right way" checks.
    pub fn observe(&self) -> Option<GeometricObservables> {
        let st = self.state.as_ref()?;
        let energy = potential_energy(st, &self.settings);
        let force = compute_forces(st, &self.settings);

        let (mut max2, mut sum2) = (0.0f64, 0.0f64);
        for i in 0..st.n {
            let (fx, fy, fz) = (
                force[3 * i] as f64,
                force[3 * i + 1] as f64,
                force[3 * i + 2] as f64,
            );
            let m2 = fx * fx + fy * fy + fz * fz;
            max2 = max2.max(m2);
            sum2 += m2;
        }
        let mut kinetic = 0.0f64;
        for i in 0..st.n {
            let m = st.resolved.mass[i] as f64;
            let (vx, vy, vz) = (
                st.velocities[3 * i] as f64,
                st.velocities[3 * i + 1] as f64,
                st.velocities[3 * i + 2] as f64,
            );
            kinetic += 0.5 * m * (vx * vx + vy * vy + vz * vz);
        }

        Some(GeometricObservables {
            n: st.n,
            potential: energy.total(),
            energy,
            kinetic: kinetic as f32,
            max_residual: max2.sqrt() as f32,
            rms_residual: if st.n > 0 {
                (sum2 / st.n as f64).sqrt() as f32
            } else {
                0.0
            },
        })
    }
}

impl GeometricEngine {
    /// The current dynamic bonds (self-assembly P1) as canonical `(a, b)` pairs
    /// with `a < b`, sorted by `(a, b)`. Empty unless `bonding_enabled` and at
    /// least one bond stage has run. Exposed so the validation harness can assert
    /// the bond set directly (which pairs bonded, how many). `None` before
    /// [`init`](LayoutEngine::init).
    pub fn dynamic_bonds(&self) -> Option<Vec<(u32, u32)>> {
        self.state
            .as_ref()
            .map(|st| st.dynamic_edges.iter().map(|e| (e.a, e.b)).collect())
    }

    /// The engine's current interleaved x,y,z positions (length `3n`), or an empty
    /// slice before [`init`](LayoutEngine::init). A read-only accessor so the
    /// validation harness can measure the *shape* (gyration tensor) of an
    /// assembled patch on the live positions without stepping (which the public
    /// `StepOutput` path would otherwise require).
    #[doc(hidden)]
    pub fn positions_for_test(&self) -> &[f32] {
        self.state.as_ref().map(|st| st.positions.as_slice()).unwrap_or(&[])
    }

    /// Run *only* the dynamic-bond stage (cell-list build + add/remove sweep) on
    /// the current positions, without computing forces or integrating. Exposed so
    /// the validation harness can benchmark the bond stage's O(n) scaling in
    /// isolation from the engine's separate O(n²) pair-force pass (which would
    /// otherwise dominate any whole-`step` timing). A no-op before
    /// [`init`](LayoutEngine::init) or when `bonding_enabled == false`.
    #[doc(hidden)]
    pub fn run_bond_stage_for_test(&mut self) {
        if !self.settings.bonding_enabled {
            return;
        }
        if let Some(st) = self.state.as_mut() {
            update_dynamic_bonds(st, &self.settings);
        }
    }

    /// The current per-node director field (interleaved x,y,z, length `3n`), or
    /// `None` before [`init`](LayoutEngine::init). Exposed so the validation
    /// harness can compute orientational order parameters (the nematic `S`) on the
    /// live orientation field — the Phase-O observable, computed in-test for now.
    pub fn directors(&self) -> Option<&[f32]> {
        self.state.as_ref().map(|st| st.directors.as_slice())
    }

    /// Compute the self-assembly **order parameters** (nematic `S`, cluster-size
    /// distribution, closure/curvature) on the live configuration, without
    /// advancing it. Returns `None` before a successful [`init`](LayoutEngine::init).
    ///
    /// This is the Phase-O observable (`docs/self-assembly-plan.md` §6): the signal
    /// that *detects* each emergent level (monomer → chain → sheet → vesicle).
    /// Kept separate from [`observe`](Self::observe) (energy / residual) because it
    /// answers a different question — *what did the dynamics build?*, not *has it
    /// converged?* — and is `O(n²)` for the contact pass like the force kernels.
    ///
    /// The contact cutoff for clustering is `contact_scale·(rᵢ+rⱼ)`, i.e. the
    /// per-pair exclusion onset scaled out a little so a relaxed (near-`σ`) bond
    /// counts as a contact. `1.2` is a sensible default (just past contact, well
    /// inside the attractive well).
    pub fn observe_assembly(&self) -> Option<AssemblyObservables> {
        self.observe_assembly_with(1.2)
    }

    /// [`observe_assembly`](Self::observe_assembly) with an explicit contact-cutoff
    /// scale (the multiple of `σ = rᵢ+rⱼ` below which two nodes are "in contact").
    pub fn observe_assembly_with(&self, contact_scale: f32) -> Option<AssemblyObservables> {
        let st = self.state.as_ref()?;
        Some(assembly_observables(st, &self.settings, contact_scale))
    }
}

impl LayoutEngine for GeometricEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: GeometricSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode geometric settings: {e}"))?;
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
        let resolved = Self::resolve(&self.settings, g, graph.attributes)?;
        // Director RNG: a stream distinct from the thermostat's (different fixed
        // mix constant) so building the orientation field can't shift the
        // translational thermostat stream — backward-compat for every T>0 canary.
        let mut director_rng = self.settings.rng_seed ^ 0xD1EC_70F0_FACE_B00C;
        let directors =
            resolve_directors(&self.settings, n, graph.attributes, &mut director_rng)?;
        self.state = Some(State {
            n,
            positions: positions.to_vec(),
            velocities: vec![0.0f32; 3 * n],
            graph: g.clone(),
            resolved,
            directors,
            // Offset off zero so a `rng_seed` of 0 is still a usable stream.
            rng: self.settings.rng_seed ^ 0x9E37_79B9_7F4A_7C15,
            rot_rng: director_rng,
            dynamic_edges: Vec::new(),
            bonded: std::collections::HashSet::new(),
            step_count: 0,
        });
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let settings = self.settings.clone();
        let st = self
            .state
            .as_mut()
            .expect("geometric step called before successful init");
        if st.n == 0 {
            return StepOutput::positions_only(st.positions.clone());
        }
        step_forces(st, &settings);
        StepOutput::positions_only(st.positions.clone())
    }
}

// ---------------------------------------------------------------------------
// Force integration
// ---------------------------------------------------------------------------

/// One explicit integration step: optionally run the dynamic-bond stage, then
/// accumulate all geometric forces and advance velocities/positions/directors.
///
/// The bond stage runs *before* the force pass (so the freshly created/removed
/// dynamic edges are felt this same step) and only every `bond_every` steps when
/// `bonding_enabled` (the Verlet-style amortisation). With bonding OFF (the
/// default) nothing here changes: `dynamic_edges` stays empty and the force pass
/// sees exactly the static edges it always did — byte-identical.
fn step_forces(st: &mut State, s: &GeometricSettings) {
    if s.bonding_enabled {
        let every = s.bond_every.max(1) as u64;
        if st.step_count % every == 0 {
            update_dynamic_bonds(st, s);
        }
    }
    let force = compute_forces(st, s);
    integrate(st, &force, s);
    integrate_directors(st, s);
    st.step_count += 1;
}

/// Accumulate every geometric force into a fresh `3n` vector **without**
/// integrating. Shared by [`step_forces`] (which then advances state) and by
/// [`GeometricEngine::observe`] (which inspects the residual `‖∇E‖` to tell
/// whether the layout has reached equilibrium) — so the residual the observable
/// reports is, to the last bit, the force the integrator would apply.
fn compute_forces(st: &State, s: &GeometricSettings) -> Vec<f32> {
    let mut force = vec![0.0f32; 3 * st.n];
    accumulate_edge_forces(
        &mut force,
        &st.positions,
        &st.resolved.edges,
        s.edge_stiffness,
    );
    // Dynamic bonds (self-assembly) ride the SAME harmonic spring as the static
    // edges, at `bond_stiffness`. Empty unless `bonding_enabled`, so a default run
    // adds nothing here (byte-identical). The bond stage keeps each entry's
    // `target_len = r_bond`, so a fresh bond relaxes toward the creation distance.
    if !st.dynamic_edges.is_empty() {
        accumulate_edge_forces(&mut force, &st.positions, &st.dynamic_edges, s.bond_stiffness);
    }
    if s.angle_stiffness != 0.0 {
        accumulate_angle_forces(&mut force, st, s);
        // Dynamic bonds (self-assembly P2) feed the SAME harmonic-angle force as
        // the static coordination constraint, but over the *dynamic-bond*
        // adjacency and toward the per-class `bond_target_angle` (so bonded
        // neighbours are driven to e.g. 180°⇒chain, 120°⇒sheet). Empty unless
        // `bonding_enabled` produced bonds ⇒ a default run adds nothing here
        // (byte-identical). Gated on the same `angle_stiffness != 0` as the static
        // term so disabling angles disables both.
        if !st.dynamic_edges.is_empty() {
            accumulate_bond_angle_forces(&mut force, st, s);
        }
    }
    accumulate_exclusion_affinity(&mut force, st, s);
    accumulate_gravity(&mut force, &st.positions, &st.resolved.mass, s.gravity);
    force
}

/// Decomposed conservative potential energy of the current layout. Each term is
/// the integral of its force law, so `-∇(this) == compute_forces` for the
/// conservative terms (affinity excepted — see [`EnergyBreakdown`]). Accumulated
/// in `f64` for numerical stability over large pair counts, returned as `f32`.
fn potential_energy(st: &State, s: &GeometricSettings) -> EnergyBreakdown {
    let pos = &st.positions;

    // Edge springs: ½·k·(d − target)².
    let mut edge = 0.0f64;
    for e in &st.resolved.edges {
        let dl = (pair_dist(pos, e.a as usize, e.b as usize) - e.target_len) as f64;
        edge += 0.5 * s.edge_stiffness as f64 * dl * dl;
    }

    // Angle: ½·k·(θ − ideal)² over the SAME kept triples the force pass uses.
    let mut angle = 0.0f64;
    if s.angle_stiffness != 0.0 {
        let k = s.angle_stiffness as f64;
        for_each_angle_triple(st, s, |c, j, kn, ideal_rad| {
            let d = triple_angle(pos, c, j, kn) as f64 - ideal_rad as f64;
            angle += 0.5 * k * d * d;
        });
    }

    // Exclusion: the integral of the soft repulsion force S·(σ/d − 1) (for
    // d < σ), i.e. S·(σ·ln(σ/d) − (σ − d)). Zero at and beyond σ. The force's
    // distance cutoff (cutoff_scale·σ ≥ σ) never clips a repulsion pair, so this
    // is exact regardless of `cutoff_scale`.
    //
    // Cohesion: the cosine² attractive well, the exact integral of its force so
    // `−∇(cohesion) == its force`. V = −ε for d<σ, −ε·cos²(π(d−σ)/(2 w_c)) for
    // σ≤d≤σ+w_c, 0 beyond. Only the σ≤d≤σ+w_c branch can be sampled by a relaxed
    // pair (d<σ is held off by exclusion), but the flat −ε branch is included so
    // the potential is continuous and consistent across the whole domain.
    let mut exclusion = 0.0f64;
    let mut cohesion = 0.0f64;
    let mut tilt = 0.0f64;
    let class = &st.resolved.class;
    let nc = s.class_affinity_dim as usize;
    let wc = s.well_width.max(1e-4) as f64;
    let tilt_k = s.tilt_coupling_strength as f64;
    let tilt_t = 0.5 * s.spont_curvature_c0 as f64;
    for i in 0..st.n {
        let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
        for j in (i + 1)..st.n {
            let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
            let sigma = (ri + rj).max(1e-3) as f64;
            let ddx = pos[3 * j] - pos[3 * i];
            let ddy = pos[3 * j + 1] - pos[3 * i + 1];
            let ddz = pos[3 * j + 2] - pos[3 * i + 2];
            let d = ((ddx * ddx + ddy * ddy + ddz * ddz).sqrt().max(1e-4)) as f64;
            if d < sigma {
                exclusion += s.exclusion_strength as f64 * (sigma * (sigma / d).ln() - (sigma - d));
            }
            // Same r̂ the force pass uses, so the GB-side depth scalar (and hence
            // the cohesion energy) matches the radial force's depth exactly.
            let invd = 1.0 / (d as f32).max(1e-4);
            let (ux, uy, uz) = (ddx * invd, ddy * invd, ddz * invd);
            let eps = (pair_well_depth(s, nc, class[i] as usize, class[j] as usize)
                * orientation_factor(
                    &st.directors,
                    s.anisotropy_strength,
                    s.gb_side_strength,
                    ux,
                    uy,
                    uz,
                    i,
                    j,
                )) as f64;
            if eps > 0.0 {
                if d < sigma {
                    cohesion -= eps;
                } else if d <= sigma + wc {
                    let c = (std::f64::consts::FRAC_PI_2 * (d - sigma) / wc).cos();
                    cohesion -= eps * c * c;
                }
            }

            // Tilt-coupling potential — the integral matching `accumulate_tilt_force`
            // (same w_c weight, same r̂, same ±c₀/2 targets) so `−∇(tilt) == its force`.
            if tilt_k > 0.0 && eps > 0.0 {
                let w = if d <= sigma {
                    1.0
                } else if d <= sigma + wc {
                    let c = (std::f64::consts::FRAC_PI_2 * (d - sigma) / wc).cos();
                    c * c
                } else {
                    0.0
                };
                if w > 0.0 {
                    let (nix, niy, niz) = (
                        st.directors[3 * i] as f64,
                        st.directors[3 * i + 1] as f64,
                        st.directors[3 * i + 2] as f64,
                    );
                    let (njx, njy, njz) = (
                        st.directors[3 * j] as f64,
                        st.directors[3 * j + 1] as f64,
                        st.directors[3 * j + 2] as f64,
                    );
                    let a = nix * ux as f64 + niy * uy as f64 + niz * uz as f64 - tilt_t;
                    let b = njx * ux as f64 + njy * uy as f64 + njz * uz as f64 + tilt_t;
                    tilt += 0.5 * tilt_k * w * (a * a + b * b);
                }
            }
        }
    }

    // Gravity: ½·gravity·mᵢ·‖rᵢ‖².
    let mut gravity = 0.0f64;
    if s.gravity != 0.0 {
        for i in 0..st.n {
            let r2 = (pos[3 * i] * pos[3 * i]
                + pos[3 * i + 1] * pos[3 * i + 1]
                + pos[3 * i + 2] * pos[3 * i + 2]) as f64;
            gravity += 0.5 * s.gravity as f64 * st.resolved.mass[i] as f64 * r2;
        }
    }

    EnergyBreakdown {
        edge: edge as f32,
        angle: angle as f32,
        exclusion: exclusion as f32,
        cohesion: cohesion as f32,
        gravity: gravity as f32,
        tilt: tilt as f32,
    }
}

/// Euclidean distance between nodes `i` and `j` in an interleaved x,y,z buffer.
fn pair_dist(pos: &[f32], i: usize, j: usize) -> f32 {
    let dx = pos[3 * j] - pos[3 * i];
    let dy = pos[3 * j + 1] - pos[3 * i + 1];
    let dz = pos[3 * j + 2] - pos[3 * i + 2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Angle (radians) at center `c` subtended by neighbours `j` and `kn` — the same
/// `θ` [`apply_angle_pair`] differentiates, so the energy and force agree.
fn triple_angle(pos: &[f32], c: usize, j: usize, kn: usize) -> f32 {
    let (ax, ay, az) = (
        pos[3 * j] - pos[3 * c],
        pos[3 * j + 1] - pos[3 * c + 1],
        pos[3 * j + 2] - pos[3 * c + 2],
    );
    let (bx, by, bz) = (
        pos[3 * kn] - pos[3 * c],
        pos[3 * kn + 1] - pos[3 * c + 1],
        pos[3 * kn + 2] - pos[3 * c + 2],
    );
    let la = (ax * ax + ay * ay + az * az).sqrt().max(1e-6);
    let lb = (bx * bx + by * by + bz * bz).sqrt().max(1e-6);
    ((ax * bx + ay * by + az * bz) / (la * lb))
        .clamp(-1.0, 1.0)
        .acos()
}

/// Harmonic edge-length springs: each unique edge pulls/pushes its endpoints
/// toward its target length.
fn accumulate_edge_forces(force: &mut [f32], pos: &[f32], edges: &[ResolvedEdge], k: f32) {
    for e in edges {
        let (i, j) = (e.a as usize, e.b as usize);
        let dx = pos[3 * j] - pos[3 * i];
        let dy = pos[3 * j + 1] - pos[3 * i + 1];
        let dz = pos[3 * j + 2] - pos[3 * i + 2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
        // +f pulls i toward j when dist > target.
        let f = k * (dist - e.target_len) / dist;
        let (fx, fy, fz) = (f * dx, f * dy, f * dz);
        force[3 * i] += fx;
        force[3 * i + 1] += fy;
        force[3 * i + 2] += fz;
        force[3 * j] -= fx;
        force[3 * j + 1] -= fy;
        force[3 * j + 2] -= fz;
    }
}

/// Angle (coordination) constraint: for each node, push its neighbour pairs
/// toward the node's preferred angle. This is the term that makes motifs
/// "crystallize" — degree-3 nodes drive their three neighbours to 120°, etc.
fn accumulate_angle_forces(force: &mut [f32], st: &State, s: &GeometricSettings) {
    let k = s.angle_stiffness;
    for_each_angle_triple(st, s, |c, j, kn, ideal_rad| {
        apply_angle_pair(force, &st.positions, c, j, kn, ideal_rad, k);
    });
}

/// Visit each *kept* neighbour-pair triple (center `c`, neighbours `j` and `kn`,
/// with the center's ideal angle in radians), honouring the per-node degree cap
/// and deterministic stride. Single source of truth for which pairs the angle
/// term acts on, shared by the force pass ([`accumulate_angle_forces`]) and the
/// energy ([`potential_energy`]) so the two never drift out of agreement.
fn for_each_angle_triple(
    st: &State,
    s: &GeometricSettings,
    mut visit: impl FnMut(usize, usize, usize, f32),
) {
    let g = &st.graph;
    let cap = s.max_angle_pairs as usize;
    for c in 0..st.n {
        let deg_id = st.resolved.coordination[c] as usize;
        // Terminal coordinations (no meaningful angle) and isolated/degree-1
        // nodes contribute nothing.
        let start = g.offsets[c] as usize;
        let end = g.offsets[c + 1] as usize;
        let neigh = &g.neighbors[start..end];
        if neigh.len() < 2 {
            continue;
        }
        let ideal_rad = lookup_angle(&s.coordination_angles, deg_id).to_radians();

        // Enumerate neighbour pairs (j,k), capped + strided for high degree.
        let m = neigh.len();
        let total_pairs = m * (m - 1) / 2;
        let stride = if cap != 0 && total_pairs > cap {
            // Deterministic decimation: keep ~`cap` pairs spread across the set.
            (total_pairs / cap).max(1)
        } else {
            1
        };
        let mut pair_idx = 0usize;
        for jj in 0..m {
            for kk in (jj + 1)..m {
                let keep = stride == 1 || pair_idx % stride == 0;
                pair_idx += 1;
                if !keep {
                    continue;
                }
                let j = neigh[jj] as usize;
                let kn = neigh[kk] as usize;
                if j == c || kn == c || j == kn {
                    continue;
                }
                visit(c, j, kn, ideal_rad);
            }
        }
    }
}

/// Apply one angle-constraint update for the triple (center `c`, neighbours `j`,
/// `kn`) toward `ideal_rad`. Standard bond-angle gradient: forces on the two
/// end nodes, with the equal-and-opposite reaction on the center.
fn apply_angle_pair(
    force: &mut [f32],
    pos: &[f32],
    c: usize,
    j: usize,
    kn: usize,
    ideal_rad: f32,
    k: f32,
) {
    // a = p_j - p_c, b = p_kn - p_c
    let ax = pos[3 * j] - pos[3 * c];
    let ay = pos[3 * j + 1] - pos[3 * c + 1];
    let az = pos[3 * j + 2] - pos[3 * c + 2];
    let bx = pos[3 * kn] - pos[3 * c];
    let by = pos[3 * kn + 1] - pos[3 * c + 1];
    let bz = pos[3 * kn + 2] - pos[3 * c + 2];

    let la = (ax * ax + ay * ay + az * az).sqrt().max(1e-6);
    let lb = (bx * bx + by * by + bz * bz).sqrt().max(1e-6);

    // Unit vectors.
    let (uax, uay, uaz) = (ax / la, ay / la, az / la);
    let (ubx, uby, ubz) = (bx / lb, by / lb, bz / lb);

    let cos_t = (uax * ubx + uay * uby + uaz * ubz).clamp(-1.0, 1.0);
    let theta = cos_t.acos();
    let sin_t = (1.0 - cos_t * cos_t).sqrt().max(1e-4);

    // dE/dtheta with E = 0.5 k (theta - ideal)^2  ⇒  coef = k (theta - ideal).
    // F_j = (coef / sin_t) * (1/la) (û_b - cosθ û_a)   (negative gradient)
    let coef = k * (theta - ideal_rad);
    let gj = coef / (sin_t * la);
    let fjx = gj * (ubx - cos_t * uax);
    let fjy = gj * (uby - cos_t * uay);
    let fjz = gj * (ubz - cos_t * uaz);

    let gk = coef / (sin_t * lb);
    let fkx = gk * (uax - cos_t * ubx);
    let fky = gk * (uay - cos_t * uby);
    let fkz = gk * (uaz - cos_t * ubz);

    force[3 * j] += fjx;
    force[3 * j + 1] += fjy;
    force[3 * j + 2] += fjz;
    force[3 * kn] += fkx;
    force[3 * kn + 1] += fky;
    force[3 * kn + 2] += fkz;
    // Reaction keeps the triple's net force zero (no spurious drift).
    force[3 * c] -= fjx + fkx;
    force[3 * c + 1] -= fjy + fky;
    force[3 * c + 2] -= fjz + fkz;
}

/// Dynamic-bond angle (coordination) constraint (self-assembly P2): for each node,
/// push its *bonded* neighbour pairs toward the node's per-class
/// [`bond_target_angle`](GeometricSettings::bond_target_angle) — the bond-geometry
/// half of the morphology ladder (180°⇒chain, 120°⇒honeycomb sheet, 90°⇒square
/// net). Identical harmonic-angle gradient as the static
/// [`accumulate_angle_forces`] (via [`apply_angle_pair`]), but it acts on the
/// *dynamic-bond* adjacency built from [`State::dynamic_edges`] rather than the
/// static CSR. The per-node degree cap / decimation stride from `max_angle_pairs`
/// is reused so a transiently-over-bonded hub can't blow up the `O(deg²)` work.
fn accumulate_bond_angle_forces(force: &mut [f32], st: &State, s: &GeometricSettings) {
    let k = s.angle_stiffness;
    let cap = s.max_angle_pairs as usize;
    // Per-node bonded-neighbour adjacency from the dynamic edge set. Built fresh
    // each force pass (cheap: the bond set is small relative to n²). Deterministic:
    // `dynamic_edges` is kept sorted by `(a, b)`, so each node's neighbour list is
    // in ascending order ⇒ the kept-pair stride is stable run-to-run.
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); st.n];
    for e in &st.dynamic_edges {
        adj[e.a as usize].push(e.b);
        adj[e.b as usize].push(e.a);
    }
    for c in 0..st.n {
        let neigh = &adj[c];
        if neigh.len() < 2 {
            continue; // terminal / single-bond node ⇒ no angle to constrain
        }
        let ideal_rad = lookup_bond_angle(s, st.resolved.class[c] as usize).to_radians();
        let m = neigh.len();
        let total_pairs = m * (m - 1) / 2;
        let stride = if cap != 0 && total_pairs > cap {
            (total_pairs / cap).max(1)
        } else {
            1
        };
        let mut pair_idx = 0usize;
        for jj in 0..m {
            for kk in (jj + 1)..m {
                let keep = stride == 1 || pair_idx % stride == 0;
                pair_idx += 1;
                if !keep {
                    continue;
                }
                let j = neigh[jj] as usize;
                let kn = neigh[kk] as usize;
                if j == c || kn == c || j == kn {
                    continue;
                }
                apply_angle_pair(force, &st.positions, c, j, kn, ideal_rad, k);
            }
        }
    }
}

/// Pairwise class exclusion (overlap prevention) + inter-class affinity
/// (attract/repel). `O(n²)` with a per-pair distance cutoff. This is the term
/// that phase-separates classes.
fn accumulate_exclusion_affinity(force: &mut [f32], st: &State, s: &GeometricSettings) {
    let n = st.n;
    let pos = &st.positions;
    let class = &st.resolved.class;
    let nc = s.class_affinity_dim as usize;

    let wc = s.well_width.max(1e-4);
    for i in 0..n {
        let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
        for j in (i + 1)..n {
            let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
            let sigma = (ri + rj).max(1e-3);

            // Separation + unit direction up front: the orientation factor's
            // Gay–Berne side-by-side term needs r̂, so the well depth (which sets
            // the cutoff) must be computed against the same r̂ the force uses.
            let dx = pos[3 * j] - pos[3 * i];
            let dy = pos[3 * j + 1] - pos[3 * i + 1];
            let dz = pos[3 * j + 2] - pos[3 * i + 2];
            let dist2 = dx * dx + dy * dy + dz * dz;
            let dist = dist2.sqrt().max(1e-4);
            let (ux, uy, uz) = (dx / dist, dy / dist, dz / dist);

            // The attractive well's tail reaches σ + w_c, which may exceed the
            // exclusion/affinity cutoff — extend so the well is never clipped.
            // The orientation (patchy + GB-side) factor scales the well depth per
            // pair: at anisotropy 0 and gb_side 0 it is 1 (isotropic ⇒
            // byte-identical default).
            let eps = pair_well_depth(s, nc, class[i] as usize, class[j] as usize)
                * orientation_factor(
                    &st.directors,
                    s.anisotropy_strength,
                    s.gb_side_strength,
                    ux,
                    uy,
                    uz,
                    i,
                    j,
                );
            let cutoff = (s.cutoff_scale * sigma).max(if eps > 0.0 { sigma + wc } else { 0.0 });

            if dist2 > cutoff * cutoff {
                continue;
            }

            // Short-range soft exclusion: zero at dist = sigma, growing as the
            // pair approaches. Positive `repel` pushes them apart.
            let repel = if dist < sigma {
                s.exclusion_strength * (sigma / dist - 1.0)
            } else {
                0.0
            };

            // Long-range affinity, constant magnitude (bounded). >0 attracts.
            let aff = lookup_affinity(&s.class_affinity, nc, class[i] as usize, class[j] as usize);
            let attract = aff * s.affinity_strength;

            // Tunable-range cosine² attractive well (cohesion), a *clean*
            // potential. V_att = −ε for d<σ (flat ⇒ no force there; WCA handles
            // d<σ), −ε·cos²(π(d−σ)/(2 w_c)) for σ≤d≤σ+w_c, 0 beyond. The
            // attractive force toward j is −dV/dd = −ε·(π/(2 w_c))·sin(π(d−σ)/w_c)
            // for σ≤d≤σ+w_c (positive ⇒ pulls together), 0 otherwise.
            let cohere = if eps > 0.0 && dist >= sigma && dist <= sigma + wc {
                let x = std::f32::consts::PI * (dist - sigma) / wc;
                eps * std::f32::consts::FRAC_PI_2 / wc * x.sin()
            } else {
                0.0
            };

            // net_toward_j applied to i (and the negative to j).
            let net = attract + cohere - repel;
            let (fx, fy, fz) = (net * ux, net * uy, net * uz);
            force[3 * i] += fx;
            force[3 * i + 1] += fy;
            force[3 * i + 2] += fz;
            force[3 * j] -= fx;
            force[3 * j + 1] -= fy;
            force[3 * j + 2] -= fz;

            // Director→position tilt coupling (Phase C3) — a clean potential
            // V = ½·k·w_c(d)·[(nᵢ·r̂−c₀/2)² + (nⱼ·r̂+c₀/2)²] whose negative gradient
            // is added here (and whose integral is `EnergyBreakdown::tilt`). Gated
            // on a positive depth so it only acts inside a cohering aggregate, and
            // OFF by default (k=0) ⇒ byte-identical.
            if s.tilt_coupling_strength > 0.0 && eps > 0.0 {
                accumulate_tilt_force(
                    force, &st.directors, i, j, ux, uy, uz, dist, sigma, wc,
                    s.tilt_coupling_strength, s.spont_curvature_c0,
                );
            }
        }
    }
}

/// Director→position tilt-coupling force for one cohering pair: the negative
/// gradient of `V = ½·k·w_c(d)·[(nᵢ·r̂−t)² + (nⱼ·r̂+t)²]`, `t = c₀/2`, `r̂` from
/// `i`→`j` and `w_c(d)` the cosine² cohesion weight (1 inside contact `σ`, cos²
/// decay over the well, 0 beyond `σ+w_c`). Pushes the pair's positions so each
/// normal's projection onto `r̂` reaches its target — driving a FLAT bilayer at
/// `c₀=0` (targets 0 ⇒ neighbours side-by-side ⊥ normal) and a uniformly CURVED
/// sheet at `c₀>0`. See [`GeometricSettings::tilt_coupling_strength`]. The matching
/// energy is summed in [`potential_energy`] with the identical `w_c`/projection
/// algebra, so `−∇E == this` exactly (finite-difference canary).
#[allow(clippy::too_many_arguments)]
fn accumulate_tilt_force(
    force: &mut [f32],
    directors: &[f32],
    i: usize,
    j: usize,
    ux: f32,
    uy: f32,
    uz: f32,
    dist: f32,
    sigma: f32,
    wc: f32,
    k: f32,
    c0: f32,
) {
    // Cohesion weight w and its derivative w' along d. Outside the well ⇒ no term.
    let (w, wp) = if dist <= sigma {
        (1.0f32, 0.0f32)
    } else if dist <= sigma + wc {
        let x = std::f32::consts::FRAC_PI_2 * (dist - sigma) / wc;
        let (sx, cx) = x.sin_cos();
        // w = cos²x ; w' = -2 cos x sin x · (π/(2 w_c)) = -(π/w_c)·cos x·sin x.
        (cx * cx, -(std::f32::consts::PI / wc) * cx * sx)
    } else {
        return;
    };
    let t = 0.5 * c0;
    let (nix, niy, niz) = (directors[3 * i], directors[3 * i + 1], directors[3 * i + 2]);
    let (njx, njy, njz) = (directors[3 * j], directors[3 * j + 1], directors[3 * j + 2]);
    let a = nix * ux + niy * uy + niz * uz; // nᵢ·r̂
    let b = njx * ux + njy * uy + njz * uz; // nⱼ·r̂
    let ea = a - t;
    let eb = b + t;

    // ∂V/∂d split into (1) the weight's radial part and (2) the projection part.
    // (1) ½·k·w'·(ea²+eb²) along r̂ (a scalar magnitude on **d**).
    let radial = 0.5 * k * wp * (ea * ea + eb * eb);
    // (2) k·w·[ ea·(nᵢ − a·r̂) + eb·(nⱼ − b·r̂) ] / d   (vector on **d**).
    let inv_d = 1.0 / dist.max(1e-6);
    let kw = k * w * inv_d;
    let pix = kw * ea * (nix - a * ux);
    let piy = kw * ea * (niy - a * uy);
    let piz = kw * ea * (niz - a * uz);
    let pjx = kw * eb * (njx - b * ux);
    let pjy = kw * eb * (njy - b * uy);
    let pjz = kw * eb * (njz - b * uz);
    // Total ∂V/∂**d** = radial·r̂ + (proj part). Force on j is −∂V/∂**d**; on i +.
    let gx = radial * ux + pix + pjx;
    let gy = radial * uy + piy + pjy;
    let gz = radial * uz + piz + pjz;
    force[3 * i] += gx;
    force[3 * i + 1] += gy;
    force[3 * i + 2] += gz;
    force[3 * j] -= gx;
    force[3 * j + 1] -= gy;
    force[3 * j + 2] -= gz;
}

/// Mass-scaled linear pull toward the origin.
fn accumulate_gravity(force: &mut [f32], pos: &[f32], mass: &[f32], gravity: f32) {
    if gravity == 0.0 {
        return;
    }
    let n = mass.len();
    for i in 0..n {
        let g = gravity * mass[i];
        force[3 * i] -= g * pos[3 * i];
        force[3 * i + 1] -= g * pos[3 * i + 1];
        force[3 * i + 2] -= g * pos[3 * i + 2];
    }
}

// ---------------------------------------------------------------------------
// Dynamic bonding (self-assembly P1): uniform cell-list + bond add/remove
// ---------------------------------------------------------------------------

/// A bond's class-compatibility test (P1): a pair *may* bond when the inter-class
/// affinity is **positive** (`affinity > 0 ⇒ attract ⇒ may bond`), reusing the
/// existing `class_affinity` matrix sign convention. With no affinity matrix
/// (`class_affinity_dim == 0`) every pair is compatible (the simple monomer soup),
/// matching how `pair_well_depth` treats the no-matrix case (uniform cohesion).
fn bond_compatible(s: &GeometricSettings, ca: usize, cb: usize) -> bool {
    let nc = s.class_affinity_dim as usize;
    if nc == 0 {
        return true;
    }
    lookup_affinity(&s.class_affinity, nc, ca, cb) > 0.0
}

/// Effective break cutoff with the hysteresis fallback: a valid `r_break` must be
/// finite and strictly greater than `r_bond` (a non-degenerate hysteresis band);
/// otherwise fall back to `1.3 · r_bond` (the documented default ratio).
fn effective_r_break(s: &GeometricSettings) -> f32 {
    if s.r_break.is_finite() && s.r_break > s.r_bond {
        s.r_break
    } else {
        1.3 * s.r_bond
    }
}

/// Canonical ordering of an undirected pair (`a < b`).
fn canon_pair(i: usize, j: usize) -> (u32, u32) {
    if i < j {
        (i as u32, j as u32)
    } else {
        (j as u32, i as u32)
    }
}

/// A uniform cell list over the current positions, cell size = `r_break`. Maps
/// each node into an integer cell and supports scanning the 3×3×3 = 27 neighbour
/// cells around any node — the O(n) candidate-pair generator from the design
/// (`docs/dynamic-edge-bonding-plan.md` §1, NVIDIA *Particles* whitepaper).
///
/// Build is O(n) (hash bucketing, *no* per-node `log n` map lookup) and each
/// cell's bucket preserves node-insertion (ascending index) order. Determinism of
/// the *resulting bond set* does not depend on cell iteration order: the `j > i`
/// candidate filter dedupes pairs, a [`std::collections::HashSet`] dedupes
/// creations, and [`update_dynamic_bonds`] sorts `dynamic_edges` by `(a, b)`
/// before the force pass — so the force-pass edge order is stable run-to-run.
struct CellList {
    /// cell size (= r_break), > 0.
    cell: f32,
    /// node index buckets keyed by integer cell coordinate (ix, iy, iz).
    cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>>,
}

impl CellList {
    /// Build a cell list over `pos` (interleaved x,y,z, length `3n`) with the given
    /// `cell` size. A non-positive/non-finite cell size is clamped to a small
    /// positive value so the grid is always well-defined. O(n).
    fn build(pos: &[f32], n: usize, cell: f32) -> CellList {
        let cell = if cell.is_finite() && cell > 1e-4 { cell } else { 1e-4 };
        let inv = 1.0 / cell;
        let mut cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>> =
            std::collections::HashMap::with_capacity(n);
        for i in 0..n {
            let key = Self::cell_of(pos, i, inv);
            cells.entry(key).or_default().push(i as u32);
        }
        CellList { cell, cells }
    }

    /// Integer cell coordinate of node `i`. `inv = 1/cell`. `floor` (not truncate)
    /// so negative coordinates bucket correctly.
    fn cell_of(pos: &[f32], i: usize, inv: f32) -> (i32, i32, i32) {
        let cx = (pos[3 * i] * inv).floor() as i32;
        let cy = (pos[3 * i + 1] * inv).floor() as i32;
        let cz = (pos[3 * i + 2] * inv).floor() as i32;
        (cx, cy, cz)
    }

    /// Visit each candidate node `j > i` in the 27 cells around `i` (i.e. each
    /// unordered candidate pair exactly once). The `i < j` filter dedupes the pair
    /// regardless of which cell each falls in.
    fn for_each_candidate(&self, pos: &[f32], i: usize, mut visit: impl FnMut(usize)) {
        let inv = 1.0 / self.cell;
        let (cx, cy, cz) = Self::cell_of(pos, i, inv);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(bucket) = self.cells.get(&(cx + dx, cy + dy, cz + dz)) {
                        for &j in bucket {
                            let j = j as usize;
                            if j > i {
                                visit(j);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The dynamic-bond stage (run every `bond_every` steps when `bonding_enabled`):
///
///  1. **break** every existing dynamic bond whose current length exceeds
///     `r_break` (hysteresis — a bond persists between `r_bond` and `r_break`).
///  2. **create** a dynamic bond for every unbonded, class-compatible candidate
///     pair within `r_bond`, found via the uniform cell list (cell = `r_break`,
///     27-cell scan ⇒ O(n) candidates). No valence cap in P1.
///
/// Determinism: candidates are produced in ascending-index order by the cell list
/// and the resulting `dynamic_edges` is re-sorted by `(a, b)`, so the force pass
/// reads a stable edge order run-to-run (no atomics, no hashing in the hot path).
fn update_dynamic_bonds(st: &mut State, s: &GeometricSettings) {
    let n = st.n;
    let r_bond = s.r_bond.max(0.0);
    let r_break = effective_r_break(s);
    let r_bond2 = r_bond * r_bond;
    let r_break2 = r_break * r_break;
    let pos = &st.positions;
    let class = &st.resolved.class;

    // --- 1. break over-stretched bonds (hysteresis) ----------------------
    st.dynamic_edges.retain(|e| {
        let keep = pair_dist(pos, e.a as usize, e.b as usize).powi(2) <= r_break2;
        if !keep {
            st.bonded.remove(&(e.a, e.b));
        }
        keep
    });

    // --- 2. gather in-range, compatible, unbonded candidate pairs --------
    let grid = CellList::build(pos, n, r_break);
    let mut new_bonds: Vec<(u32, u32)> = Vec::new();
    for i in 0..n {
        grid.for_each_candidate(pos, i, |j| {
            let key = canon_pair(i, j);
            if st.bonded.contains(&key) {
                return; // already bonded
            }
            if !bond_compatible(s, class[i] as usize, class[j] as usize) {
                return; // class-incompatible (affinity not positive)
            }
            let dx = pos[3 * j] - pos[3 * i];
            let dy = pos[3 * j + 1] - pos[3 * i + 1];
            let dz = pos[3 * j + 2] - pos[3 * i + 2];
            if dx * dx + dy * dy + dz * dz <= r_bond2 {
                new_bonds.push(key);
            }
        });
    }

    // --- 3. valence-capped, conflict-free accept/reject (P2) -------------
    //
    // Sort candidates by `(a, b)` so the accept/reject decision is a single
    // deterministic pass independent of cell-iteration order (no atomics, no
    // race-dependent results — the WebGPU-safe pattern from design §2.4). Each
    // node carries a running valence counter seeded from the bonds that survived
    // the break step; a candidate is accepted only when BOTH endpoints are still
    // under their class cap, and accepting it bumps both counters so later
    // candidates see the spent capacity. `default_max_valence == 0` (and an empty
    // table) ⇒ uncapped ⇒ the P1 behaviour (every in-range pair is accepted).
    new_bonds.sort_unstable();
    new_bonds.dedup();

    let capped = s.default_max_valence != 0 || !s.max_valence.is_empty();
    let mut valence: Vec<u32> = vec![0; n];
    if capped {
        for e in &st.dynamic_edges {
            valence[e.a as usize] += 1;
            valence[e.b as usize] += 1;
        }
    }

    for key in new_bonds {
        let (a, b) = (key.0 as usize, key.1 as usize);
        if capped {
            let cap_a = lookup_max_valence(s, class[a] as usize);
            let cap_b = lookup_max_valence(s, class[b] as usize);
            if valence[a] >= cap_a || valence[b] >= cap_b {
                continue; // either endpoint at its valence cap ⇒ reject
            }
        }
        // `insert` returns false only if the pair was somehow already present
        // (it cannot be here — candidates are deduped and exclude bonded pairs —
        // but the guard keeps the edge/counter accounting exact).
        if st.bonded.insert(key) {
            if capped {
                valence[a] += 1;
                valence[b] += 1;
            }
            st.dynamic_edges.push(ResolvedEdge {
                a: key.0,
                b: key.1,
                target_len: r_bond,
            });
        }
    }

    // Deterministic force-pass order regardless of break/create history.
    st.dynamic_edges.sort_by_key(|e| (e.a, e.b));
}

/// Look up a class's dynamic-bond valence cap, falling back to
/// [`GeometricSettings::default_max_valence`] beyond the table (or for every class
/// when the table is empty). `0` means **uncapped**; callers treat `valence >= cap`
/// with a `0` cap by never reaching this path (the `capped` gate skips it).
fn lookup_max_valence(s: &GeometricSettings, class: usize) -> u32 {
    let v = s.max_valence.get(class).copied().unwrap_or(s.default_max_valence);
    // A per-class entry of 0 inherits the fallback (so a partially-filled table
    // doesn't accidentally pin some classes to "no bonds at all" unless the
    // fallback itself is 0). When both are 0 the `capped` gate is off anyway.
    if v == 0 { s.default_max_valence } else { v }
}

/// Look up a class's preferred dynamic-bond angle (degrees), clamped to the
/// [`GeometricSettings::bond_target_angle`] table; empty table ⇒
/// [`GeometricSettings::default_bond_angle`].
fn lookup_bond_angle(s: &GeometricSettings, class: usize) -> f32 {
    if s.bond_target_angle.is_empty() {
        return s.default_bond_angle;
    }
    s.bond_target_angle[class.min(s.bond_target_angle.len() - 1)]
}

/// Advance velocities (mass-scaled accel, damped) and positions, with an
/// optional per-node displacement clamp for stability.
///
/// With `temperature > 0` this is a **Langevin** integrator: the deterministic
/// `v ← damping·(v + dt·a)` half (the dissipation) is followed by an
/// Ornstein–Uhlenbeck fluctuation kick `+ √((1 − damping²)·kT/m)·ξ`, `ξ~N(0,1)`.
/// For a free particle (`a = 0`) the stationary velocity variance is exactly
/// `kT/m`, so `⟨½ m v²⟩ = ½ kT` per DOF — equipartition, independent of `dt`.
/// At `temperature == 0` the kick vanishes and this is the original pure damped
/// minimizer (no RNG is even consumed).
fn integrate(st: &mut State, force: &[f32], s: &GeometricSettings) {
    let dt = s.time_step;
    let damping = s.damping.clamp(0.0, 1.0);
    let max_step = s.max_step;
    let kt = s.temperature.max(0.0);
    // OU kick is balanced against the friction `damping` (fluctuation–
    // dissipation): variance √(1 − damping²) so the steady state hits `kT/m`.
    let noise_base = if kt > 0.0 {
        ((1.0 - damping * damping).max(0.0) * kt).sqrt()
    } else {
        0.0
    };
    let n = st.n;
    for i in 0..n {
        let m = st.resolved.mass[i];
        // Per-DOF thermal speed scale: √((1 − damping²)·kT/m).
        let sigma = if noise_base > 0.0 {
            noise_base / m.sqrt()
        } else {
            0.0
        };
        for d in 0..3 {
            let idx = 3 * i + d;
            let a = force[idx] / m;
            let mut v = (st.velocities[idx] + dt * a) * damping;
            if sigma > 0.0 {
                v += sigma * next_gaussian(&mut st.rng);
            }
            // Clamp displacement (not velocity directly) for an intuitive cap.
            let mut disp = dt * v;
            if max_step > 0.0 && disp.abs() > max_step {
                disp = disp.signum() * max_step;
                v = disp / dt;
            }
            // Non-finite guard: a degenerate upstream (a non-finite injected
            // position, or an overflow accumulated over many steps) can make the
            // force — and hence `v`/`disp` — NaN/Inf. The `max_step` clamp above
            // does NOT catch it (`NaN.abs() > max_step` is false), so a poisoned
            // value would propagate into every later step through the O(n²) pair
            // scan. Drop the step to rest rather than carry the non-finite value
            // forward. Never fires on well-conditioned inputs (all finite ⇒ both
            // branches skipped ⇒ byte-identical to the unguarded path).
            if !v.is_finite() {
                v = 0.0;
            }
            if !disp.is_finite() {
                disp = 0.0;
            }
            st.velocities[idx] = v;
            st.positions[idx] += disp;
        }
    }
}

/// Evolve the per-node directors one step under rotational Brownian motion plus
/// an *aligning* mean-field torque from the patchy interaction.
///
/// This is the rotational analogue of [`integrate`]. Two ingredients, both gated
/// so they vanish in the default configuration:
///
///  - **Aligning field.** For each node `i` we accumulate a Lebwohl–Lasher-style
///    mean-field `hᵢ = Σⱼ wᵢⱼ·nⱼ` over neighbours `j` within the well's range,
///    weighted `wᵢⱼ` by how strongly the well couples that pair (depth × a smooth
///    distance falloff). The torque rotates `nᵢ` toward `hᵢ` (its component
///    perpendicular to `nᵢ`), so directors of nearby cohering particles align —
///    the orientational ground state of the patchy potential. Active only when
///    `anisotropy_strength > 0` (no patchiness ⇒ no preferred orientation).
///  - **Rotational noise.** A small isotropic random kick scaled by
///    `√(rotational_diffusion·kT·dt)`, then renormalise to a unit vector — the
///    fluctuation that lets the field explore rather than freeze into the seed.
///    Active only when `temperature > 0` (so `temperature == 0` keeps directors
///    perfectly static and deterministic).
///  - **Splay-bend torque (bending rigidity, Phase C).** When `kappa_bend > 0`
///    (and a cohesion well exists), each neighbour pair accumulates a second
///    scratch field that drives `nⱼ` toward the *preferred relative tilt* of `nᵢ`
///    — parallel normals (flat) when `spont_curvature_c0 == 0`, or normals tilted
///    by `c₀` about the in-plane separation axis (uniformly curved) when `c₀ > 0`.
///    This is the bending modulus: a quadratic-in-local-curvature director
///    misalignment cost. It is a TORQUE ONLY — it never enters [`compute_forces`]
///    / [`potential_energy`], so the residual invariant and the golden master are
///    untouched (the term mutates only `st.directors`, which `observe()` never
///    reads). Gated on `kappa_bend > 0 && well_depth > 0`, mirroring `align_on`.
///
/// With everything off (the default: anisotropy 0 / temperature 0 / kappa_bend 0)
/// the directors are untouched and never affect the forces, so default-settings
/// behaviour — and the golden master — is byte-identical.
fn integrate_directors(st: &mut State, s: &GeometricSettings) {
    let kt = s.temperature.max(0.0);
    let noise_on = kt > 0.0 && s.rotational_diffusion > 0.0;
    let align_on = s.anisotropy_strength > 0.0 && s.well_depth > 0.0;
    let bend_on = s.kappa_bend > 0.0 && s.well_depth > 0.0;
    if !noise_on && !align_on && !bend_on {
        return; // directors are static — nothing (incl. RNG) is consumed
    }
    let n = st.n;
    let dt = s.time_step;
    let nc = s.class_affinity_dim as usize;
    let class = &st.resolved.class;
    let pos = &st.positions;
    // Read from a snapshot so the update is synchronous (every node integrates
    // against the previous step's orientation field, matching the force passes).
    let dir = st.directors.clone();

    // --- aligning mean-field h_i = Σ_j w_ij n_j (perpendicular part applied) ---
    // Computed into a scratch buffer first so the update is synchronous (every
    // node sees the *previous* step's directors, like the other force passes).
    let mut field = vec![0.0f32; 3 * n];
    if align_on {
        let wc = s.well_width.max(1e-4);
        for i in 0..n {
            let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
            for j in (i + 1)..n {
                let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
                let sigma = (ri + rj).max(1e-3);
                let depth = pair_well_depth(s, nc, class[i] as usize, class[j] as usize);
                if depth <= 0.0 {
                    continue;
                }
                let d = pair_dist(pos, i, j);
                // Smooth distance weight: full inside contact, cosine² decay over
                // the well, zero beyond σ + w_c (matches the well's own range).
                let w = if d <= sigma {
                    depth
                } else if d <= sigma + wc {
                    let c = (std::f32::consts::FRAC_PI_2 * (d - sigma) / wc).cos();
                    depth * c * c
                } else {
                    0.0
                };
                if w == 0.0 {
                    continue;
                }
                for k in 0..3 {
                    field[3 * i + k] += w * dir[3 * j + k];
                    field[3 * j + k] += w * dir[3 * i + k];
                }
            }
        }
    }

    // --- splay-bend field b_i: drive n_j toward n_i's preferred tilt ----------
    // Same i<j neighbour loop, same well distance falloff w as the aligning
    // field. For pair (i,j) with in-plane separation r̂, the target for n_j is
    // n_i tilted toward r̂ by the spontaneous-curvature angle c₀ (small-angle):
    //   t_ij = n_i + c₀·(r̂ − (r̂·n_i)·n_i)   (renormalised)
    // and symmetrically the target for n_i uses r̂ negated, so c₀ imposes ONE
    // coherent curvature sign across the pair. The torque on n_j is the
    // perpendicular-to-n_j component of κ·w·(t_ij − n_j); accumulate it (and the
    // symmetric term on n_i) into bend_field. c₀==0 ⇒ t_ij == n_i ⇒ this reduces
    // to a pure alignment torque (flat ground state).
    let mut bend_field = vec![0.0f32; 3 * n];
    if bend_on {
        let kappa = s.kappa_bend;
        let c0 = s.spont_curvature_c0;
        let wc = s.well_width.max(1e-4);
        for i in 0..n {
            let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
            for j in (i + 1)..n {
                let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
                let sigma = (ri + rj).max(1e-3);
                let depth = pair_well_depth(s, nc, class[i] as usize, class[j] as usize);
                if depth <= 0.0 {
                    continue;
                }
                let dx = pos[3 * j] - pos[3 * i];
                let dy = pos[3 * j + 1] - pos[3 * i + 1];
                let dz = pos[3 * j + 2] - pos[3 * i + 2];
                let d = (dx * dx + dy * dy + dz * dz).sqrt();
                // Distance weight: full inside contact, cosine² decay over the
                // well, zero beyond σ + w_c (matches the cohesion well's range).
                let w = if d <= sigma {
                    1.0
                } else if d <= sigma + wc {
                    let c = (std::f32::consts::FRAC_PI_2 * (d - sigma) / wc).cos();
                    c * c
                } else {
                    0.0
                };
                if w == 0.0 || d < 1e-6 {
                    continue;
                }
                let inv = 1.0 / d;
                let (rx, ry, rz) = (dx * inv, dy * inv, dz * inv); // r̂ from i→j
                let (nix, niy, niz) = (dir[3 * i], dir[3 * i + 1], dir[3 * i + 2]);
                let (njx, njy, njz) = (dir[3 * j], dir[3 * j + 1], dir[3 * j + 2]);
                let kw = kappa * w;

                // Target for n_j: tilt n_i toward r̂ (in-plane part of r̂) by c₀.
                accumulate_bend_torque(
                    &mut bend_field, j, nix, niy, niz, rx, ry, rz, njx, njy, njz, c0, kw,
                );
                // Target for n_i: tilt n_j toward −r̂ by c₀ (consistent sign).
                accumulate_bend_torque(
                    &mut bend_field, i, njx, njy, njz, -rx, -ry, -rz, nix, niy, niz, c0, kw,
                );
            }
        }
    }

    // --- integrate each director: align toward its field + rotational noise ---
    // Overdamped rotational step toward the field's *direction*: the aligning
    // field h is normalised to a unit target before use, so the per-step rotation
    // increment is bounded (∝ dt·μ) regardless of how many neighbours summed into
    // h — an unbounded raw `μ·h` overshoots and makes the director oscillate, the
    // head-to-head sign-flips that show up as a volatile, never-settling S.
    let mobility = s.anisotropy_strength.min(1.0); // bounded align rate
    // Bounded bending rate, reusing the same min(.,1) overshoot guard the align
    // rate uses (the splay-bend torque is already an accumulated sum over
    // neighbours, so an unbounded κ would overshoot and oscillate the director).
    let bend_mobility = s.kappa_bend.min(1.0);
    let noise_sigma = if noise_on {
        (s.rotational_diffusion * kt * dt).sqrt()
    } else {
        0.0
    };
    for i in 0..n {
        let (nx, ny, nz) = (dir[3 * i], dir[3 * i + 1], dir[3 * i + 2]);
        let (mut ax, mut ay, mut az) = (nx, ny, nz);

        if align_on {
            let (hx, hy, hz) = (field[3 * i], field[3 * i + 1], field[3 * i + 2]);
            let hlen = (hx * hx + hy * hy + hz * hz).sqrt();
            if hlen > 1e-6 {
                // Unit alignment target, then its component perpendicular to n (a
                // pure rotation toward the target, no stretch). The patchy energy
                // `1 + a·(nᵢ·nⱼ)` is *polar* — it rewards same-direction alignment —
                // so the field's own direction is the target (no hemisphere flip).
                let (tx, ty, tz) = (hx / hlen, hy / hlen, hz / hlen);
                let tdot = tx * nx + ty * ny + tz * nz;
                let (px, py, pz) = (tx - tdot * nx, ty - tdot * ny, tz - tdot * nz);
                ax += dt * mobility * px;
                ay += dt * mobility * py;
                az += dt * mobility * pz;
            }
        }
        if bend_on {
            // bend_field is already the perpendicular-to-nᵢ torque (κ·w·perp),
            // summed over neighbours. Apply it with the bounded bend mobility,
            // mirroring the aligning step. c₀==0 makes this a pure flat-alignment
            // torque; c₀>0 drives a uniform inter-normal tilt (curvature).
            ax += dt * bend_mobility * bend_field[3 * i];
            ay += dt * bend_mobility * bend_field[3 * i + 1];
            az += dt * bend_mobility * bend_field[3 * i + 2];
        }
        if noise_sigma > 0.0 {
            ax += noise_sigma * next_gaussian(&mut st.rot_rng);
            ay += noise_sigma * next_gaussian(&mut st.rot_rng);
            az += noise_sigma * next_gaussian(&mut st.rot_rng);
        }

        // Renormalise back to the unit sphere (degenerate ⇒ keep the old director).
        let len = (ax * ax + ay * ay + az * az).sqrt();
        if len > 1e-6 {
            st.directors[3 * i] = ax / len;
            st.directors[3 * i + 1] = ay / len;
            st.directors[3 * i + 2] = az / len;
        }
    }
}

/// Accumulate the splay-bend torque on node `target` whose director is
/// `(nx, ny, nz)`, driving it toward the *preferred relative tilt* of the
/// reference normal `(refx, refy, refz)` about the in-plane separation axis
/// `(rx, ry, rz)` (a unit vector) by the spontaneous-curvature angle `c0`, scaled
/// by `kw = κ·w`.
///
/// The target orientation is the reference normal tilted toward `r̂` in the plane
/// they span (small-angle): `t = ref + c0·(r̂ − (r̂·ref)·ref)`, renormalised. The
/// torque is the component of `kw·(t − n)` perpendicular to `n` — a pure rotation
/// toward `t`, no stretch. `c0 == 0` ⇒ `t == ref` ⇒ a pure alignment torque (the
/// flat ground state). Accumulated into `bend_field[target]`.
#[allow(clippy::too_many_arguments)]
fn accumulate_bend_torque(
    bend_field: &mut [f32],
    target: usize,
    refx: f32,
    refy: f32,
    refz: f32,
    rx: f32,
    ry: f32,
    rz: f32,
    nx: f32,
    ny: f32,
    nz: f32,
    c0: f32,
    kw: f32,
) {
    // In-plane part of r̂ relative to the reference normal: r̂ − (r̂·ref)·ref.
    let rdotn = rx * refx + ry * refy + rz * refz;
    let (ipx, ipy, ipz) = (rx - rdotn * refx, ry - rdotn * refy, rz - rdotn * refz);
    // Preferred target = ref tilted toward the in-plane axis by c0, renormalised.
    let (mut tx, mut ty, mut tz) = (refx + c0 * ipx, refy + c0 * ipy, refz + c0 * ipz);
    let tlen = (tx * tx + ty * ty + tz * tz).sqrt();
    if tlen < 1e-9 {
        return;
    }
    let invt = 1.0 / tlen;
    tx *= invt;
    ty *= invt;
    tz *= invt;
    // Torque = kw·(t − n) projected perpendicular to n (rotation toward t).
    let tdot = tx * nx + ty * ny + tz * nz;
    let (px, py, pz) = (tx - tdot * nx, ty - tdot * ny, tz - tdot * nz);
    bend_field[3 * target] += kw * px;
    bend_field[3 * target + 1] += kw * py;
    bend_field[3 * target + 2] += kw * pz;
}

// ---------------------------------------------------------------------------
// Self-assembly order parameters (Phase O)
// ---------------------------------------------------------------------------

/// Compute every [`AssemblyObservables`] field from the live state. Non-
/// destructive (reads positions/directors, mutates nothing). `O(n²)` for the
/// contact pass — the same class as the exclusion force kernel.
fn assembly_observables(
    st: &State,
    s: &GeometricSettings,
    contact_scale: f32,
) -> AssemblyObservables {
    let n = st.n;
    if n == 0 {
        return AssemblyObservables::default();
    }

    let nematic_s = nematic_order(&st.directors);

    // ---- cluster-size distribution (union-find over the contact graph) -----
    let parent = contact_components(st, s, contact_scale);
    // Tally component sizes by representative root.
    let mut sizes: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut roots = vec![0usize; n];
    for i in 0..n {
        let r = find_root(&parent, i);
        roots[i] = r;
        *sizes.entry(r).or_insert(0) += 1;
    }
    let cluster_count = sizes.len();
    let (largest_root, largest_cluster) = sizes
        .iter()
        .map(|(&r, &c)| (r, c))
        .max_by_key(|&(_, c)| c)
        .unwrap_or((0, 0));
    let largest_cluster_frac = largest_cluster as f32 / n as f32;

    // ---- largest-cluster geometry: centroid, R_g, closure ------------------
    let members: Vec<usize> = (0..n).filter(|&i| roots[i] == largest_root).collect();
    let (largest_cluster_rg, closure) = cluster_geometry(&st.positions, &members);

    AssemblyObservables {
        n,
        nematic_s,
        cluster_count,
        largest_cluster,
        largest_cluster_frac,
        closure,
        largest_cluster_rg,
    }
}

/// Nematic order parameter `S = (3/2)·λ_max(Q)`, `Q = ⟨n⊗n⟩ − I/3` over the
/// per-node directors (interleaved x,y,z). `0` isotropic, `1` perfectly aligned;
/// head–tail symmetric. The largest eigenvalue of the symmetric traceless 3×3 is
/// found by the closed-form trigonometric method (Smith 1961) — no iterative
/// solver, WASM-safe.
fn nematic_order(directors: &[f32]) -> f32 {
    let n = directors.len() / 3;
    if n == 0 {
        return 0.0;
    }
    let (mut qxx, mut qyy, mut qzz) = (0.0f64, 0.0f64, 0.0f64);
    let (mut qxy, mut qxz, mut qyz) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y, z) = (
            directors[3 * i] as f64,
            directors[3 * i + 1] as f64,
            directors[3 * i + 2] as f64,
        );
        qxx += x * x;
        qyy += y * y;
        qzz += z * z;
        qxy += x * y;
        qxz += x * z;
        qyz += y * z;
    }
    let inv = 1.0 / n as f64;
    let third = 1.0 / 3.0;
    let (qxx, qyy, qzz) = (qxx * inv - third, qyy * inv - third, qzz * inv - third);
    let (qxy, qxz, qyz) = (qxy * inv, qxz * inv, qyz * inv);

    let p1 = qxy * qxy + qxz * qxz + qyz * qyz;
    if p1 < 1e-18 {
        // Already diagonal: the largest diagonal entry is the largest eigenvalue.
        return (1.5 * qxx.max(qyy).max(qzz)) as f32;
    }
    let q = (qxx + qyy + qzz) / 3.0; // = 0 (traceless), kept for generality
    let p2 = (qxx - q).powi(2) + (qyy - q).powi(2) + (qzz - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    let (bxx, byy, bzz) = ((qxx - q) / p, (qyy - q) / p, (qzz - q) / p);
    let (bxy, bxz, byz) = (qxy / p, qxz / p, qyz / p);
    let det_b = bxx * (byy * bzz - byz * byz) - bxy * (bxy * bzz - byz * bxz)
        + bxz * (bxy * byz - byy * bxz);
    let r = (det_b / 2.0).clamp(-1.0, 1.0);
    let phi = r.acos() / 3.0;
    (1.5 * (q + 2.0 * p * phi.cos())) as f32
}

/// Union-find over the **contact graph**: nodes `i,j` are linked when their
/// separation is `≤ contact_scale·(rᵢ+rⱼ)`. Returns the parent array, walked by
/// [`find_root`] to read each node's component root. `O(n²)` — mirrors the
/// exclusion scan (so the contact cutoff lives in the same coordinate frame).
fn contact_components(st: &State, s: &GeometricSettings, contact_scale: f32) -> Vec<usize> {
    let n = st.n;
    let pos = &st.positions;
    let class = &st.resolved.class;
    let scale = contact_scale.max(0.0);
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
        for j in (i + 1)..n {
            let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
            let cutoff = scale * (ri + rj).max(1e-3);
            let dx = pos[3 * j] - pos[3 * i];
            let dy = pos[3 * j + 1] - pos[3 * i + 1];
            let dz = pos[3 * j + 2] - pos[3 * i + 2];
            if dx * dx + dy * dy + dz * dz <= cutoff * cutoff {
                union(&mut parent, i, j);
            }
        }
    }
    parent
}

/// Walk to a node's component root. Read-only (no path compression) so it borrows
/// `parent` immutably — the trees stay shallow because [`union`] always reparents
/// the larger root index, and `n` is the `O(n²)` force-pass scale anyway.
fn find_root(parent: &[usize], mut i: usize) -> usize {
    while parent[i] != i {
        i = parent[i];
    }
    i
}

/// Union by attaching the larger root index under the smaller (keeps roots
/// deterministic regardless of visitation order).
fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = {
        let mut i = a;
        while parent[i] != i {
            i = parent[i];
        }
        i
    };
    let rb = {
        let mut i = b;
        while parent[i] != i {
            i = parent[i];
        }
        i
    };
    if ra == rb {
        return;
    }
    let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
    parent[hi] = lo;
}

/// Geometry of one cluster (given its member indices): radius of gyration and the
/// **closure** metric (solid-angle coverage around the centroid).
///
/// Closure: place a fixed angular grid on the unit sphere (latitude×longitude
/// buckets), bin each member's direction-from-centroid into a bucket weighted by
/// its solid angle, and return the fraction of the total sphere solid angle whose
/// buckets are hit. A hollow shell wrapping the centroid hits buckets all around
/// (`→ 1`); a flat sheet/disk only points *outward in the plane*, leaving the two
/// polar caps empty (`≈ 0.5` or less). Mesh-free and `O(members)`. Limits are
/// documented on [`AssemblyObservables::is_closed`].
fn cluster_geometry(positions: &[f32], members: &[usize]) -> (f32, f32) {
    let m = members.len();
    if m == 0 {
        return (0.0, 0.0);
    }
    // Centroid.
    let (mut cx, mut cy, mut cz) = (0.0f64, 0.0f64, 0.0f64);
    for &i in members {
        cx += positions[3 * i] as f64;
        cy += positions[3 * i + 1] as f64;
        cz += positions[3 * i + 2] as f64;
    }
    let inv = 1.0 / m as f64;
    let (cx, cy, cz) = (cx * inv, cy * inv, cz * inv);

    // Radius of gyration: sqrt(mean squared distance to centroid).
    let mut r2 = 0.0f64;
    for &i in members {
        let dx = positions[3 * i] as f64 - cx;
        let dy = positions[3 * i + 1] as f64 - cy;
        let dz = positions[3 * i + 2] as f64 - cz;
        r2 += dx * dx + dy * dy + dz * dz;
    }
    let rg = (r2 * inv).sqrt() as f32;

    // Solid-angle coverage. A latitude (polar, `n_theta`) × longitude (azimuth,
    // `n_phi`) grid. Each cell's solid angle is `Δφ·(cosθ₀ − cosθ₁)`; summing the
    // hit cells' solid angle over the full `4π` gives the covered fraction. The
    // resolution is coarse on purpose: it must register a face as "covered" from
    // only a handful of particles, and read a sheet's two open caps as empty.
    const N_THETA: usize = 6;
    const N_PHI: usize = 12;
    let mut hit = [[false; N_PHI]; N_THETA];
    for &i in members {
        let dx = positions[3 * i] as f64 - cx;
        let dy = positions[3 * i + 1] as f64 - cy;
        let dz = positions[3 * i + 2] as f64 - cz;
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        if len < 1e-9 {
            continue; // a particle at the centroid carries no direction
        }
        let theta = (dz / len).clamp(-1.0, 1.0).acos(); // [0, π]
        let phi = dy.atan2(dx) + std::f64::consts::PI; // [0, 2π)
        let it = ((theta / std::f64::consts::PI) * N_THETA as f64) as usize;
        let ip = ((phi / std::f64::consts::TAU) * N_PHI as f64) as usize;
        hit[it.min(N_THETA - 1)][ip.min(N_PHI - 1)] = true;
    }
    // Covered solid angle / 4π.
    let mut covered = 0.0f64;
    let dphi = std::f64::consts::TAU / N_PHI as f64;
    for it in 0..N_THETA {
        let t0 = (it as f64 / N_THETA as f64) * std::f64::consts::PI;
        let t1 = ((it + 1) as f64 / N_THETA as f64) * std::f64::consts::PI;
        let band = dphi * (t0.cos() - t1.cos()); // solid angle of one cell in this band
        for ip in 0..N_PHI {
            if hit[it][ip] {
                covered += band;
            }
        }
    }
    let closure = (covered / (4.0 * std::f64::consts::PI)) as f32;
    (rg, closure.clamp(0.0, 1.0))
}

/// One SplitMix64 step: advance `state` in place and return a scrambled `u64`.
/// Deterministic and WASM-safe (no `getrandom`); the same generator the test
/// seed helper uses, so the thermostat stream is reproducible from `rng_seed`.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform `f64` in `[0, 1)` from the top 53 bits of a SplitMix64 draw.
fn next_uniform(state: &mut u64) -> f64 {
    (splitmix64(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// A standard-normal sample via Box–Muller (one variate per call; the paired
/// variate is discarded for simplicity — the thermostat is not RNG-hot relative
/// to the `O(n²)` force pass).
fn next_gaussian(state: &mut u64) -> f32 {
    // Guard the log against an exact-zero draw.
    let u1 = next_uniform(state).max(1e-12);
    let u2 = next_uniform(state);
    let r = (-2.0 * u1.ln()).sqrt();
    (r * (std::f64::consts::TAU * u2).cos()) as f32
}

/// A uniformly-distributed point on the unit sphere (Marsaglia's method), drawn
/// from the given SplitMix64 stream. Used to seed random directors.
fn random_unit_vector(state: &mut u64) -> [f32; 3] {
    loop {
        let u = next_uniform(state) * 2.0 - 1.0;
        let v = next_uniform(state) * 2.0 - 1.0;
        let s = u * u + v * v;
        if s < 1.0 && s > 1e-9 {
            let f = 2.0 * (1.0 - s).sqrt();
            return [(u * f) as f32, (v * f) as f32, (1.0 - 2.0 * s) as f32];
        }
    }
}

/// Build the per-node director field (interleaved x,y,z, length `3n`) from the
/// chosen [`DirectorSource`]. Random directors consume `rng`; injected directors
/// are normalised to unit length (a zero/non-finite injected vector falls back to
/// `+z`). The field is built unconditionally — but it only enters the forces when
/// `anisotropy_strength > 0`, so a default run (anisotropy 0) is byte-identical.
fn resolve_directors(
    s: &GeometricSettings,
    n: usize,
    attrs: Option<&GraphAttributes>,
    rng: &mut u64,
) -> Result<Vec<f32>, String> {
    let mut d = vec![0.0f32; 3 * n];
    match &s.director_source {
        DirectorSource::Random => {
            for i in 0..n {
                let u = random_unit_vector(rng);
                d[3 * i] = u[0];
                d[3 * i + 1] = u[1];
                d[3 * i + 2] = u[2];
            }
        }
        DirectorSource::AlignedZ => {
            for i in 0..n {
                d[3 * i + 2] = 1.0;
            }
        }
        DirectorSource::Injected => {
            let src = attrs.and_then(|a| a.node_director.clone()).ok_or_else(|| {
                "director_source = injected but GraphAttributes.node_director is absent".to_string()
            })?;
            for i in 0..n {
                let (x, y, z) = (src[3 * i], src[3 * i + 1], src[3 * i + 2]);
                let len = (x * x + y * y + z * z).sqrt();
                if len.is_finite() && len > 1e-6 {
                    d[3 * i] = x / len;
                    d[3 * i + 1] = y / len;
                    d[3 * i + 2] = z / len;
                } else {
                    d[3 * i + 2] = 1.0; // degenerate ⇒ default +z
                }
            }
        }
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// Resolvers + lookups (structural attribute computation)
// ---------------------------------------------------------------------------

/// CSR out-degree per node (= number of stored neighbours).
fn compute_degree(g: &CsrGraph) -> Vec<u32> {
    let n = g.n_nodes as usize;
    (0..n).map(|v| g.offsets[v + 1] - g.offsets[v]).collect()
}

/// Bucket a value by how many ascending `thresholds` it meets or exceeds.
fn bucket_by_thresholds(value: u32, thresholds: &[u32]) -> u32 {
    let mut bucket = 0u32;
    for &t in thresholds {
        if value >= t {
            bucket += 1;
        } else {
            break;
        }
    }
    bucket
}

/// Look up a coordination angle (degrees) by id, clamping to the table. Empty
/// table ⇒ 120° (a reasonable neutral default).
fn lookup_angle(table: &[f32], id: usize) -> f32 {
    if table.is_empty() {
        return 120.0;
    }
    table[id.min(table.len() - 1)]
}

/// Look up a class radius by id, falling back to `default` beyond the table.
fn lookup_radius(table: &[f32], id: usize, default: f32) -> f32 {
    table.get(id).copied().unwrap_or(default)
}

/// Look up an inter-class affinity from the flattened `n×n` matrix; neutral (0)
/// for ids outside the matrix.
fn lookup_affinity(matrix: &[f32], n: usize, a: usize, b: usize) -> f32 {
    if n == 0 || a >= n || b >= n {
        return 0.0;
    }
    matrix.get(a * n + b).copied().unwrap_or(0.0)
}

/// Per-pair attractive-well depth `ε` for the cohesion term, gated by class.
///
/// `well_depth` is the base depth; the affinity matrix (when present) modulates
/// it so heads/tails can cohere differently: a positive `class_affinity[a][b]`
/// scales the well up, a non-positive one switches it off for that class pair
/// (so a repulsive class pair never also attracts via cohesion). With no affinity
/// matrix (`class_affinity_dim == 0`) the well applies uniformly at full depth —
/// the simple monomer case. Returns `0` (OFF) whenever `well_depth == 0`.
/// Orientation (patchy) factor multiplying a pair's cohesion-well depth.
///
/// Two multiplicative contributions, both default-off (returning exactly `1.0` so
/// the well depth — and therefore both the radial force *and* its energy integral
/// — is byte-identical to before either knob existed):
///
///  - **Patchy / polar (`anisotropy`):** `max(0, 1 + anisotropy·(nᵢ·nⱼ))`. Pairs
///    whose directors (normals) are **aligned** (`nᵢ·nⱼ → +1`) get a deeper well
///    (attract more), anti-aligned pairs a shallower/zero well — the
///    mutually-aligned (nematic) aggregate is the bilayer-sheet precursor. Clamped
///    at 0 so the well never *inverts* into a spurious repulsion.
///  - **Gay–Berne side-by-side (`gb_side`):** `1 + gb_side·(1 − (nᵢ·r̂)²)·(1 − (nⱼ·r̂)²)`,
///    where `r̂` is the unit separation `(uⱼ − uᵢ)/‖·‖`. With the director as the
///    membrane NORMAL, `(1 − (n·r̂)²)` is largest when the separation lies in the
///    tangent plane (`r̂ ⊥ n`), so this *rewards neighbours sitting side-by-side in
///    each other's plane* — spreading into a self-limiting lamella rather than
///    stacking into a nematic droplet. Applied here, the one place the well depth
///    is scaled, so it feeds the radial force and its energy integral identically
///    (the `−∇E == compute_forces` relationship is preserved). Its tangential
///    derivative is intentionally not added to the translational force in this cut.
///
/// `r̂` is passed as its three unit components `(rx, ry, rz)`; callers that only
/// need the patchy factor (none currently) may pass any unit vector when
/// `gb_side == 0`, as that branch is then skipped.
fn orientation_factor(
    directors: &[f32],
    anisotropy: f32,
    gb_side: f32,
    rx: f32,
    ry: f32,
    rz: f32,
    i: usize,
    j: usize,
) -> f32 {
    if anisotropy == 0.0 && gb_side == 0.0 {
        return 1.0;
    }
    let (nix, niy, niz) = (directors[3 * i], directors[3 * i + 1], directors[3 * i + 2]);
    let (njx, njy, njz) = (directors[3 * j], directors[3 * j + 1], directors[3 * j + 2]);

    let mut f = if anisotropy != 0.0 {
        let dot = nix * njx + niy * njy + niz * njz;
        (1.0 + anisotropy * dot).max(0.0)
    } else {
        1.0
    };

    if gb_side != 0.0 {
        // (1 − (n·r̂)²) ∈ [0,1]: 1 when the normal is perpendicular to the
        // separation (side-by-side), 0 when parallel (stacked face-to-face).
        let ir = nix * rx + niy * ry + niz * rz;
        let jr = njx * rx + njy * ry + njz * rz;
        let side = (1.0 - ir * ir) * (1.0 - jr * jr);
        f *= 1.0 + gb_side * side;
    }
    f
}

fn pair_well_depth(s: &GeometricSettings, nc: usize, a: usize, b: usize) -> f32 {
    if s.well_depth <= 0.0 {
        return 0.0;
    }
    if nc == 0 {
        return s.well_depth;
    }
    let aff = lookup_affinity(&s.class_affinity, nc, a, b);
    if aff > 0.0 {
        s.well_depth * aff
    } else {
        0.0
    }
}

/// Linearly normalize `values` into `[lo, hi]`. Constant input maps to `lo`.
fn normalize_to_range(values: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let (mut vmin, mut vmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in values {
        if v.is_finite() {
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
    }
    if !vmin.is_finite() || !vmax.is_finite() || (vmax - vmin).abs() < 1e-12 {
        return vec![lo; values.len()];
    }
    let span = vmax - vmin;
    values
        .iter()
        .map(|&v| {
            let t = ((v - vmin) / span).clamp(0.0, 1.0);
            lo + t * (hi - lo)
        })
        .collect()
}

/// Build the unique undirected edge set (each pair once, `a < b`) with target
/// lengths. When `injected_len` is `Some` (parallel to `g.neighbors`), the
/// length is read from the CSR entry the edge was discovered at; otherwise every
/// edge gets `rest_len`.
fn build_unique_edges(
    g: &CsrGraph,
    rest_len: f32,
    injected_len: Option<&[f32]>,
) -> Vec<ResolvedEdge> {
    let n = g.n_nodes as usize;
    let mut edges = Vec::new();
    for v in 0..n {
        let start = g.offsets[v] as usize;
        let end = g.offsets[v + 1] as usize;
        for e in start..end {
            let u = g.neighbors[e] as usize;
            if u <= v {
                continue; // emit each undirected edge once, skip self-loops
            }
            let target_len = match injected_len {
                Some(lens) => {
                    let l = lens.get(e).copied().unwrap_or(rest_len);
                    if l.is_finite() && l > 0.0 {
                        l
                    } else {
                        rest_len
                    }
                }
                None => rest_len,
            };
            edges.push(ResolvedEdge {
                a: v as u32,
                b: u as u32,
                target_len,
            });
        }
    }
    edges
}

/// Label-propagation community detection (Raghavan et al.). Each node adopts the
/// most frequent label among its neighbours, iterating `passes` synchronous
/// sweeps; labels are then compacted to a dense `[0, k)` range. Deterministic
/// (ties broken by lowest label) so a layout is reproducible. `O(passes·m)`.
fn label_propagation(g: &CsrGraph, passes: u32) -> Vec<u32> {
    let n = g.n_nodes as usize;
    if n == 0 {
        return Vec::new();
    }
    let mut label: Vec<u32> = (0..n as u32).collect();
    let passes = passes.max(1);
    let mut counts: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for _ in 0..passes {
        let mut changed = false;
        for v in 0..n {
            let start = g.offsets[v] as usize;
            let end = g.offsets[v + 1] as usize;
            if start == end {
                continue;
            }
            counts.clear();
            for e in start..end {
                let u = g.neighbors[e] as usize;
                *counts.entry(label[u]).or_insert(0) += 1;
            }
            // Most frequent neighbour label; ties → lowest label id.
            let mut best_label = label[v];
            let mut best_count = 0u32;
            for (&lbl, &cnt) in counts.iter() {
                if cnt > best_count || (cnt == best_count && lbl < best_label) {
                    best_count = cnt;
                    best_label = lbl;
                }
            }
            if best_label != label[v] {
                label[v] = best_label;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    compact_labels(&label)
}

/// Remap arbitrary label ids to a dense `[0, k)` range, preserving grouping.
fn compact_labels(label: &[u32]) -> Vec<u32> {
    let mut remap: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut next = 0u32;
    label
        .iter()
        .map(|&l| {
            *remap.entry(l).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            })
        })
        .collect()
}

/// PageRank over the (undirected, symmetrized) CSR by power iteration. Returns
/// the stationary score per node. `O(iters·m)`.
fn pagerank(g: &CsrGraph, damping: f32, iters: u32) -> Vec<f32> {
    let n = g.n_nodes as usize;
    if n == 0 {
        return Vec::new();
    }
    let inv_n = 1.0 / n as f32;
    let mut rank = vec![inv_n; n];
    let out_deg: Vec<f32> = (0..n)
        .map(|v| (g.offsets[v + 1] - g.offsets[v]) as f32)
        .collect();
    let teleport = (1.0 - damping) * inv_n;
    for _ in 0..iters {
        let mut next = vec![0.0f32; n];
        let mut dangling = 0.0f32;
        for v in 0..n {
            if out_deg[v] == 0.0 {
                dangling += rank[v];
                continue;
            }
            let share = rank[v] / out_deg[v];
            let start = g.offsets[v] as usize;
            let end = g.offsets[v + 1] as usize;
            for e in start..end {
                let u = g.neighbors[e] as usize;
                next[u] += share;
            }
        }
        let dangling_share = damping * dangling * inv_n;
        for v in 0..n {
            next[v] = teleport + dangling_share + damping * next[v];
        }
        rank = next;
    }
    rank
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

    /// A triangle: 0-1, 1-2, 2-0.
    fn triangle() -> CsrGraph {
        // offsets/neighbors for a 3-cycle
        CsrGraph {
            n_nodes: 3,
            offsets: vec![0, 2, 4, 6],
            neighbors: vec![1, 2, 0, 2, 0, 1],
        }
    }

    /// Two triangles joined by a single bridge edge (3)-(4):
    /// {0,1,2} clique and {3,4,5} clique, bridge 2-3.
    fn dumbbell() -> CsrGraph {
        // adjacency:
        // 0:1,2  1:0,2  2:0,1,3  3:2,4,5  4:3,5  5:3,4
        let neighbors = vec![
            1, 2, /*0*/ 0, 2, /*1*/ 0, 1, 3, /*2*/ 2, 4, 5, /*3*/ 3, 5,
            /*4*/ 3, 4, /*5*/
        ];
        let offsets = vec![0, 2, 4, 7, 10, 12, 14];
        CsrGraph {
            n_nodes: 6,
            offsets,
            neighbors,
        }
    }

    fn init_engine(g: &CsrGraph, settings: GeometricSettings, pos: &[f32]) -> GeometricEngine {
        let mut e = GeometricEngine::new();
        e.settings = settings;
        let mut ctx = EngineCtx::cpu_only();
        let shard = CsrShard::whole(g);
        e.init(&mut ctx, &shard, pos).expect("init");
        e
    }

    // -----------------------------------------------------------------------
    // Numerical-stability hardening: degenerate geometry must never produce
    // NaN/Inf. Each case enables EVERY force term that has a singularity
    // (exclusion + cohesion well + patchy anisotropy + GB-side + tilt coupling
    // + bending + a thermostat) so the coincident-particle / zero-length /
    // collinear / single-node guards are actually exercised, then steps several
    // times and asserts every position AND velocity stays finite.
    // -----------------------------------------------------------------------

    /// A maximally-hazardous settings block: all the divide-by-distance,
    /// normalize, acos, and sqrt force terms are turned ON at once.
    fn hardening_settings() -> GeometricSettings {
        GeometricSettings {
            coordination_source: CoordinationSource::Degree,
            director_source: DirectorSource::Random,
            edge_rest_len: 1.0,
            edge_stiffness: 0.3,
            angle_stiffness: 0.2,
            // exclusion + cohesion well (radial divide-by-distance + cos²)
            default_radius: 0.5,
            exclusion_strength: 1.0,
            well_depth: 1.0,
            well_width: 1.0,
            // orientation-dependent depth (patchy + Gay–Berne side-by-side)
            anisotropy_strength: 0.8,
            gb_side_strength: 0.5,
            // director→position tilt coupling + bending torque + curvature
            tilt_coupling_strength: 0.4,
            kappa_bend: 0.3,
            spont_curvature_c0: 0.2,
            // a thermostat so the sqrt(kT/m) + Box–Muller paths run too
            temperature: 0.5,
            rotational_diffusion: 1.0,
            gravity: 0.02,
            damping: 0.9,
            time_step: 1.0,
            max_step: 10.0,
            ..GeometricSettings::default()
        }
    }

    /// Run `steps` and assert no position or velocity is ever non-finite.
    fn assert_finite_after_steps(mut e: GeometricEngine, steps: usize, label: &str) {
        let mut ctx = EngineCtx::cpu_only();
        for s in 0..steps {
            let out = e.step(&mut ctx);
            for (i, &p) in out.positions.iter().enumerate() {
                assert!(
                    p.is_finite(),
                    "{label}: non-finite position[{i}] = {p} at step {s}"
                );
            }
            if let Some(st) = e.state.as_ref() {
                for (i, &v) in st.velocities.iter().enumerate() {
                    assert!(
                        v.is_finite(),
                        "{label}: non-finite velocity[{i}] = {v} at step {s}"
                    );
                }
                for (i, &d) in st.directors.iter().enumerate() {
                    assert!(
                        d.is_finite(),
                        "{label}: non-finite director[{i}] = {d} at step {s}"
                    );
                }
            }
        }
    }

    #[test]
    fn degenerate_coincident_pair_stays_finite() {
        // Two nodes at the EXACT same position (zero separation) — the canonical
        // divide-by-distance / normalize-zero-vector singularity.
        let g = single_edge_csr();
        let pos = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let e = init_engine(&g, hardening_settings(), &pos);
        assert_finite_after_steps(e, 20, "coincident-pair");
    }

    #[test]
    fn degenerate_zero_length_edge_stays_finite() {
        // An edge whose endpoints coincide: the spring's `(dist - target)/dist`
        // with dist → 0 plus exclusion at zero distance.
        let g = triangle(); // every node has degree 2 (angle term active)
        let pos = vec![
            1.0, 1.0, 1.0, // node 0
            1.0, 1.0, 1.0, // node 1 — coincident with 0 ⇒ zero-length edge 0-1
            2.0, 0.0, 0.0, // node 2
        ];
        let e = init_engine(&g, hardening_settings(), &pos);
        assert_finite_after_steps(e, 20, "zero-length-edge");
    }

    #[test]
    fn degenerate_all_same_position_cluster_stays_finite() {
        // A tight cluster of many nodes ALL at the same point: every pair is a
        // zero-distance exclusion/cohesion pair simultaneously.
        let n = 8;
        let g = no_edges_csr(n);
        let pos = vec![0.0f32; 3 * n];
        let e = init_engine(&g, hardening_settings(), &pos);
        assert_finite_after_steps(e, 20, "all-same-position-cluster");
    }

    #[test]
    fn degenerate_collinear_angle_triple_stays_finite() {
        // A degree-2 center with two neighbours exactly collinear through it
        // (θ = π, sin θ = 0): the angle-gradient `coef / sin_t` singularity.
        // node 1 is the center; 0 and 2 are antiparallel about it.
        let g = path_csr(3); // 0-1-2, node 1 has degree 2
        let pos = vec![
            -1.0, 0.0, 0.0, // node 0
            0.0, 0.0, 0.0, // node 1 (center)
            1.0, 0.0, 0.0, // node 2 — exactly collinear ⇒ θ = π
        ];
        let mut s = hardening_settings();
        s.temperature = 0.0; // keep it deterministically collinear at step 0
        let e = init_engine(&g, s, &pos);
        assert_finite_after_steps(e, 20, "collinear-angle-triple");
    }

    #[test]
    fn degenerate_single_node_stays_finite() {
        let g = no_edges_csr(1);
        let pos = vec![0.3, -0.4, 0.5];
        let e = init_engine(&g, hardening_settings(), &pos);
        assert_finite_after_steps(e, 10, "single-node");
    }

    #[test]
    fn degenerate_empty_graph_steps_cleanly() {
        let g = no_edges_csr(0);
        let pos: Vec<f32> = Vec::new();
        let mut e = init_engine(&g, hardening_settings(), &pos);
        let mut ctx = EngineCtx::cpu_only();
        // The empty-graph fast path returns an empty buffer without dividing by n.
        let out = e.step(&mut ctx);
        assert!(out.positions.is_empty(), "empty graph ⇒ no positions");
    }

    #[test]
    fn extreme_temperature_stays_finite() {
        // A very large kT for a few steps: the sqrt(kT/m) thermal kick + the
        // max_step clamp + the new non-finite integrator guard together must
        // keep everything bounded and finite.
        let g = triangle();
        let pos = deterministic_spread(3, 0.5);
        let mut s = hardening_settings();
        s.temperature = 1.0e6; // extreme thermostat
        let e = init_engine(&g, s, &pos);
        assert_finite_after_steps(e, 30, "extreme-temperature");
    }

    // --- small graph + seed builders used by the hardening tests ---------

    /// Single undirected edge 0-1.
    fn single_edge_csr() -> CsrGraph {
        CsrGraph {
            n_nodes: 2,
            offsets: vec![0, 1, 2],
            neighbors: vec![1, 0],
        }
    }

    /// Undirected path 0-1-2-…-(n-1).
    fn path_csr(n: usize) -> CsrGraph {
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        for v in 0..n {
            if v > 0 {
                neighbors.push(v as u32 - 1);
            }
            if v + 1 < n {
                neighbors.push(v as u32 + 1);
            }
            offsets.push(neighbors.len() as u32);
        }
        CsrGraph {
            n_nodes: n as u32,
            offsets,
            neighbors,
        }
    }

    /// `n` isolated nodes (no edges) — the exclusion/cohesion-only fixture.
    fn no_edges_csr(n: usize) -> CsrGraph {
        CsrGraph {
            n_nodes: n as u32,
            offsets: vec![0u32; n + 1],
            neighbors: Vec::new(),
        }
    }

    /// A deterministic, mildly-spread seed (interleaved x,y,z).
    fn deterministic_spread(n: usize, spread: f32) -> Vec<f32> {
        let mut p = vec![0.0f32; 3 * n];
        for i in 0..n {
            let t = i as f32;
            p[3 * i] = (t * 1.7).sin() * spread;
            p[3 * i + 1] = (t * 2.3).cos() * spread;
            p[3 * i + 2] = (t * 0.9).sin() * spread;
        }
        p
    }

    #[test]
    fn degree_and_buckets() {
        let g = dumbbell();
        let deg = compute_degree(&g);
        assert_eq!(deg, vec![2, 2, 3, 3, 2, 2]);
        // thresholds [3] ⇒ degree<3 → 0, degree>=3 → 1
        assert_eq!(bucket_by_thresholds(2, &[3]), 0);
        assert_eq!(bucket_by_thresholds(3, &[3]), 1);
        assert_eq!(bucket_by_thresholds(20, &[3, 10]), 2);
    }

    #[test]
    fn label_propagation_splits_dumbbell() {
        let g = dumbbell();
        let comm = label_propagation(&g, 20);
        assert_eq!(comm.len(), 6);
        // The two triangles should land in (at most) a small number of
        // communities; crucially nodes within a triangle share a label.
        // We assert the dense-compaction invariant and that the bridge doesn't
        // collapse everything to a single community OR explode to 6.
        let k = comm.iter().copied().max().unwrap() + 1;
        assert!(
            (1..=3).contains(&k),
            "unexpected community count {k}: {comm:?}"
        );
    }

    #[test]
    fn pagerank_ranks_hub_highest() {
        // Star: node 0 connected to 1,2,3,4 (undirected).
        let neighbors = vec![
            1, 2, 3, 4, /*0*/ 0, /*1*/ 0, /*2*/ 0, /*3*/ 0, /*4*/
        ];
        let offsets = vec![0, 4, 5, 6, 7, 8];
        let g = CsrGraph {
            n_nodes: 5,
            offsets,
            neighbors,
        };
        let pr = pagerank(&g, 0.85, 50);
        let hub = pr[0];
        for leaf in &pr[1..] {
            assert!(hub > *leaf, "hub {hub} should outrank leaf {leaf}");
        }
    }

    #[test]
    fn injected_class_required_when_selected() {
        let g = triangle();
        let mut s = GeometricSettings::default();
        s.class_source = ClassSource::Injected;
        let mut e = GeometricEngine::new();
        e.settings = s;
        let mut ctx = EngineCtx::cpu_only();
        let shard = CsrShard::whole(&g); // no attributes
        let err = e
            .init(&mut ctx, &shard, &ring_positions(3))
            .expect_err("should require injected node_class");
        assert!(err.contains("node_class"), "got: {err}");
    }

    #[test]
    fn injected_attributes_validate_length() {
        let g = triangle();
        let attrs = GraphAttributes {
            node_class: Some(vec![0, 1]), // wrong length (n=3)
            ..Default::default()
        };
        let err = attrs.validate(&g).expect_err("length mismatch");
        assert!(err.contains("node_class"), "got: {err}");
    }

    #[test]
    fn injected_class_drives_resolution() {
        let g = triangle();
        let attrs = GraphAttributes {
            node_class: Some(vec![0, 1, 2]),
            ..Default::default()
        };
        let mut s = GeometricSettings::default();
        s.class_source = ClassSource::Injected;
        let resolved = GeometricEngine::resolve(&s, &g, Some(&attrs)).expect("resolve");
        assert_eq!(resolved.class, vec![0, 1, 2]);
    }

    #[test]
    fn injected_edge_len_parallel_to_neighbors() {
        // Path 0-1-2: neighbors = [1, 0,2, 1] (offsets [0,1,3,4]).
        let g = CsrGraph::path(3);
        // Give the CSR entries distinct lengths; unique edges are (0,1) and (1,2).
        // neighbors order: idx0=(0->1), idx1=(1->0), idx2=(1->2), idx3=(2->1).
        let edge_len = vec![5.0, 99.0, 7.0, 99.0];
        let attrs = GraphAttributes {
            edge_len: Some(edge_len),
            ..Default::default()
        };
        let mut s = GeometricSettings::default();
        s.edge_length_source = EdgeLengthSource::Injected;
        let resolved = GeometricEngine::resolve(&s, &g, Some(&attrs)).expect("resolve");
        // Unique edges emitted at the entry where u > v: (0,1) at idx0 → 5.0,
        // (1,2) at idx2 → 7.0.
        assert_eq!(resolved.edges.len(), 2);
        let (e01, e12) = (&resolved.edges[0], &resolved.edges[1]);
        assert_eq!((e01.a, e01.b), (0, 1));
        assert!((e01.target_len - 5.0).abs() < 1e-6);
        assert_eq!((e12.a, e12.b), (1, 2));
        assert!((e12.target_len - 7.0).abs() < 1e-6);
    }

    #[test]
    fn angle_constraint_pushes_triangle_toward_ideal() {
        // A triangle of degree-2 nodes; coordination angle for degree 2 is 180°
        // by default — but a triangle can't be straight, so instead set a custom
        // table making degree-2 want 60° (the actual interior angle of an
        // equilateral triangle), and assert the angle error shrinks.
        let g = triangle();
        // Start from a deliberately skewed triangle.
        let pos = vec![
            0.0, 0.0, 0.0, // 0
            2.0, 0.0, 0.0, // 1
            0.2, 0.3, 0.0, // 2 (squished)
        ];
        let mut s = GeometricSettings::default();
        s.coordination_source = CoordinationSource::Uniform { bucket: 0 };
        s.coordination_angles = vec![60.0]; // every node wants 60° between neighbours
        s.angle_stiffness = 0.2;
        s.edge_stiffness = 0.1;
        s.exclusion_strength = 0.0; // isolate the angle behaviour
        s.gravity = 0.0;

        let mut e = init_engine(&g, s, &pos);
        let mut ctx = EngineCtx::cpu_only();

        let before = mean_abs_angle_error(&g, &pos, 60.0);
        let mut out = pos.clone();
        for _ in 0..200 {
            out = e.step(&mut ctx).positions;
        }
        let after = mean_abs_angle_error(&g, &out, 60.0);
        assert!(
            after < before,
            "angle error should shrink: before={before} after={after}"
        );
    }

    #[test]
    fn negative_affinity_separates_two_classes() {
        // Two disconnected pairs, classes {0,0} and {1,1}, with strong negative
        // (repulsive) cross-affinity. The two classes' centroids should move
        // apart over time.
        // Graph: 0-1 and 2-3 (two separate edges).
        let neighbors = vec![1, /*0*/ 0, /*1*/ 3, /*2*/ 2 /*3*/];
        let offsets = vec![0, 1, 2, 3, 4];
        let g = CsrGraph {
            n_nodes: 4,
            offsets,
            neighbors,
        };
        // Place all four near the origin so affinity dominates initial motion.
        let pos = vec![
            -0.1, 0.0, 0.0, // 0 (class 0)
            0.1, 0.0, 0.0, // 1 (class 0)
            0.0, 0.1, 0.0, // 2 (class 1)
            0.0, -0.1, 0.0, // 3 (class 1)
        ];
        let attrs = GraphAttributes {
            node_class: Some(vec![0, 0, 1, 1]),
            ..Default::default()
        };
        let mut s = GeometricSettings::default();
        s.class_source = ClassSource::Injected;
        // 2x2 affinity: within-class attract (+), cross-class repel (-).
        s.class_affinity_dim = 2;
        s.class_affinity = vec![
            1.0, -1.0, // class 0 vs {0,1}
            -1.0, 1.0, // class 1 vs {0,1}
        ];
        s.affinity_strength = 0.5;
        s.exclusion_strength = 0.0;
        s.gravity = 0.0;
        s.angle_stiffness = 0.0;

        let mut e = GeometricEngine::new();
        e.settings = s;
        let mut ctx = EngineCtx::cpu_only();
        let shard = CsrShard::whole_with_attributes(&g, &attrs);
        e.init(&mut ctx, &shard, &pos).expect("init");

        // Use mean cross-class pairwise distance (symmetry-independent): centroids
        // happen to coincide at the origin here by construction, so a centroid
        // metric would be blind to the (real) repulsion spreading each class.
        let before = mean_cross_class_distance(&pos, &[0, 0, 1, 1]);
        let mut out = pos.clone();
        for _ in 0..100 {
            out = e.step(&mut ctx).positions;
        }
        let after = mean_cross_class_distance(&out, &[0, 0, 1, 1]);
        assert!(
            after > before,
            "classes should separate: before={before} after={after}"
        );
    }

    #[test]
    fn registry_exposes_geometric() {
        let reg = crate::engines::EngineRegistry::builtin();
        assert!(reg.contains(GeometricEngine::ID));
        let e = crate::engines::construct_leaf(GeometricEngine::ID).expect("constructible");
        assert_eq!(e.descriptor().id, GeometricEngine::ID);
    }

    #[test]
    fn handles_empty_and_singleton() {
        for n in [0u32, 1] {
            let g = CsrGraph::path(n);
            let pos = ring_positions(n as usize);
            let mut e = init_engine(&g, GeometricSettings::default(), &pos);
            let mut ctx = EngineCtx::cpu_only();
            let out = e.step(&mut ctx).positions;
            assert_eq!(out.len(), pos.len());
        }
    }

    #[test]
    fn settings_roundtrip_json() {
        let s = GeometricSettings::default();
        let v = serde_json::to_value(&s).expect("serialize");
        let back: GeometricSettings = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back.class_source, s.class_source);
        assert_eq!(back.coordination_source, s.coordination_source);
        assert_eq!(back.edge_rest_len, s.edge_rest_len);
    }

    #[test]
    fn source_enums_tagged_json_shape() {
        // Confirm the wire shape the frontend will author: internally-tagged.
        let v = serde_json::json!({
            "class_source": {"kind": "community", "passes": 8},
            "mass_source": {"kind": "page_rank", "damping_milli": 850, "iters": 40},
            "coordination_source": {"kind": "degree"},
            "edge_length_source": {"kind": "injected"}
        });
        let s: GeometricSettings = serde_json::from_value(v).expect("decode tagged");
        assert_eq!(s.class_source, ClassSource::Community { passes: 8 });
        assert_eq!(
            s.mass_source,
            MassSource::PageRank {
                damping_milli: 850,
                iters: 40
            }
        );
        assert_eq!(s.edge_length_source, EdgeLengthSource::Injected);
    }

    // --- test helpers ---

    fn mean_abs_angle_error(g: &CsrGraph, pos: &[f32], ideal_deg: f32) -> f32 {
        let ideal = ideal_deg.to_radians();
        let mut total = 0.0f32;
        let mut count = 0u32;
        let n = g.n_nodes as usize;
        for c in 0..n {
            let start = g.offsets[c] as usize;
            let end = g.offsets[c + 1] as usize;
            let neigh = &g.neighbors[start..end];
            for jj in 0..neigh.len() {
                for kk in (jj + 1)..neigh.len() {
                    let j = neigh[jj] as usize;
                    let k = neigh[kk] as usize;
                    let a = [
                        pos[3 * j] - pos[3 * c],
                        pos[3 * j + 1] - pos[3 * c + 1],
                        pos[3 * j + 2] - pos[3 * c + 2],
                    ];
                    let b = [
                        pos[3 * k] - pos[3 * c],
                        pos[3 * k + 1] - pos[3 * c + 1],
                        pos[3 * k + 2] - pos[3 * c + 2],
                    ];
                    let la = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-6);
                    let lb = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt().max(1e-6);
                    let cos =
                        ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)).clamp(-1.0, 1.0);
                    total += (cos.acos() - ideal).abs();
                    count += 1;
                }
            }
        }
        if count == 0 {
            0.0
        } else {
            total / count as f32
        }
    }

    /// Mean Euclidean distance over all cross-class node pairs. Increases when
    /// classes repel, regardless of where their centroids sit (so it survives
    /// the symmetric layout the separation test uses).
    fn mean_cross_class_distance(pos: &[f32], class: &[u32]) -> f32 {
        let n = class.len();
        let mut total = 0.0f32;
        let mut count = 0u32;
        for i in 0..n {
            for j in (i + 1)..n {
                if class[i] == class[j] {
                    continue;
                }
                let dx = pos[3 * i] - pos[3 * j];
                let dy = pos[3 * i + 1] - pos[3 * j + 1];
                let dz = pos[3 * i + 2] - pos[3 * j + 2];
                total += (dx * dx + dy * dy + dz * dz).sqrt();
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            total / count as f32
        }
    }

    // -----------------------------------------------------------------------
    // Dynamic bonding (P1) — cell-list neighbour search internals
    // -----------------------------------------------------------------------

    /// Brute O(n²) candidate set: every unordered pair within `cutoff` (the same
    /// geometric question the cell list answers, by exhaustive scan). Returns
    /// canonical `(a, b)` pairs sorted — the reference the cell list must match.
    fn brute_pairs_within(pos: &[f32], n: usize, cutoff: f32) -> Vec<(u32, u32)> {
        let c2 = cutoff * cutoff;
        let mut out = Vec::new();
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[3 * j] - pos[3 * i];
                let dy = pos[3 * j + 1] - pos[3 * i + 1];
                let dz = pos[3 * j + 2] - pos[3 * i + 2];
                if dx * dx + dy * dy + dz * dz <= c2 {
                    out.push((i as u32, j as u32));
                }
            }
        }
        out.sort();
        out
    }

    /// All candidate pairs the cell list yields within `cutoff`, with cell =
    /// `cutoff` (the design's invariant: cell size = the cutoff ⇒ the 27-cell
    /// stencil covers every pair). Canonical + sorted, like the brute reference.
    fn celllist_pairs_within(pos: &[f32], n: usize, cutoff: f32) -> Vec<(u32, u32)> {
        let grid = CellList::build(pos, n, cutoff);
        let c2 = cutoff * cutoff;
        let mut out = Vec::new();
        for i in 0..n {
            grid.for_each_candidate(pos, i, |j| {
                let dx = pos[3 * j] - pos[3 * i];
                let dy = pos[3 * j + 1] - pos[3 * i + 1];
                let dz = pos[3 * j + 2] - pos[3 * i + 2];
                if dx * dx + dy * dy + dz * dz <= c2 {
                    out.push(canon_pair(i, j));
                }
            });
        }
        out.sort();
        out
    }

    #[test]
    fn celllist_matches_brute_candidate_set() {
        // The cell list (cell = cutoff, 27-cell scan) must find EXACTLY the same
        // in-range pair set as the O(n²) brute scan — across a deterministic cloud
        // with both positive and negative coordinates (floor-vs-truncate cell
        // bucketing) at several cutoffs.
        let n = 200usize;
        let pos = deterministic_spread(n, 6.0);
        for &cutoff in &[0.5f32, 1.0, 1.3, 2.0, 3.5] {
            let brute = brute_pairs_within(&pos, n, cutoff);
            let cells = celllist_pairs_within(&pos, n, cutoff);
            assert_eq!(
                cells, brute,
                "cell-list candidate set must equal brute scan at cutoff {cutoff} \
                 (brute {} pairs, cells {} pairs)",
                brute.len(),
                cells.len()
            );
        }
    }

    #[test]
    fn celllist_handles_coincident_and_single_node() {
        // Degenerate clouds: all-coincident (every pair in range) and a single
        // node (no pairs). The cell list must still match the brute set.
        let coincident = vec![0.0f32; 3 * 5];
        assert_eq!(
            celllist_pairs_within(&coincident, 5, 1.0),
            brute_pairs_within(&coincident, 5, 1.0)
        );
        let single = vec![0.3f32, -0.4, 0.5];
        assert!(celllist_pairs_within(&single, 1, 1.0).is_empty());
    }

    /// A geometric helper to drive the bond stage on a hand-built configuration.
    fn bonding_settings(r_bond: f32, r_break: f32) -> GeometricSettings {
        GeometricSettings {
            bonding_enabled: true,
            r_bond,
            r_break,
            bond_stiffness: 0.3,
            bond_every: 1,
            // Hold positions essentially still so the test controls geometry: no
            // springs (no static edges anyway), no exclusion/cohesion, no gravity,
            // no thermostat. The bond stage is what we are validating.
            edge_stiffness: 0.0,
            angle_stiffness: 0.0,
            exclusion_strength: 0.0,
            well_depth: 0.0,
            gravity: 0.0,
            temperature: 0.0,
            ..GeometricSettings::default()
        }
    }

    #[test]
    fn bonds_form_in_range_and_break_past_r_break() {
        // Hand-built 3-node line on the x-axis. With r_bond = 1.0, r_break = 1.5:
        //   0 at x=0, 1 at x=0.8 (within r_bond of 0), 2 at x=2.0 (1.2 from 1,
        //   2.0 from 0). Expect exactly the 0-1 bond to form (0.8 ≤ 1.0); 1-2 is
        //   1.2 > 1.0 (no bond); 0-2 is 2.0 (no bond).
        let g = no_edges_csr(3);
        let pos = vec![0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 2.0, 0.0, 0.0];
        let mut e = init_engine(&g, bonding_settings(1.0, 1.5), &pos);
        let mut ctx = EngineCtx::cpu_only();
        e.step(&mut ctx); // runs the bond stage (bond_every = 1)
        let bonds = e.dynamic_bonds().unwrap();
        assert_eq!(bonds, vec![(0, 1)], "only the in-range 0-1 pair should bond");

        // Now pull node 1 far past r_break of node 0 (and away from node 2) by
        // mutating its position and stepping again: the 0-1 bond must BREAK
        // (hysteresis upper edge) and no new bond should form.
        {
            let st = e.state.as_mut().unwrap();
            st.positions[3] = -2.0; // node 1 → x=-2.0: dist(0,1)=2.0 > r_break 1.5,
                                    // dist(1,2)=4.0 (no new bond)
        }
        e.step(&mut ctx);
        let bonds = e.dynamic_bonds().unwrap();
        assert!(
            bonds.is_empty(),
            "the 0-1 bond must break past r_break, got {bonds:?}"
        );
    }

    #[test]
    fn bond_persists_inside_hysteresis_band() {
        // A bond formed at r_bond must NOT break while r_bond < dist ≤ r_break —
        // the no-flicker guarantee. Form 0-1 at 0.9 (< r_bond 1.0), then stretch
        // to 1.3 (between r_bond 1.0 and r_break 1.5): the bond persists; it does
        // NOT re-create either (already bonded), and it does not break.
        let g = no_edges_csr(2);
        let pos = vec![0.0, 0.0, 0.0, 0.9, 0.0, 0.0];
        let mut e = init_engine(&g, bonding_settings(1.0, 1.5), &pos);
        let mut ctx = EngineCtx::cpu_only();
        e.step(&mut ctx);
        assert_eq!(e.dynamic_bonds().unwrap(), vec![(0, 1)]);
        {
            let st = e.state.as_mut().unwrap();
            st.positions[3] = 1.3; // inside (r_bond, r_break]
        }
        e.step(&mut ctx);
        assert_eq!(
            e.dynamic_bonds().unwrap(),
            vec![(0, 1)],
            "a bond in the hysteresis band must persist (no flicker)"
        );
    }

    #[test]
    fn bonds_respect_class_compatibility() {
        // Two nodes well within r_bond but with a NEGATIVE inter-class affinity
        // (class 0 vs class 1, affinity[0][1] = -1) must NOT bond. The positive
        // self-affinity 0-0 case bonds, confirming the sign gate works both ways.
        let g = no_edges_csr(2);
        let pos = vec![0.0, 0.0, 0.0, 0.5, 0.0, 0.0]; // dist 0.5 < r_bond 1.0

        // Incompatible: class [0,1], affinity[0][1] = -1 ⇒ no bond.
        let mut s = bonding_settings(1.0, 1.5);
        s.class_source = ClassSource::Injected;
        s.class_affinity_dim = 2;
        s.class_affinity = vec![1.0, -1.0, -1.0, 1.0];
        let attrs = GraphAttributes {
            node_class: Some(vec![0, 1]),
            ..Default::default()
        };
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        e.init(&mut ctx, &CsrShard::whole_with_attributes(&g, &attrs), &pos)
            .unwrap();
        e.step(&mut ctx);
        assert!(
            e.dynamic_bonds().unwrap().is_empty(),
            "an affinity<0 class pair must not bond"
        );

        // Compatible: same matrix but both class 0 ⇒ affinity[0][0] = +1 ⇒ bond.
        let attrs0 = GraphAttributes {
            node_class: Some(vec![0, 0]),
            ..Default::default()
        };
        let mut e2 = GeometricEngine::new();
        e2.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        e2.init(&mut ctx, &CsrShard::whole_with_attributes(&g, &attrs0), &pos)
            .unwrap();
        e2.step(&mut ctx);
        assert_eq!(
            e2.dynamic_bonds().unwrap(),
            vec![(0, 1)],
            "an affinity>0 class pair within range should bond"
        );
    }

    // -----------------------------------------------------------------------
    // Dynamic bonding (P2) — per-class valence cap + bond angle
    // -----------------------------------------------------------------------

    /// Degree (dynamic-bond valence) histogram over `n` nodes from a bond list.
    fn bond_degree_histogram(n: usize, bonds: &[(u32, u32)]) -> Vec<u32> {
        let mut deg = vec![0u32; n];
        for &(a, b) in bonds {
            deg[a as usize] += 1;
            deg[b as usize] += 1;
        }
        deg
    }

    #[test]
    fn valence_cap_rejects_bonds_past_the_class_cap() {
        // A central node 0 surrounded by FOUR equidistant partners all well within
        // r_bond. With a uniform valence cap of 2, node 0 must end up with EXACTLY
        // 2 dynamic bonds — never 3 or 4 — and the accept/reject must be
        // deterministic (sorted candidate order ⇒ the two lowest-index partners
        // win). No partner exceeds the cap either.
        let g = no_edges_csr(5);
        // node 0 at origin; 1..4 at ±0.3 on x and y (all dist 0.3 < r_bond 1.0,
        // and partners are ≥ 0.42 apart from each other ... but several partner
        // pairs are also < r_bond, so the cap must hold globally, not just at 0).
        let pos = vec![
            0.0, 0.0, 0.0, // 0 (center)
            0.3, 0.0, 0.0, // 1
            -0.3, 0.0, 0.0, // 2
            0.0, 0.3, 0.0, // 3
            0.0, -0.3, 0.0, // 4
        ];
        let mut s = bonding_settings(1.0, 1.5);
        s.default_max_valence = 2;
        let mut e = init_engine(&g, s, &pos);
        let mut ctx = EngineCtx::cpu_only();
        e.step(&mut ctx);
        let bonds = e.dynamic_bonds().unwrap();
        let deg = bond_degree_histogram(5, &bonds);
        assert!(
            deg.iter().all(|&d| d <= 2),
            "no node may exceed the valence cap of 2, got degrees {deg:?} (bonds {bonds:?})"
        );
        // Determinism: re-run from scratch on the identical config ⇒ identical set.
        let mut e2 = init_engine(&g, {
            let mut s2 = bonding_settings(1.0, 1.5);
            s2.default_max_valence = 2;
            s2
        }, &pos);
        e2.step(&mut ctx);
        assert_eq!(
            bonds,
            e2.dynamic_bonds().unwrap(),
            "valence-capped bond set must be deterministic across runs"
        );
    }

    #[test]
    fn valence_cap_zero_is_uncapped_p1_behaviour() {
        // default_max_valence = 0 and an empty table ⇒ NO cap ⇒ identical to P1:
        // every in-range pair bonds. A tight 4-cluster (all pairs in range) bonds
        // into the complete graph K4 (6 edges, every node degree 3).
        let g = no_edges_csr(4);
        let pos = vec![
            0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.2,
        ];
        let e = {
            let mut e = init_engine(&g, bonding_settings(1.0, 1.5), &pos);
            let mut ctx = EngineCtx::cpu_only();
            e.step(&mut ctx);
            e
        };
        let bonds = e.dynamic_bonds().unwrap();
        assert_eq!(bonds.len(), 6, "uncapped tight 4-cluster ⇒ K4 (6 bonds), got {bonds:?}");
        let deg = bond_degree_histogram(4, &bonds);
        assert!(deg.iter().all(|&d| d == 3), "K4 ⇒ every node degree 3, got {deg:?}");
    }

    #[test]
    fn valence_cap_is_per_class() {
        // Two classes with different caps via an injected class vector. Node 0 is
        // class 0 (cap 1); nodes 1,2,3 are class 1 (cap 3). All four mutually in
        // range. The affinity matrix is all-positive so every pair is compatible.
        // Node 0 must accept at most 1 bond despite three willing partners.
        let g = no_edges_csr(4);
        let pos = vec![
            0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.2,
        ];
        let mut s = bonding_settings(1.0, 1.5);
        s.class_source = ClassSource::Injected;
        s.class_affinity_dim = 2;
        s.class_affinity = vec![1.0, 1.0, 1.0, 1.0]; // all classes compatible
        s.max_valence = vec![1, 3]; // class 0 cap 1, class 1 cap 3
        let attrs = GraphAttributes {
            node_class: Some(vec![0, 1, 1, 1]),
            ..Default::default()
        };
        let mut e = GeometricEngine::new();
        e.set_params(&serde_json::to_value(&s).unwrap()).unwrap();
        let mut ctx = EngineCtx::cpu_only();
        e.init(&mut ctx, &CsrShard::whole_with_attributes(&g, &attrs), &pos)
            .unwrap();
        e.step(&mut ctx);
        let bonds = e.dynamic_bonds().unwrap();
        let deg = bond_degree_histogram(4, &bonds);
        assert!(
            deg[0] <= 1,
            "class-0 node must respect its cap of 1, got degree {} (bonds {bonds:?})",
            deg[0]
        );
        assert!(
            deg[1..].iter().all(|&d| d <= 3),
            "class-1 nodes must respect their cap of 3, got {deg:?}"
        );
    }

    #[test]
    fn dynamic_bond_angle_term_acts_on_bonded_triple() {
        // Three nodes bonded into a bent chain 1-0-2 (node 0 is the center, bonded
        // to 1 and 2). With a 180° target bond angle the angle term must push the
        // two outer nodes apart (straighten the chain). We verify the FORCE on the
        // bent triple is non-trivial and straightening: compare θ after a few steps
        // against the bent start — it should increase toward 180°.
        let g = no_edges_csr(3);
        // Bent: 1 at (-1,0,0), 0 at origin, 2 at (0.6,0.8,0) ⇒ angle ≈ 127°.
        let pos = vec![
            0.0, 0.0, 0.0, // 0 center
            -1.0, 0.0, 0.0, // 1
            0.6, 0.8, 0.0, // 2
        ];
        let mut s = bonding_settings(1.3, 1.8);
        s.angle_stiffness = 0.3;
        s.bond_stiffness = 0.0; // isolate the angle term (no length spring)
        s.default_bond_angle = 180.0;
        s.bond_every = 1;
        s.time_step = 0.2;
        let mut e = init_engine(&g, s, &pos);
        let mut ctx = EngineCtx::cpu_only();
        e.step(&mut ctx); // forms 0-1 and 0-2 bonds, applies angle force

        let theta0 = {
            // initial angle 1-0-2
            super::triple_angle(&pos, 0, 1, 2).to_degrees()
        };
        for _ in 0..40 {
            e.step(&mut ctx);
        }
        let st = e.state.as_ref().unwrap();
        // Confirm the bonded triple still exists (center degree 2).
        let bonds = e.dynamic_bonds().unwrap();
        assert_eq!(bonds.len(), 2, "center should be bonded to both ends, got {bonds:?}");
        let theta1 = super::triple_angle(&st.positions, 0, 1, 2).to_degrees();
        assert!(
            theta1 > theta0 + 2.0,
            "the 180° bond-angle term should STRAIGHTEN the bent chain: \
             θ went {theta0:.1}° → {theta1:.1}° (expected to increase toward 180°)"
        );
    }

    #[test]
    fn bonding_disabled_creates_no_dynamic_edges() {
        // The hard default-OFF guarantee: with bonding_enabled = false, even a
        // tight cluster (every pair in range) produces ZERO dynamic edges, so the
        // force pass sees only static edges (byte-identical default behaviour).
        let g = no_edges_csr(6);
        let pos = vec![0.0f32; 3 * 6]; // all coincident ⇒ all in range if enabled
        let mut s = bonding_settings(1.0, 1.5);
        s.bonding_enabled = false;
        let mut e = init_engine(&g, s, &pos);
        let mut ctx = EngineCtx::cpu_only();
        for _ in 0..10 {
            e.step(&mut ctx);
        }
        assert!(
            e.dynamic_bonds().unwrap().is_empty(),
            "bonding disabled must never create dynamic edges"
        );
    }
}
