use async_trait::async_trait;
use shared::types::Graph;
use thiserror::Error;

/// Storage errors
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Graph not found: {0}")]
    NotFound(String),
    
    #[error("Storage error: {0}")]
    Internal(String),
    
    #[error("Invalid data: {0}")]
    InvalidData(String),
}

/// Graph storage trait
/// This trait defines the interface for storing and retrieving graphs
/// It's designed to be implemented by different storage backends
#[async_trait]
pub trait GraphStorage: Send + Sync {
    /// Get a graph by ID
    async fn get_graph(&self, id: &str) -> Result<Graph, StorageError>;
    
    /// Save a graph
    async fn save_graph(&self, id: &str, graph: &Graph) -> Result<(), StorageError>;
    
    /// Delete a graph
    async fn delete_graph(&self, id: &str) -> Result<(), StorageError>;
    
    /// List all available graphs
    async fn list_graphs(&self) -> Result<Vec<String>, StorageError>;
}
