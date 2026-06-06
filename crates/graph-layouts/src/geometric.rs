use serde::{Deserialize, Serialize};

fn default_edge_strength_spread() -> f32 {
    3.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum ClassLens {
    Uniform,
    DegreeBuckets,
    Louvain,
    Field(String),
    Tag(String),
    NodeType,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum CoordinationLens {
    Degree,
    Uniform(u32),
    Field(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum MassLens {
    Uniform,
    Degree,
    PageRank,
    Field(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum EdgeLengthLens {
    Uniform,
    Weight,
    EdgeType,
    /// Structural edge **strength** → spring rest length: edges embedded in a
    /// dense neighbourhood (high common-neighbour / Jaccard overlap) target a
    /// short length and keep their cluster tight, while global "shortcut" edges
    /// (low overlap) target a long length and let communities separate. This is
    /// the small-world de-hairballing lever (van Ham & van Wijk / Satuluri); the
    /// stretch factor is `LensConfig::edge_strength_spread`. See
    /// `docs/small-world-layout-research.md`.
    JaccardStrength,
    /// Same idea, using Batagelj's corrected overlap weight `T/(μ+M−T)` instead
    /// of plain Jaccard — damps the over-emphasis of edges inside tiny dense
    /// subgraphs.
    CorrectedOverlapStrength,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LensConfig {
    pub url: String,
    pub reconnect_backoff_ms: u32,
    pub use_gpu: bool,
    /// Wrap the geometric engine in the multilevel (coarsen → solve → prolong →
    /// refine) cascade for faster convergence and sharper global structure on
    /// large graphs (Walshaw/FM³/sfdp). The geometric engine becomes the inner
    /// solver driven across coarsening levels. NOTE: injected attributes
    /// (class/edge-strength) currently apply on the flat path only — the cascade
    /// runs on topology (attribute coarsening is a follow-up).
    #[serde(default)]
    pub use_multilevel: bool,

    pub class: ClassLens,
    pub coordination: CoordinationLens,
    pub mass: MassLens,
    pub edge_length: EdgeLengthLens,
    /// Stretch factor for the structural-strength edge-length lenses
    /// (`JaccardStrength` / `CorrectedOverlapStrength`): a pure shortcut edge
    /// (strength 0) targets `edge_rest_len · (1 + spread)`, a fully-embedded edge
    /// (strength 1) targets `edge_rest_len`. `0` disables the effect. Ignored by
    /// the other edge-length lenses.
    #[serde(default = "default_edge_strength_spread")]
    pub edge_strength_spread: f32,

    pub edge_stiffness: f32,
    pub angle_stiffness: f32,
    pub exclusion_strength: f32,
    pub affinity_strength: f32,
    pub gravity: f32,
    pub coordination_angles: Vec<f32>,
    pub class_radius: Vec<f32>,
    pub class_affinity: Vec<f32>,

    // --- dynamic-bond (self-assembly) knobs ---------------------------------
    // All default-OFF and `#[serde(default)]` so a LensConfig written before
    // these fields existed (and the default LensConfig) deserialize byte-clean
    // and drive the geometric engine's byte-identical default behaviour: with
    // `bonding_enabled == false` the engine never reads any of the rest. These
    // map 1:1 onto `GeometricSettings`'s P1–P3 dynamic-bond fields via
    // `graph-api`'s `attribute_resolver::resolve`. See
    // `docs/dynamic-edge-bonding-plan.md`.
    /// Master switch for the dynamic-bond self-assembly stage. Default `false`
    /// ⇒ the geometric engine is byte-identical to before this feature existed.
    #[serde(default)]
    pub bonding_enabled: bool,
    /// Bond **creation** cutoff (a class-compatible pair within this distance
    /// bonds). Default `1.0`. Only read when `bonding_enabled`.
    #[serde(default = "default_r_bond")]
    pub r_bond: f32,
    /// Bond **break** cutoff (hysteresis, `≈ 1.2–1.5 · r_bond`). Default `1.3`.
    #[serde(default = "default_r_break")]
    pub r_break: f32,
    /// Rebuild cadence — the bond stage runs every `bond_every` steps. Default `8`.
    #[serde(default = "default_bond_every")]
    pub bond_every: u32,
    /// Harmonic stiffness of the dynamic-bond spring. Default `0.3`.
    #[serde(default = "default_bond_stiffness")]
    pub bond_stiffness: f32,
    /// Uniform per-node valence cap (`0` ⇒ uncapped). `2 ⇒ chain`, `3 ⇒
    /// honeycomb sheet`, `4 ⇒ square net`. Default `0`.
    #[serde(default)]
    pub default_max_valence: u32,
    /// Target dynamic-bond angle in **degrees** (`180 ⇒ chain`, `120 ⇒
    /// honeycomb`, `90 ⇒ square net`). Default `180`.
    #[serde(default = "default_bond_angle")]
    pub default_bond_angle: f32,
    /// Rim **line-tension** — the seam force that closes an open bonded
    /// disk/sheet (requires a valence cap to define the rim). `0` ⇒ OFF.
    #[serde(default)]
    pub line_tension: f32,
    /// Spontaneous **curvature/tilt** between bonded neighbours (radians,
    /// small-angle) — the flat→tube→vesicle selector. `0` ⇒ flat. Default `0`.
    #[serde(default)]
    pub spont_curvature: f32,

    // --- membrane / thermostat knobs (needed for the validated self-assembly
    //     regimes; all default-OFF ⇒ byte-identical engine default) -----------
    /// Cooke–Deserno cohesion-well **depth** ε. `0` ⇒ OFF (no cohesion). A
    /// non-zero depth condenses a soup so dynamic bonds can form. Default `0`.
    #[serde(default)]
    pub well_depth: f32,
    /// Cohesion-well **width** (range of the attractive tail). Default `1.0`.
    #[serde(default = "default_well_width")]
    pub well_width: f32,
    /// Langevin **temperature** kT — the Brownian drive self-assembly emerges
    /// from. `0` ⇒ deterministic minimizer (the engine default). Default `0`.
    #[serde(default)]
    pub temperature: f32,
    /// Patchy-well orientation **anisotropy** — aligned directors deepen the
    /// well, driving nematic/membrane order. `0` ⇒ isotropic. Default `0`.
    #[serde(default)]
    pub anisotropy_strength: f32,
    /// Gay–Berne **side-by-side** packing bias — rewards a flat lamella over a
    /// nematic droplet. `0` ⇒ OFF. Default `0`.
    #[serde(default)]
    pub gb_side_strength: f32,
    /// Director→position **tilt coupling** — converts the director field's
    /// orientational preference into real (flat/curved) membrane geometry.
    /// `0` ⇒ OFF. Default `0`.
    #[serde(default)]
    pub tilt_coupling_strength: f32,
}

fn default_well_width() -> f32 {
    1.0
}

fn default_r_bond() -> f32 {
    1.0
}
fn default_r_break() -> f32 {
    1.3
}
fn default_bond_every() -> u32 {
    8
}
fn default_bond_stiffness() -> f32 {
    0.3
}
fn default_bond_angle() -> f32 {
    180.0
}

impl Default for LensConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8080/graph/layout/stream".to_string(),
            reconnect_backoff_ms: 1000,
            use_gpu: false,
            use_multilevel: false,
            class: ClassLens::Uniform,
            coordination: CoordinationLens::Uniform(0),
            mass: MassLens::Uniform,
            edge_length: EdgeLengthLens::Uniform,
            edge_strength_spread: 3.0,
            edge_stiffness: 0.1,
            angle_stiffness: 0.05,
            exclusion_strength: 100.0,
            affinity_strength: 0.0,
            gravity: 0.005,
            coordination_angles: vec![],
            class_radius: vec![],
            class_affinity: vec![],

            // Dynamic bonding default-OFF (byte-identical engine default).
            bonding_enabled: false,
            r_bond: 1.0,
            r_break: 1.3,
            bond_every: 8,
            bond_stiffness: 0.3,
            default_max_valence: 0,
            default_bond_angle: 180.0,
            line_tension: 0.0,
            spont_curvature: 0.0,

            // Membrane / thermostat default-OFF (byte-identical engine default).
            well_depth: 0.0,
            well_width: 1.0,
            temperature: 0.0,
            anisotropy_strength: 0.0,
            gb_side_strength: 0.0,
            tilt_coupling_strength: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lens_config_serde_roundtrip() {
        let mut config = LensConfig::default();
        config.class = ClassLens::Louvain;
        config.exclusion_strength = 1337.0;
        config.edge_length = EdgeLengthLens::JaccardStrength;
        config.edge_strength_spread = 4.5;
        config.use_multilevel = true;

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LensConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.class, ClassLens::Louvain);
        assert_eq!(decoded.exclusion_strength, 1337.0);
        assert_eq!(decoded.edge_length, EdgeLengthLens::JaccardStrength);
        assert_eq!(decoded.edge_strength_spread, 4.5);
        assert!(decoded.use_multilevel);
    }

    /// Configs written before the new fields existed must still deserialize
    /// (the strength/multilevel fields default).
    #[test]
    fn lens_config_backcompat_without_new_fields() {
        let old = serde_json::json!({
            "url": "ws://x/stream",
            "reconnect_backoff_ms": 1000,
            "use_gpu": true,
            "class": { "kind": "Louvain" },
            "coordination": { "kind": "Degree" },
            "mass": { "kind": "Uniform" },
            "edge_length": { "kind": "Uniform" },
            "edge_stiffness": 0.1,
            "angle_stiffness": 0.05,
            "exclusion_strength": 100.0,
            "affinity_strength": 0.0,
            "gravity": 0.005,
            "coordination_angles": [],
            "class_radius": [],
            "class_affinity": []
        });
        let decoded: LensConfig = serde_json::from_value(old).unwrap();
        assert!(!decoded.use_multilevel);
        assert_eq!(decoded.edge_length, EdgeLengthLens::Uniform);
        // The dynamic-bond fields (added later) must default OFF when absent so
        // a pre-bonding config keeps the byte-identical geometric default.
        assert!(!decoded.bonding_enabled);
        assert_eq!(decoded.default_max_valence, 0);
        assert_eq!(decoded.r_bond, 1.0);
        assert_eq!(decoded.r_break, 1.3);
        assert_eq!(decoded.bond_every, 8);
        assert_eq!(decoded.default_bond_angle, 180.0);
        assert_eq!(decoded.line_tension, 0.0);
        assert_eq!(decoded.spont_curvature, 0.0);
    }

    #[test]
    fn lens_config_bonding_roundtrip() {
        let mut config = LensConfig::default();
        config.bonding_enabled = true;
        config.r_bond = 1.1;
        config.r_break = 1.5;
        config.bond_every = 4;
        config.bond_stiffness = 0.4;
        config.default_max_valence = 3;
        config.default_bond_angle = 120.0;
        config.line_tension = 4.0;
        config.spont_curvature = 0.5;

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LensConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, config);
    }
}
