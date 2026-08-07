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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultNode {
    pub id: String,
    pub meta: NodeMeta,
    pub metrics: NodeMetrics,
    pub x: f32,
    pub y: f32,
}
