use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{VaultEdge, VaultNode};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultGraph {
    pub nodes: IndexMap<String, VaultNode>,
    pub edges: Vec<VaultEdge>,
    pub num_communities: usize,
    pub num_wcc: usize,
    pub density: f64,
}

impl VaultGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: VaultNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    pub fn add_edge(&mut self, edge: VaultEdge) {
        self.edges.push(edge);
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}
