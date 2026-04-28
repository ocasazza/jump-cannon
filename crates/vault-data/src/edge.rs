use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEdge {
    pub source: String,
    pub target: String,
}
