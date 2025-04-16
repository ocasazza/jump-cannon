use crate::types::{Graph, CiseLayoutOptions};
use crate::layout::traits::{LayoutEngine, CircularLayout};

pub struct CiseLayoutEngine {
    options: CiseLayoutOptions,
}

impl CiseLayoutEngine {
    pub fn new(options: CiseLayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for CiseLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Step 1: Arrange nodes in clusters on circles
        self.arrange_clusters(graph)?;
        
        // Step 2: Optimize node ordering to minimize edge crossings
        self.optimize_ordering(graph)?;
        
        Ok(())
    }
    
    fn name(&self) -> &'static str {
        "CiSE"
    }
    
    fn description(&self) -> &'static str {
        "Circular Spring Embedder layout algorithm"
    }
}

impl CircularLayout for CiseLayoutEngine {
    fn arrange_circle(&self, graph: &mut Graph, radius: f64) -> Result<(), String> {
        let node_count = graph.nodes.len();
        if node_count == 0 {
            return Ok(());
        }
        
        let angle_step = 2.0 * std::f64::consts::PI / node_count as f64;
        
        for (i, node) in graph.nodes.values_mut().enumerate() {
            let angle = angle_step * i as f64;
            let x = radius * angle.cos();
            let y = radius * angle.sin();
            node.position = Some((x, y));
        }
        
        Ok(())
    }
    
    fn optimize_ordering(&self, graph: &mut Graph) -> Result<(), String> {
        // This is a simplified implementation
        // A full implementation would use a more sophisticated algorithm
        // to minimize edge crossings
        
        // For now, we'll just sort nodes by their degree
        let mut node_degrees: Vec<(String, usize)> = graph.nodes.keys()
            .map(|id| {
                let degree = graph.edges.values()
                    .filter(|e| e.source == *id || e.target == *id)
                    .count();
                (id.clone(), degree)
            })
            .collect();
        
        node_degrees.sort_by_key(|(_, degree)| *degree);
        
        // Rearrange nodes in a circle based on the sorted order
        let node_count = node_degrees.len();
        if node_count == 0 {
            return Ok(());
        }
        
        let angle_step = 2.0 * std::f64::consts::PI / node_count as f64;
        let radius = 100.0; // Default radius
        
        for (i, (id, _)) in node_degrees.iter().enumerate() {
            if let Some(node) = graph.nodes.get_mut(id) {
                let angle = angle_step * i as f64;
                let x = radius * angle.cos();
                let y = radius * angle.sin();
                node.position = Some((x, y));
            }
        }
        
        Ok(())
    }
}

impl CiseLayoutEngine {
    /// Arrange nodes in clusters on circles
    fn arrange_clusters(&self, graph: &mut Graph) -> Result<(), String> {
        // If no clusters are defined, arrange all nodes in a single circle
        if self.options.clusters.is_empty() {
            return self.arrange_circle(graph, 100.0);
        }
        
        // Arrange each cluster in its own circle
        let cluster_count = self.options.clusters.len();
        let cluster_radius = 100.0;
        let circle_spacing = self.options.circle_spacing;
        
        // Calculate positions for cluster centers
        let outer_radius = cluster_radius * 2.0 + circle_spacing;
        let angle_step = 2.0 * std::f64::consts::PI / cluster_count as f64;
        
        for (cluster_idx, cluster) in self.options.clusters.iter().enumerate() {
            // Skip empty clusters
            if cluster.is_empty() {
                continue;
            }
            
            // Calculate cluster center
            let angle = angle_step * cluster_idx as f64;
            let center_x = outer_radius * angle.cos();
            let center_y = outer_radius * angle.sin();
            
            // Arrange nodes in this cluster
            let node_count = cluster.len();
            let inner_angle_step = 2.0 * std::f64::consts::PI / node_count as f64;
            
            for (node_idx, node_id) in cluster.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(node_id) {
                    let inner_angle = inner_angle_step * node_idx as f64;
                    let x = center_x + cluster_radius * inner_angle.cos();
                    let y = center_y + cluster_radius * inner_angle.sin();
                    node.position = Some((x, y));
                }
            }
        }
        
        // Handle nodes not in any cluster
        let unclustered = graph.nodes.keys()
            .filter(|id| !self.options.clusters.iter().any(|cluster| cluster.contains(id)))
            .cloned()
            .collect::<Vec<_>>();
        
        if !unclustered.is_empty() {
            let unclustered_count = unclustered.len();
            let unclustered_angle_step = 2.0 * std::f64::consts::PI / unclustered_count as f64;
            
            for (idx, id) in unclustered.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(id) {
                    let angle = unclustered_angle_step * idx as f64;
                    let x = (outer_radius + cluster_radius) * angle.cos();
                    let y = (outer_radius + cluster_radius) * angle.sin();
                    node.position = Some((x, y));
                }
            }
        }
        
        Ok(())
    }
}

/// Public interface for applying the CiSE layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &CiseLayoutOptions) -> Result<(), String> {
    let engine = CiseLayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Node, Edge};

    #[test]
    fn test_circular_arrangement() {
        let mut graph = Graph::new();
        
        // Create a simple circular graph
        for i in 0..4 {
            let node = Node::new(format!("node{}", i));
            graph.add_node(node);
        }
        
        // Connect nodes in a circle
        for i in 0..4 {
            let edge = Edge::new(
                format!("edge{}", i),
                format!("node{}", i),
                format!("node{}", (i + 1) % 4),
            );
            graph.add_edge(edge);
        }
        
        let engine = CiseLayoutEngine::new(CiseLayoutOptions::default());
        engine.arrange_circle(&mut graph, 100.0).unwrap();
        
        // Verify all nodes have positions
        for node in graph.nodes.values() {
            assert!(node.position.is_some());
        }
    }
}
