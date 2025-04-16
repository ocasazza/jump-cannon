use std::collections::{HashMap, HashSet};
use crate::types::{Graph, DagreLayoutOptions};
use crate::layout::traits::{LayoutEngine, LayeredLayout};

/// Dagre layout engine implementation
pub struct DagreLayoutEngine {
    options: DagreLayoutOptions,
}

impl DagreLayoutEngine {
    /// Create a new Dagre layout engine with the given options
    pub fn new(options: DagreLayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for DagreLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Step 1: Assign nodes to ranks (layers)
        let mut layers = self.assign_layers(graph)?;
        
        // Step 2: Break cycles if needed (if acyclic option is enabled)
        if self.options.acyclic {
            self.break_cycles(graph, &mut layers)?;
        }
        
        // Step 3: Order nodes within ranks to minimize crossings
        self.minimize_crossings(&mut layers, graph)?;
        
        // Step 4: Assign coordinates based on rank and position
        self.assign_coordinates(graph, &layers)
    }
    
    fn name(&self) -> &'static str {
        "Dagre"
    }
    
    fn description(&self) -> &'static str {
        "Directed graph layout algorithm optimized for hierarchical visualizations"
    }
}

impl LayeredLayout for DagreLayoutEngine {
    fn assign_layers(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        match self.options.ranker.as_str() {
            "network-simplex" => self.network_simplex_ranking(graph),
            "tight-tree" => self.tight_tree_ranking(graph),
            "longest-path" => self.longest_path_ranking(graph),
            _ => self.longest_path_ranking(graph), // Default to longest-path if unknown
        }
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

impl DagreLayoutEngine {
    /// Assign coordinates to nodes based on their layer and position
    fn assign_coordinates(&self, graph: &mut Graph, layers: &[Vec<String>]) -> Result<(), String> {
        let is_horizontal = self.options.rank_direction == "LR" || self.options.rank_direction == "RL";
        let is_reversed = self.options.rank_direction == "BT" || self.options.rank_direction == "RL";
        
        let rank_separation = self.options.rank_separation;
        let node_separation = self.options.node_separation;
        
        // Assign coordinates based on rank direction
        for (layer_idx, layer) in layers.iter().enumerate() {
        let layer_pos = if is_reversed && layers.len() > 0 {
            // Ensure we don't underflow when calculating the reversed position
            if layer_idx < layers.len() {
                let reversed_idx = layers.len() - 1 - layer_idx;
                reversed_idx as f64 * rank_separation
            } else {
                0.0 // Default position if layer_idx is out of bounds
            }
        } else {
            layer_idx as f64 * rank_separation
        };
            
            // Assign positions within layer
            let layer_width = if layer.len() > 0 {
                (layer.len() - 1) as f64 * node_separation
            } else {
                0.0
            };
            let start_pos = -layer_width / 2.0;
            
            for (node_idx, node_id) in layer.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(node_id) {
                    let node_pos = start_pos + node_idx as f64 * node_separation;
                    
                    // Set position based on rank direction
                    if is_horizontal {
                        node.position = Some((layer_pos, node_pos));
                    } else {
                        node.position = Some((node_pos, layer_pos));
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Longest path ranking algorithm
    fn longest_path_ranking(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        let mut layers: Vec<Vec<String>> = Vec::new();
        let mut assigned = HashSet::new();
        
        // Find root nodes (nodes with no incoming edges)
        let mut roots: Vec<String> = graph.nodes.keys()
            .filter(|node_id| !graph.edges.values().any(|e| e.target == **node_id))
            .cloned()
            .collect();
        
        // If no root nodes found, start with any node
        if roots.is_empty() && !graph.nodes.is_empty() {
            roots.push(graph.nodes.keys().next().unwrap().clone());
        }
        
        // Assign initial nodes to layer 0
        layers.push(roots.clone());
        for root in &roots {
            assigned.insert(root.clone());
        }
        
        // Build subsequent layers
        let mut current_layer = 0;
        while current_layer < layers.len() {
            let mut next_layer = Vec::new();
            
            for node_id in &layers[current_layer] {
                // Find all unassigned nodes that this node points to
                for edge in graph.edges.values() {
                    if edge.source == *node_id && !assigned.contains(&edge.target) {
                        next_layer.push(edge.target.clone());
                        assigned.insert(edge.target.clone());
                    }
                }
            }
            
            if !next_layer.is_empty() {
                layers.push(next_layer);
            }
            
            current_layer += 1;
        }
        
        // Handle any remaining nodes (disconnected or in cycles)
        let remaining: Vec<String> = graph.nodes.keys()
            .filter(|node_id| !assigned.contains(*node_id))
            .cloned()
            .collect();
        
        if !remaining.is_empty() {
            layers.push(remaining);
        }
        
        Ok(layers)
    }
    
    /// Network simplex ranking algorithm (simplified version)
    fn network_simplex_ranking(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        // For simplicity, we'll use a modified longest path algorithm
        // A full network simplex implementation would be more complex
        
        // First, get initial ranking using longest path
        let mut layers = self.longest_path_ranking(graph)?;
        
        // Then, try to optimize the ranking to minimize edge lengths
        self.optimize_ranking(&mut layers, graph)?;
        
        Ok(layers)
    }
    
    /// Tight tree ranking algorithm
    fn tight_tree_ranking(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        // Similar to longest path but with tighter constraints
        let mut layers = self.longest_path_ranking(graph)?;
        
        // Try to make the tree more compact
        self.compact_layers(&mut layers, graph)?;
        
        Ok(layers)
    }
    
    /// Optimize node ranking to minimize edge lengths
    fn optimize_ranking(&self, layers: &mut Vec<Vec<String>>, graph: &Graph) -> Result<(), String> {
        // Create a map of node to layer
        let mut node_to_layer = HashMap::new();
        for (layer_idx, layer) in layers.iter().enumerate() {
            for node_id in layer {
                node_to_layer.insert(node_id.clone(), layer_idx);
            }
        }
        
        // Try to move nodes to minimize edge lengths
        let mut improved = true;
        while improved {
            improved = false;
            
            for layer_idx in 0..layers.len() {
                let mut i = 0;
                while i < layers[layer_idx].len() {
                    let node_id = &layers[layer_idx][i];
                    
                    // Calculate current edge length sum
                    let mut current_sum: usize = 0;
                    for edge in graph.edges.values() {
                        if edge.source == *node_id || edge.target == *node_id {
                            let other_node = if edge.source == *node_id { &edge.target } else { &edge.source };
                            if let Some(other_layer) = node_to_layer.get(other_node) {
                                // Safely calculate the absolute difference to avoid overflow
                                let diff = if layer_idx > *other_layer {
                                    layer_idx - *other_layer
                                } else {
                                    *other_layer - layer_idx
                                };
                                current_sum = current_sum.saturating_add(diff);
                            }
                        }
                    }
                    
                    // Try moving to adjacent layers
                    for new_layer_idx in [layer_idx.saturating_sub(1), layer_idx + 1] {
                        if new_layer_idx >= layers.len() {
                            continue;
                        }
                        
                        // Calculate new edge length sum if moved
                        let mut new_sum: usize = 0;
                        for edge in graph.edges.values() {
                            if edge.source == *node_id || edge.target == *node_id {
                                let other_node = if edge.source == *node_id { &edge.target } else { &edge.source };
                                if let Some(other_layer) = node_to_layer.get(other_node) {
                                    // Safely calculate the absolute difference to avoid overflow
                                    let diff = if new_layer_idx > *other_layer {
                                        new_layer_idx - *other_layer
                                    } else {
                                        *other_layer - new_layer_idx
                                    };
                                    new_sum = new_sum.saturating_add(diff);
                                }
                            }
                        }
                        
                        // If moving improves the sum, do it
                        if new_sum < current_sum {
                            let node = layers[layer_idx].remove(i);
                            layers[new_layer_idx].push(node.clone());
                            node_to_layer.insert(node, new_layer_idx);
                            improved = true;
                            
                            // Adjust index only if i > 0 to avoid underflow
                            if i > 0 {
                                i -= 1;
                            }
                            break;
                        }
                    }
                    
                    i += 1;
                }
            }
        }
        
        Ok(())
    }
    
    /// Make layers more compact
    fn compact_layers(&self, layers: &mut Vec<Vec<String>>, _graph: &Graph) -> Result<(), String> {
        // Remove empty layers
        layers.retain(|layer| !layer.is_empty());
        
        Ok(())
    }
}

/// Public interface for applying the Dagre layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &DagreLayoutOptions) -> Result<(), String> {
    let engine = DagreLayoutEngine::new(options.clone());
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
        
        let engine = DagreLayoutEngine::new(DagreLayoutOptions::default());
        engine.apply_layout(&mut graph).unwrap();
        
        // For top-to-bottom layout, y-coordinates should increase
        let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
        let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
        let c_pos = graph.nodes.get("C").unwrap().position.unwrap();
        
        assert!(a_pos.1 < b_pos.1);
        assert!(b_pos.1 < c_pos.1);
    }

    #[test]
    fn test_left_to_right_direction() {
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
        
        // Create options with left-to-right direction
        let mut options = DagreLayoutOptions::default();
        options.rank_direction = "LR".to_string();
        
        let engine = DagreLayoutEngine::new(options);
        engine.apply_layout(&mut graph).unwrap();
        
        // For left-to-right layout, x-coordinates should increase
        let a_pos = graph.nodes.get("A").unwrap().position.unwrap();
        let b_pos = graph.nodes.get("B").unwrap().position.unwrap();
        let c_pos = graph.nodes.get("C").unwrap().position.unwrap();
        
        assert!(a_pos.0 < b_pos.0);
        assert!(b_pos.0 < c_pos.0);
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
        
        let mut options = DagreLayoutOptions::default();
        options.acyclic = true;
        
        let engine = DagreLayoutEngine::new(options);
        let mut layers = engine.assign_layers(&graph).unwrap();
        engine.break_cycles(&mut graph, &mut layers).unwrap();
        
        // After cycle breaking, we should have either all A->B or all B->A
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
