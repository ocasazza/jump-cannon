use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEdge {
    pub source: String,
    pub target: String,
}

/// Canonical identity of an edge: its endpoints.
///
/// Edges carry no attributes, so two edges with the same `(source, target)`
/// are the same edge. Used by graph-vcs for edge diff/merge keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeId {
    pub source: String,
    pub target: String,
}

impl From<&VaultEdge> for EdgeId {
    fn from(edge: &VaultEdge) -> Self {
        Self {
            source: edge.source.clone(),
            target: edge.target.clone(),
        }
    }
}

impl From<VaultEdge> for EdgeId {
    fn from(edge: VaultEdge) -> Self {
        Self::from(&edge)
    }
}

impl std::fmt::Display for EdgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}", self.source, self.target)
    }
}
