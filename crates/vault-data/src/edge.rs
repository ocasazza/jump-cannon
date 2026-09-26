use serde::{Deserialize, Serialize};

/// A directed edge between two canonical node ids.
///
/// `kind` is an optional attribute — the importer-declared edge type (a key
/// of the importer schema's `edge_types`) for sources that distinguish their
/// relations, `None` for sources that do not. Presentation of a kind belongs
/// to the client; importers only carry the vocabulary through. Identity is
/// the endpoints alone, see [`EdgeId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEdge {
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl VaultEdge {
    /// An untyped edge between two node ids.
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            kind: None,
        }
    }
}

/// Canonical identity of an edge: its endpoints.
///
/// Two edges with the same `(source, target)` are the same edge regardless
/// of `kind`, which is an attribute rather than identity. Used by graph-vcs
/// for edge diff/merge keys.
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
