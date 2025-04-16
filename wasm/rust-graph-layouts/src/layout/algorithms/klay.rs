use std::collections::HashSet;
use crate::types::{Graph, KlayLayeredLayoutOptions};
use crate::layout::traits::{LayoutEngine, LayeredLayout};

/// KLay Layered layout engine implementation
pub struct KlayLayoutEngine {
    options: KlayLayeredLayoutOptions,
}

impl KlayLayoutEngine {
    /// Create a new KLay layout engine with the given options
    pub fn new(options: KlayLayeredLayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for KlayLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Step 1: Assign nodes to layers
        let mut layers = self.assign_layers(graph)?;
        
        // Step 2: Break cycles if needed
        self.break_cycles(graph, &mut layers)?;
        
        // Step 3: Order nodes within layers to minimize crossings
        self.minimize_crossings(&mut layers, graph)?;
        
        // Step 4: Assign coordinates
        self.assign_coordinates(graph, &layers)
    }
    
    fn name(&self) -> &'static str {
        "KLay Layered"
    }
    
    fn description(&self) -> &'static str {
        "Layer-based layout algorithm optimized for directed graphs"
    }
}

impl LayeredLayout for KlayLayoutEngine {
    fn assign_layers(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        let mut layers: Vec<Vec<String>> = Vec::new();
        let mut assigned = HashSet::new();
        let mut current_layer = Vec::new();
        
        // Find root nodes (nodes with no incoming edges)
        for node_id in graph.nodes.keys() {
            let has_incoming = graph.edges.values().any(|e| e.target == *node_id);
            if !has_incoming {
                current_layer.push(node_id.clone());
                assigned.insert(node_id.clone());
            }
        }
        
        // If no root nodes found, start with any node
        if current_layer.is_empty() && !graph.nodes.is_empty() {
            let first_node = graph.nodes.keys().next().unwrap().clone();
            current_layer.push(first_node.clone());
            assigned.insert(first_node);
        }
        
        // Build layers
        while !current_layer.is_empty() {
            layers.push(current_layer.clone());
            let mut next_layer = Vec::new();
            
            for node_id in &current_layer {
                // Find all unassigned nodes that this node points to
                for edge in graph.edges.values() {
                    if edge.source == *node_id && !assigned.contains(&edge.target) {
                        next_layer.push(edge.target.clone());
                        assigned.insert(edge.target.clone());
                    }
                }
            }
            
            current_layer = next_layer;
        }
        
        // Handle any remaining nodes (disconnected or in cycles)
        for node_id in graph.nodes.keys() {
            if !assigned.contains(node_id) {
                if let Some(last_layer) = layers.last_mut() {
                    last_layer.push(node_id.clone());
                } else {
                    layers.push(vec![node_id.clone()]);
                }
            }
        }
        
        Ok(layers)
    }
    
    fn break_cycles(&self, graph: &mut Graph, layers: &mut Vec<Vec<String>>) -> Result<(), String> {
        // Find edges that point to nodes in previous layers
        let edges_to_reverse: Vec<String> = graph.edges.values()
            .filter(|edge| {
                let source_layer = layers.iter().position(|layer| layer.contains(&edge.source));
                let target_layer = layers.iter().position(|layer| layer.contains(&edge.target));
                
                if let (Some(sl), Some(tl)) = (source_layer, target_layer) {
                    sl > tl // Edge points backwards
                } else {
                    false
                }
            })
            .map(|edge| edge.id.clone())
            .collect();
        
        // Reverse the identified edges
        for edge_id in edges_to_reverse {
            if let Some(edge) = graph.edges.get_mut(&edge_id) {
                std::mem::swap(&mut edge.source, &mut edge.target);
            }
        }
        
        Ok(())
    }
    
    fn minimize_crossings(&self, layers: &mut Vec<Vec<String>>, graph: &Graph) -> Result<(), String> {
        // For each pair of adjacent layers
        for i in 0..layers.len().saturating_sub(1) {
            let mut improved = true;
            
            // Keep trying to improve until no more improvements can be made
            while improved {
                improved = false;
                
                // Clone the current layer for comparison
                let current_layer = layers[i].clone();
                
                // Get mutable reference to the next layer
                let next_layer = &mut layers[i + 1];
                
                // Count crossings between current positions
                let mut best_crossings = self.count_crossings(&current_layer, next_layer, graph);
                
                // Try swapping adjacent nodes in the next layer
                for j in 0..next_layer.len().saturating_sub(1) {
                    next_layer.swap(j, j + 1);
                    
                    let new_crossings = self.count_crossings(&current_layer, next_layer, graph);
                    if new_crossings < best_crossings {
                        best_crossings = new_crossings;
                        improved = true;
                    } else {
                        // Swap back if no improvement
                        next_layer.swap(j, j + 1);
                    }
                }
            }
        }
        
        Ok(())
    }
    
    fn count_crossings(&self, layer1: &[String], layer2: &[String], graph: &Graph) -> usize {
        let mut crossings = 0;
        
        // For each pair of edges between the layers
        for (i1, n1) in layer1.iter().enumerate() {
            for (i2, n2) in layer1.iter().enumerate().skip(i1 + 1) {
                for edge1 in graph.edges.values() {
                    if edge1.source != *n1 { continue; }
                    
                    for edge2 in graph.edges.values() {
                        if edge2.source != *n2 { continue; }
                        
                        let j1 = layer2.iter().position(|n| *n == edge1.target);
                        let j2 = layer2.iter().position(|n| *n == edge2.target);
                        
                        if let (Some(j1), Some(j2)) = (j1, j2) {
                            // Check if edges cross
                            if (i1 < i2 && j1 > j2) || (i1 > i2 && j1 < j2) {
                                crossings += 1;
                            }
                        }
                    }
                }
            }
        }
        
        crossings
    }
}

impl KlayLayoutEngine {
    fn assign_coordinates(&self, graph: &mut Graph, layers: &[Vec<String>]) -> Result<(), String> {
        let layer_height = self.options.layer_spacing;
        let node_spacing = self.options.node_spacing;
        
        // Assign y-coordinates based on layer
        for (layer_idx, layer) in layers.iter().enumerate() {
            let y = layer_idx as f64 * layer_height;
            
            // Assign x-coordinates within layer
            let layer_width = (layer.len() - 1) as f64 * node_spacing;
            let start_x = -layer_width / 2.0;
            
            for (node_idx, node_id) in layer.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(node_id) {
                    let x = start_x + node_idx as f64 * node_spacing;
                    node.position = Some((x, y));
                }
            }
        }
        
        Ok(())
    }
}

/// Public interface for applying the KLay layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &KlayLayeredLayoutOptions) -> Result<(), String> {
    let engine = KlayLayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Node, Edge};

    #[test]
    fn test_simple_chain() {
        let mut graph = Graph::new();
        
        let node_a = Node::new("A");
        let node_b = Node::new("B");
        let node_c = Node::new("C");
        
        graph.add_node(node_a)
             .add_node(node_b)
             .add_node(node_c);
        
        let edge1 = Edge::new("e1", "A", "B");
        let edge2 = Edge::new("e2", "B", "C");
        
        graph.add_edge(edge1)
             .add_edge(edge2);
        
        let engine = KlayLayoutEngine::new(KlayLayeredLayoutOptions::default());
        engine.apply_layout(&mut graph).unwrap();
        
        let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
        let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
        let c_pos = graph.nodes.get("C").unwrap().position.unwrap();
        
        assert!(a_pos.1 < b_pos.1);
        assert!(b_pos.1 < c_pos.1);
    }

    #[test]
    fn test_cycle_breaking() {
        let mut graph = Graph::new();
        
        let node_a = Node::new("A");
        let node_b = Node::new("B");
        
        graph.add_node(node_a)
             .add_node(node_b);
        
        let edge1 = Edge::new("e1", "A", "B");
        let edge2 = Edge::new("e2", "B", "A");
        
        graph.add_edge(edge1)
             .add_edge(edge2);
        
        let engine = KlayLayoutEngine::new(KlayLayeredLayoutOptions::default());
        let mut layers = engine.assign_layers(&graph).unwrap();
        engine.break_cycles(&mut graph, &mut layers).unwrap();
        
        let mut forward_count = 0;
        let mut backward_count = 0;
        
        for edge in graph.edges.values() {
            if edge.source == "A" && edge.target == "B" {
                forward_count += 1;
            } else if edge.source == "B" && edge.target == "A" {
                backward_count += 1;
            }
        }
        
        assert_eq!(forward_count + backward_count, 2);
        assert!(forward_count == 2 || backward_count == 2);
    }
}
