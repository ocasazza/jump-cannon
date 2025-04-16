use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use shared::types::Graph;
use super::traits::{GraphStorage, StorageError};

/// In-memory implementation of the GraphStorage trait
/// This is a simple implementation that stores graphs in memory
/// It's thread-safe and can be shared between threads
pub struct InMemoryStorage {
    graphs: Arc<RwLock<HashMap<String, Graph>>>,
}

impl InMemoryStorage {
    /// Create a new in-memory storage
    pub fn new() -> Self {
        Self {
            graphs: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GraphStorage for InMemoryStorage {
    async fn get_graph(&self, id: &str) -> Result<Graph, StorageError> {
        let graphs = self.graphs.read().map_err(|e| {
            StorageError::Internal(format!("Failed to acquire read lock: {}", e))
        })?;
        
        graphs.get(id)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(id.to_string()))
    }
    
    async fn save_graph(&self, id: &str, graph: &Graph) -> Result<(), StorageError> {
        let mut graphs = self.graphs.write().map_err(|e| {
            StorageError::Internal(format!("Failed to acquire write lock: {}", e))
        })?;
        
        graphs.insert(id.to_string(), graph.clone());
        Ok(())
    }
    
    async fn delete_graph(&self, id: &str) -> Result<(), StorageError> {
        let mut graphs = self.graphs.write().map_err(|e| {
            StorageError::Internal(format!("Failed to acquire write lock: {}", e))
        })?;
        
        if graphs.remove(id).is_none() {
            return Err(StorageError::NotFound(id.to_string()));
        }
        
        Ok(())
    }
    
    async fn list_graphs(&self) -> Result<Vec<String>, StorageError> {
        let graphs = self.graphs.read().map_err(|e| {
            StorageError::Internal(format!("Failed to acquire read lock: {}", e))
        })?;
        
        Ok(graphs.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::types::{Node, Edge};
    
    #[tokio::test]
    async fn test_in_memory_storage() {
        let storage = InMemoryStorage::new();
        
        // Create a test graph
        let mut graph = Graph::new();
        let node1 = Node::new("node1").with_position(0.0, 0.0);
        let node2 = Node::new("node2").with_position(100.0, 100.0);
        let edge = Edge::new("edge1", "node1", "node2");
        
        graph.add_node(node1);
        graph.add_node(node2);
        graph.add_edge(edge);
        
        // Test save and get
        storage.save_graph("test-graph", &graph).await.unwrap();
        let retrieved = storage.get_graph("test-graph").await.unwrap();
        
        assert_eq!(retrieved.nodes.len(), 2);
        assert_eq!(retrieved.edges.len(), 1);
        
        // Test list
        let graph_ids = storage.list_graphs().await.unwrap();
        assert_eq!(graph_ids.len(), 1);
        assert_eq!(graph_ids[0], "test-graph");
        
        // Test delete
        storage.delete_graph("test-graph").await.unwrap();
        let result = storage.get_graph("test-graph").await;
        assert!(result.is_err());
    }
}
