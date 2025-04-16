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
    // Add more types as needed
}

/// Node in the graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: Id,
    pub position: Option<(f64, f64)>, // Optional because layout algorithm may set it
    pub metadata: HashMap<String, MetadataValue>,
}

impl Node {
    pub fn new(id: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            position: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_position(mut self, x: f64, y: f64) -> Self {
        self.position = Some((x, y));
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Edge in the graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: Id,
    pub source: Id,
    pub target: Id,
    pub metadata: HashMap<String, MetadataValue>,
}

impl Edge {
    pub fn new(id: impl Into<Id>, source: impl Into<Id>, target: impl Into<Id>) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            metadata: HashMap::new(),
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

/// Location where layout computation should occur
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutComputeLocation {
    Frontend,
    Backend,
}

/// Base layout configuration options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseLayoutOptions {
    pub animate: bool,
    pub animation_duration: u32,
    pub fit: bool,
    pub padding: u32,
    pub compute_location: LayoutComputeLocation,
}

impl Default for BaseLayoutOptions {
    fn default() -> Self {
        Self {
            animate: true,
            animation_duration: 500,
            fit: true,
            padding: 30,
            compute_location: LayoutComputeLocation::Frontend,
        }
    }
}

/// fCoSE layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FcoseLayoutOptions {
    pub base: BaseLayoutOptions,
    pub quality: String, // "draft" or "default" or "proof"
    pub node_repulsion: f64,
    pub ideal_edge_length: f64,
    pub node_overlap: f64,
}

impl Default for FcoseLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            quality: "default".to_string(),
            node_repulsion: 4500.0,
            ideal_edge_length: 50.0,
            node_overlap: 10.0,
        }
    }
}

/// CoSE Bilkent layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoseBilkentLayoutOptions {
    pub base: BaseLayoutOptions,
    pub node_repulsion: f64,
    pub node_overlap: f64,
    pub ideal_edge_length: f64,
}

impl Default for CoseBilkentLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            node_repulsion: 4500.0,
            node_overlap: 10.0,
            ideal_edge_length: 50.0,
        }
    }
}

/// CiSE layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiseLayoutOptions {
    pub base: BaseLayoutOptions,
    pub clusters: Vec<Vec<Id>>, // Groups of nodes that should be placed together
    pub circle_spacing: f64,
    pub node_spacing: f64,
}

impl Default for CiseLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            clusters: Vec::new(),
            circle_spacing: 20.0,
            node_spacing: 10.0,
        }
    }
}

/// Concentric layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcentricLayoutOptions {
    pub base: BaseLayoutOptions,
    pub min_node_spacing: f64,
    pub concentric_by: String, // Property to use for concentric layout (e.g., "degree")
    pub level_width: f64,
}

impl Default for ConcentricLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            min_node_spacing: 10.0,
            concentric_by: "degree".to_string(),
            level_width: 100.0,
        }
    }
}

/// KLay Layered layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlayLayeredLayoutOptions {
    pub base: BaseLayoutOptions,
    pub layer_spacing: f64,          // Space between layers
    pub node_spacing: f64,           // Space between nodes in same layer
    pub node_placement: String,      // "SIMPLE", "LINEAR_SEGMENTS", "BRANDES_KOEPF"
    pub cross_minimization: String,  // "LAYER_SWEEP", "INTERACTIVE"
    pub cycle_breaking: String,      // "GREEDY", "INTERACTIVE"
    pub edge_routing: String,        // "ORTHOGONAL", "SPLINES", "POLYLINE"
    pub merge_edges: bool,           // Whether to merge parallel edges
}

impl Default for KlayLayeredLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            layer_spacing: 50.0,
            node_spacing: 20.0,
            node_placement: "BRANDES_KOEPF".to_string(),
            cross_minimization: "LAYER_SWEEP".to_string(),
            cycle_breaking: "GREEDY".to_string(),
            edge_routing: "ORTHOGONAL".to_string(),
            merge_edges: false,
        }
    }
}

/// Dagre layout options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagreLayoutOptions {
    pub base: BaseLayoutOptions,
    pub node_separation: f64,        // Horizontal separation between nodes in the same rank
    pub rank_separation: f64,        // Vertical separation between ranks
    pub rank_direction: String,      // "TB" (top-to-bottom), "BT" (bottom-to-top), "LR" (left-to-right), "RL" (right-to-left)
    pub align: String,               // "UL" (up-left), "UR" (up-right), "DL" (down-left), "DR" (down-right)
    pub acyclic: bool,               // Whether to run the acyclic algorithm to remove cycles
    pub ranker: String,              // "network-simplex", "tight-tree", "longest-path"
}

impl Default for DagreLayoutOptions {
    fn default() -> Self {
        Self {
            base: BaseLayoutOptions::default(),
            node_separation: 50.0,
            rank_separation: 50.0,
            rank_direction: "TB".to_string(),
            align: "UL".to_string(),
            acyclic: true,
            ranker: "network-simplex".to_string(),
        }
    }
}

/// Enum of all possible layout types with their options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutAlgorithm {
    Fcose(FcoseLayoutOptions),
    CoseBilkent(CoseBilkentLayoutOptions),
    Cise(CiseLayoutOptions),
    Concentric(ConcentricLayoutOptions),
    KlayLayered(KlayLayeredLayoutOptions),
    Dagre(DagreLayoutOptions),
}

impl LayoutAlgorithm {
    /// Get the base options for this layout algorithm
    pub fn base_options(&self) -> &BaseLayoutOptions {
        match self {
            Self::Fcose(options) => &options.base,
            Self::CoseBilkent(options) => &options.base,
            Self::Cise(options) => &options.base,
            Self::Concentric(options) => &options.base,
            Self::KlayLayered(options) => &options.base,
            Self::Dagre(options) => &options.base,
        }
    }
    
    /// Get mutable base options for this layout algorithm
    pub fn base_options_mut(&mut self) -> &mut BaseLayoutOptions {
        match self {
            Self::Fcose(options) => &mut options.base,
            Self::CoseBilkent(options) => &mut options.base,
            Self::Cise(options) => &mut options.base,
            Self::Concentric(options) => &mut options.base,
            Self::KlayLayered(options) => &mut options.base,
            Self::Dagre(options) => &mut options.base,
        }
    }
}

impl Default for LayoutAlgorithm {
    fn default() -> Self {
        Self::Fcose(FcoseLayoutOptions::default())
    }
}

/// Global rendering options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalRenderOptions {
    pub node_size: f64,
    pub node_color: String,
    pub edge_width: f64,
    pub edge_color: String,
    pub show_labels: bool,
    pub label_size: f64,
    pub dark_mode: bool,
}

impl Default for GlobalRenderOptions {
    fn default() -> Self {
        Self {
            node_size: 10.0,
            node_color: "#1E88E5".to_string(), // Material blue
            edge_width: 1.0,
            edge_color: "#757575".to_string(), // Material gray
            show_labels: true,
            label_size: 12.0,
            dark_mode: false,
        }
    }
}

/// Viewport state for the graph view
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
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
