use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeMeta {
    pub title: String,
    pub tags: Vec<String>,
    pub frontmatter: HashMap<String, serde_json::Value>,
    pub mtime: i64,
    pub path: String,
    pub doctype: Option<String>,
    pub folder: String,
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
