pub mod color;
pub mod edge;
pub mod field_schema;
pub mod graph;
pub mod node;

#[cfg(test)]
mod tests;

pub use color::PALETTE;
pub use edge::{EdgeId, VaultEdge};
pub use field_schema::{DoctypeSchema, FieldSchema, FieldType};
pub use graph::{GraphValidationError, VaultGraph};
pub use node::{NodeMeta, NodeMetrics, VaultNode};

/// Source-neutral name for the canonical graph exchanged by importers.
///
/// `VaultGraph` remains the compatibility name while the API and frontend are
/// migrated away from Obsidian terminology.
pub type Graph = VaultGraph;

/// Source-neutral name for a canonical imported node.
pub type Node = VaultNode;

/// Source-neutral name for a canonical imported edge.
pub type Edge = VaultEdge;
