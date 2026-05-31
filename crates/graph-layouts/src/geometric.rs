use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LensConfig {
    pub url: String,
    pub reconnect_backoff_ms: u32,
    pub use_gpu: bool,
    
    pub class: ClassLens,
    pub coordination: CoordinationLens,
    pub mass: MassLens,
    pub edge_length: EdgeLengthLens,

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
            class: ClassLens::Uniform,
            coordination: CoordinationLens::Uniform(0),
            mass: MassLens::Uniform,
            edge_length: EdgeLengthLens::Uniform,
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

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LensConfig = serde_json::from_str(&json).unwrap();
        
        assert_eq!(decoded.class, ClassLens::Louvain);
        assert_eq!(decoded.exclusion_strength, 1337.0);
    }
}
