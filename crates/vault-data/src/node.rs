use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeMeta {
    /// Stable importer/source instance which produced this node.
    #[serde(default)]
    pub source_id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub frontmatter: HashMap<String, serde_json::Value>,
    pub mtime: i64,
    pub path: String,
    pub doctype: Option<String>,
    pub folder: String,
    /// MIME type for source-backed content, when the active importer exposes it.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Whether graph-api can resolve content for this node through its source.
    #[serde(default)]
    pub content_readable: bool,
    /// Whether graph-api can persist content changes through its source.
    #[serde(default)]
    pub content_writable: bool,
}

/// Native NodeMeta fields every node must carry. Rust producers go through
/// [`NodeMeta::new`], whose arguments are exactly these fields; dynamic
/// sources (packages, wire formats) are checked against this list at import
/// by [`NodeMeta::missing_required`]. The two must stay in sync — the test
/// `constructor_covers_required_fields` pins them together.
pub const REQUIRED_META_FIELDS: &[&str] = &["source_id", "title"];

impl NodeMeta {
    /// Production constructor: the required fields are arguments, so a Rust
    /// producer cannot compile without supplying them. Every other field
    /// keeps its default. Fixtures that genuinely have no identity should
    /// say so explicitly rather than relying on `Default`.
    pub fn new(source_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    /// Required fields (per [`REQUIRED_META_FIELDS`]) that are empty on this
    /// node. Import validation rejects a non-empty result.
    pub fn missing_required(&self) -> Vec<&'static str> {
        REQUIRED_META_FIELDS
            .iter()
            .filter(|field| match **field {
                "source_id" => self.source_id.trim().is_empty(),
                "title" => self.title.trim().is_empty(),
                _ => false,
            })
            .copied()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeMetrics {
    pub degree: usize,
    pub indegree: usize,
    pub outdegree: usize,
    pub pagerank: f64,
    pub betweenness: f64,
    pub kcore: usize,
    pub community: usize,
    pub wcc: usize,
    /// Phase 0: energy decomposition. Total system energy attributed to this
    /// node (attractive + repulsive). `None` when no energy pass has run.
    pub energy: Option<f32>,
    /// Phase 0: attractive component of energy decomposition.
    pub energy_attractive: Option<f32>,
    /// Phase 0: repulsive component of energy decomposition.
    pub energy_repulsive: Option<f32>,
    /// Phase 0: frame-to-frame energy variance (stability metric).
    pub stability_variance: Option<f32>,
    /// Phase 0: maximum position drift between successive frames.
    pub stability_drift: Option<f32>,
    /// Phase 0: edge-anomaly flag. True when this node participates in
    /// at least one high-stress edge.
    pub anomaly_flag: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultNode {
    pub id: String,
    pub meta: NodeMeta,
    pub metrics: NodeMetrics,
    pub x: f32,
    pub y: f32,
}
