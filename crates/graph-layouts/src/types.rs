use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for nodes and edges
pub type Id = String;

/// Key-value pair for metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

/// Node in the graph
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Node {
    pub id: Id,
    pub position: Option<(f64, f64)>,
    /// 3D position for the GPU force backend. None until the GPU sim writes it.
    /// Existing 2D layouts ignore this field.
    #[serde(default)]
    pub position3: Option<[f32; 3]>,
    #[serde(default)]
    pub metadata: HashMap<String, MetadataValue>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(rename = "x", default)]
    pub pos_x: f64,
    #[serde(rename = "y", default)]
    pub pos_y: f64,
}

impl Node {
    pub fn new(id: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            position: None,
            position3: None,
            metadata: HashMap::new(),
            label: String::new(),
            r#type: String::new(),
            pos_x: 0.0,
            pos_y: 0.0,
        }
    }

    pub fn with_position(mut self, x: f64, y: f64) -> Self {
        self.position = Some((x, y));
        self.pos_x = x;
        self.pos_y = y;
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Edge in the graph
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Edge {
    #[serde(default = "generate_edge_id")]
    pub id: Id,
    pub source: Id,
    pub target: Id,
    #[serde(default)]
    pub metadata: HashMap<String, MetadataValue>,
    #[serde(default)]
    pub r#type: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

fn generate_edge_id() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    format!("e{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

impl Edge {
    pub fn new(id: impl Into<Id>, source: impl Into<Id>, target: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            metadata: HashMap::new(),
            r#type: String::new(),
            weight: 1.0,
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Complete graph structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Graph {
    pub nodes: HashMap<Id, Node>,
    pub edges: HashMap<Id, Edge>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, node: Node) -> &mut Self {
        self.nodes.insert(node.id.clone(), node);
        self
    }

    pub fn add_edge(&mut self, edge: Edge) -> &mut Self {
        self.edges.insert(edge.id.clone(), edge);
        self
    }

    pub fn remove_node(&mut self, id: &Id) -> Option<Node> {
        // Also remove any edges connected to this node
        let edges_to_remove: Vec<Id> = self.edges.values()
            .filter(|e| e.source == *id || e.target == *id)
            .map(|e| e.id.clone())
            .collect();
        
        for edge_id in edges_to_remove {
            self.edges.remove(&edge_id);
        }
        
        self.nodes.remove(id)
    }

    pub fn remove_edge(&mut self, id: &Id) -> Option<Edge> {
        self.edges.remove(id)
    }
}

/// Helper struct for deserializing graph JSON files
#[derive(Debug, Deserialize)]
pub struct GraphFile {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl From<GraphFile> for Graph {
    fn from(file: GraphFile) -> Self {
        let mut graph = Graph::new();
        
        // Convert nodes array to HashMap
        for mut node in file.nodes {
            // Update position from x,y coordinates if present
            if node.position.is_none() && (node.pos_x != 0.0 || node.pos_y != 0.0) {
                node.position = Some((node.pos_x, node.pos_y));
            }
            graph.nodes.insert(node.id.clone(), node);
        }
        
        // Convert edges array to HashMap
        for edge in file.edges {
            graph.edges.insert(edge.id.clone(), edge);
        }
        
        graph
    }
}

/// Base layout configuration options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutOptions {
    pub padding: u32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            padding: 30,
        }
    }
}

// Implement From traits for MetadataValue
impl From<String> for MetadataValue {
    fn from(value: String) -> Self {
        MetadataValue::String(value)
    }
}

impl From<&str> for MetadataValue {
    fn from(value: &str) -> Self {
        MetadataValue::String(value.to_string())
    }
}

impl From<f64> for MetadataValue {
    fn from(value: f64) -> Self {
        MetadataValue::Number(value)
    }
}

impl From<i32> for MetadataValue {
    fn from(value: i32) -> Self {
        MetadataValue::Number(value as f64)
    }
}

impl From<bool> for MetadataValue {
    fn from(value: bool) -> Self {
        MetadataValue::Boolean(value)
    }
}

// Per-algorithm layout settings live with each algorithm in
// `layout::algorithms::*` (the System-B `StaticLayout`/`PhysicsLayout`
// pattern). The legacy `*Options` structs and the `LayoutAlgorithm` enum that
// fed the removed `layout::apply_layout` dispatcher have been retired.
