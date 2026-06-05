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
}

impl EnergyBreakdown {
    /// Total conservative potential energy.
    pub fn total(&self) -> f32 {
        self.edge + self.angle + self.exclusion + self.cohesion + self.gravity
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
    /// The current per-node director field (interleaved x,y,z, length `3n`), or
    /// `None` before [`init`](LayoutEngine::init). Exposed so the validation
    /// harness can compute orientational order parameters (the nematic `S`) on the
    /// live orientation field — the Phase-O observable, computed in-test for now.
    pub fn directors(&self) -> Option<&[f32]> {
        self.state.as_ref().map(|st| st.directors.as_slice())
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

/// One explicit integration step: accumulate all geometric forces, then advance
/// velocities (mass-scaled, damped) and positions.
fn step_forces(st: &mut State, s: &GeometricSettings) {
    let force = compute_forces(st, s);
    integrate(st, &force, s);
    integrate_directors(st, s);
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
    if s.angle_stiffness != 0.0 {
        accumulate_angle_forces(&mut force, st, s);
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
    let class = &st.resolved.class;
    let nc = s.class_affinity_dim as usize;
    let wc = s.well_width.max(1e-4) as f64;
    for i in 0..st.n {
        let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
        for j in (i + 1)..st.n {
            let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
            let sigma = (ri + rj).max(1e-3) as f64;
            let d = pair_dist(pos, i, j).max(1e-4) as f64;
            if d < sigma {
                exclusion += s.exclusion_strength as f64 * (sigma * (sigma / d).ln() - (sigma - d));
            }
            let eps = (pair_well_depth(s, nc, class[i] as usize, class[j] as usize)
                * orientation_factor(&st.directors, s.anisotropy_strength, i, j))
                as f64;
            if eps > 0.0 {
                if d < sigma {
                    cohesion -= eps;
                } else if d <= sigma + wc {
                    let c = (std::f64::consts::FRAC_PI_2 * (d - sigma) / wc).cos();
                    cohesion -= eps * c * c;
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
            // The attractive well's tail reaches σ + w_c, which may exceed the
            // exclusion/affinity cutoff — extend so the well is never clipped.
            // The orientation (patchy) factor scales the well depth per pair: at
            // anisotropy 0 it is 1 (isotropic ⇒ byte-identical default).
            let eps = pair_well_depth(s, nc, class[i] as usize, class[j] as usize)
                * orientation_factor(&st.directors, s.anisotropy_strength, i, j);
            let cutoff = (s.cutoff_scale * sigma).max(if eps > 0.0 { sigma + wc } else { 0.0 });

            let dx = pos[3 * j] - pos[3 * i];
            let dy = pos[3 * j + 1] - pos[3 * i + 1];
            let dz = pos[3 * j + 2] - pos[3 * i + 2];
            let dist2 = dx * dx + dy * dy + dz * dz;
            if dist2 > cutoff * cutoff {
                continue;
            }
            let dist = dist2.sqrt().max(1e-4);
            let (ux, uy, uz) = (dx / dist, dy / dist, dz / dist);

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
        }
    }
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
///
/// With both off (the default: anisotropy 0 **and/or** temperature 0) the
/// directors are untouched and never affect the forces, so default-settings
/// behaviour — and the golden master — is byte-identical.
fn integrate_directors(st: &mut State, s: &GeometricSettings) {
    let kt = s.temperature.max(0.0);
    let noise_on = kt > 0.0 && s.rotational_diffusion > 0.0;
    let align_on = s.anisotropy_strength > 0.0 && s.well_depth > 0.0;
    if !noise_on && !align_on {
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

    // --- integrate each director: align toward its field + rotational noise ---
    // Overdamped rotational step toward the field's *direction*: the aligning
    // field h is normalised to a unit target before use, so the per-step rotation
    // increment is bounded (∝ dt·μ) regardless of how many neighbours summed into
    // h — an unbounded raw `μ·h` overshoots and makes the director oscillate, the
    // head-to-head sign-flips that show up as a volatile, never-settling S.
    let mobility = s.anisotropy_strength.min(1.0); // bounded align rate
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
/// Returns `max(0, 1 + anisotropy_strength·(nᵢ·nⱼ))`: at `anisotropy_strength == 0`
/// it is identically `1` (isotropic — the byte-identical default). Otherwise pairs
/// whose directors are **aligned** (`nᵢ·nⱼ → +1`) get a deeper well (attract more)
/// and **anti-aligned** pairs a shallower/zero well, so the system's low-energy
/// state is a mutually-aligned (nematic) aggregate — the bilayer-sheet precursor.
/// Clamped at 0 so the well never *inverts* into a spurious repulsion.
fn orientation_factor(directors: &[f32], anisotropy: f32, i: usize, j: usize) -> f32 {
    if anisotropy == 0.0 {
        return 1.0;
    }
    let dot = directors[3 * i] * directors[3 * j]
        + directors[3 * i + 1] * directors[3 * j + 1]
        + directors[3 * i + 2] * directors[3 * j + 2];
    (1.0 + anisotropy * dot).max(0.0)
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
}
