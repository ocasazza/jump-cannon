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
    }
}
