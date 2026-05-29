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
    /// constant in the `O(n²)` pair scan.
    pub cutoff_scale: f32,

    // --- mass / gravity / integration ---
    /// Lower bound of the normalized mass range (structural mass sources).
    pub mass_min: f32,
    /// Upper bound of the normalized mass range (structural mass sources).
    pub mass_max: f32,
    /// Linear pull toward the origin, scaled by node mass. Keeps the layout
    /// compact and lets heavy nodes settle central.
    pub gravity: f32,
    /// Velocity damping per step in `[0,1]` (1 = frictionless, 0 = fully
    /// damped). Keeps the explicit integrator stable.
    pub damping: f32,
    /// Integration time step.
    pub time_step: f32,
    /// Hard cap on per-step displacement magnitude per node (after integration),
    /// a stability guard against transient large forces. `0` ⇒ uncapped.
    pub max_step: f32,
}

impl Default for GeometricSettings {
    fn default() -> Self {
        Self {
            class_source: ClassSource::default(),
            coordination_source: CoordinationSource::default(),
            mass_source: MassSource::default(),
            edge_length_source: EdgeLengthSource::default(),

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

            mass_min: 1.0,
            mass_max: 1.0,
            gravity: 0.02,
            damping: 0.9,
            time_step: 1.0,
            max_step: 10.0,
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
struct Resolved {
    /// Per-node class id (already mapped into the class/affinity tables' domain
    /// by lookup-time fallback, so this may exceed the table sizes).
    class: Vec<u32>,
    /// Per-node coordination id (clamped to the angle table at lookup).
    coordination: Vec<u32>,
    /// Per-node mass (> 0).
    mass: Vec<f32>,
    /// Unique undirected edges (a < b) with target lengths.
    edges: Vec<ResolvedEdge>,
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
    fn resolve(
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
            ClassSource::Community { passes } => {
                label_propagation(graph, *passes)
            }
            ClassSource::Injected => attrs
                .and_then(|a| a.node_class.clone())
                .ok_or_else(|| {
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
            MassSource::Injected => attrs
                .and_then(|a| a.node_mass.clone())
                .ok_or_else(|| {
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
            EdgeLengthSource::Injected => Some(
                attrs
                    .and_then(|a| a.edge_len.clone())
                    .ok_or_else(|| {
                        "edge_length_source = injected but GraphAttributes.edge_len is absent"
                            .to_string()
                    })?,
            ),
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
        self.state = Some(State {
            n,
            positions: positions.to_vec(),
            velocities: vec![0.0f32; 3 * n],
            graph: g.clone(),
            resolved,
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
    let n = st.n;
    let mut force = vec![0.0f32; 3 * n];

    accumulate_edge_forces(&mut force, &st.positions, &st.resolved.edges, s.edge_stiffness);
    if s.angle_stiffness != 0.0 {
        accumulate_angle_forces(&mut force, st, s);
    }
    accumulate_exclusion_affinity(&mut force, st, s);
    accumulate_gravity(&mut force, &st.positions, &st.resolved.mass, s.gravity);

    integrate(st, &force, s);
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
    let g = &st.graph;
    let pos = &st.positions;
    let k = s.angle_stiffness;
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
        let ideal = lookup_angle(&s.coordination_angles, deg_id);
        let ideal_rad = ideal.to_radians();

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
                apply_angle_pair(force, pos, c, j, kn, ideal_rad, k);
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

    for i in 0..n {
        let ri = lookup_radius(&s.class_radius, class[i] as usize, s.default_radius);
        for j in (i + 1)..n {
            let rj = lookup_radius(&s.class_radius, class[j] as usize, s.default_radius);
            let sigma = (ri + rj).max(1e-3);
            let cutoff = s.cutoff_scale * sigma;

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

            // net_toward_j applied to i (and the negative to j).
            let net = attract - repel;
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
fn integrate(st: &mut State, force: &[f32], s: &GeometricSettings) {
    let dt = s.time_step;
    let damping = s.damping.clamp(0.0, 1.0);
    let max_step = s.max_step;
    let n = st.n;
    for i in 0..n {
        let m = st.resolved.mass[i];
        for d in 0..3 {
            let idx = 3 * i + d;
            let a = force[idx] / m;
            let mut v = (st.velocities[idx] + dt * a) * damping;
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

// ---------------------------------------------------------------------------
// Resolvers + lookups (structural attribute computation)
// ---------------------------------------------------------------------------

/// CSR out-degree per node (= number of stored neighbours).
fn compute_degree(g: &CsrGraph) -> Vec<u32> {
    let n = g.n_nodes as usize;
    (0..n)
        .map(|v| g.offsets[v + 1] - g.offsets[v])
        .collect()
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
fn build_unique_edges(g: &CsrGraph, rest_len: f32, injected_len: Option<&[f32]>) -> Vec<ResolvedEdge> {
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
        let neighbors = vec![1, 2, /*0*/ 0, 2, /*1*/ 0, 1, 3, /*2*/ 2, 4, 5, /*3*/ 3, 5, /*4*/ 3, 4 /*5*/];
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
        assert!((1..=3).contains(&k), "unexpected community count {k}: {comm:?}");
    }

    #[test]
    fn pagerank_ranks_hub_highest() {
        // Star: node 0 connected to 1,2,3,4 (undirected).
        let neighbors = vec![1, 2, 3, 4, /*0*/ 0, /*1*/ 0, /*2*/ 0, /*3*/ 0 /*4*/];
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
                    let cos = ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb))
                        .clamp(-1.0, 1.0);
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
