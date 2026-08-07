use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fmt;

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

    /// Add a node without silently replacing an existing node with the same ID.
    ///
    /// Importers should prefer this method. [`Self::add_node`] remains for
    /// compatibility with code that intentionally uses last-write-wins updates.
    pub fn try_add_node(&mut self, node: VaultNode) -> Result<(), GraphValidationError> {
        if self.nodes.contains_key(&node.id) {
            return Err(GraphValidationError::DuplicateNodeId(node.id));
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
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

    /// Validate invariants required before an imported graph is published.
    pub fn validate(&self) -> Result<(), GraphValidationError> {
        for (key, node) in &self.nodes {
            if key != &node.id {
                return Err(GraphValidationError::NodeKeyMismatch {
                    key: key.clone(),
                    node_id: node.id.clone(),
                });
            }
            if !node.x.is_finite() || !node.y.is_finite() {
                return Err(GraphValidationError::NonFinitePosition(node.id.clone()));
            }
        }

        for (index, edge) in self.edges.iter().enumerate() {
            if !self.nodes.contains_key(&edge.source) || !self.nodes.contains_key(&edge.target) {
                return Err(GraphValidationError::DanglingEdge {
                    index,
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                });
            }
        }

        Ok(())
    }
}

/// A canonical graph invariant violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphValidationError {
    DuplicateNodeId(String),
    NodeKeyMismatch {
        key: String,
        node_id: String,
    },
    NonFinitePosition(String),
    DanglingEdge {
        index: usize,
        source: String,
        target: String,
    },
}

impl fmt::Display for GraphValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateNodeId(id) => write!(f, "duplicate node id: {id}"),
            Self::NodeKeyMismatch { key, node_id } => {
                write!(f, "node map key {key:?} does not match node id {node_id:?}")
            }
            Self::NonFinitePosition(id) => write!(f, "node {id:?} has a non-finite position"),
            Self::DanglingEdge {
                index,
                source,
                target,
            } => write!(
                f,
                "edge {index} has a missing endpoint: {source:?} -> {target:?}"
            ),
        }
    }
}

impl std::error::Error for GraphValidationError {}
