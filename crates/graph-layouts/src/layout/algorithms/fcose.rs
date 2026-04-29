pub use crate::types::FcoseOptions;
use crate::types::Graph;
use crate::layout::{LayoutEngine, ForceDirectedLayout};
use rand::Rng;

pub struct FcoseLayoutEngine {
    options: FcoseOptions,
}

impl FcoseLayoutEngine {
    pub fn new(options: FcoseOptions) -> Self {
        Self { options }
    }

    /// Initialize random positions for nodes that don't have positions
    fn initialize_positions(&self, graph: &mut Graph) {
        let radius = 100.0;
        let mut rng = rand::thread_rng();
        
        for node in graph.nodes.values_mut() {
            if node.position.is_none() {
                // Generate random angle and distance from center
                let angle = rng.gen::<f64>() * 2.0 * std::f64::consts::PI;
                let distance = rng.gen::<f64>() * radius;
                
                // Convert to Cartesian coordinates
                let x = distance * angle.cos();
                let y = distance * angle.sin();
                
                node.position = Some((x, y));
            }
        }
    }
    
    /// Remove node overlaps as a post-processing step
    fn remove_overlaps(&self, graph: &mut Graph) -> Result<(), String> {
        let node_overlap = self.options.node_overlap;
        let node_size = 10.0; // Assume all nodes have the same size for simplicity
        let min_distance = node_size * 2.0 * (1.0 - node_overlap / 100.0);
        let mut rng = rand::thread_rng();
        
        // Get node positions
        let mut nodes: Vec<(&String, &mut crate::types::Node)> = graph.nodes.iter_mut().collect();
        let node_count = nodes.len();
        
        // Iterate until no more overlaps are detected or max iterations reached
        let max_iterations = 50;
        let mut iteration = 0;
        let mut overlaps_exist = true;
        
        while overlaps_exist && iteration < max_iterations {
            overlaps_exist = false;
            
            // Check all pairs of nodes for overlaps
            for i in 0..node_count {
                let pos_i = nodes[i].1.position.unwrap_or((0.0, 0.0));
                
                for j in i+1..node_count {
                    let pos_j = nodes[j].1.position.unwrap_or((0.0, 0.0));
                    
                    // Calculate distance between nodes
                    let dx = pos_j.0 - pos_i.0;
                    let dy = pos_j.1 - pos_i.1;
                    let distance = (dx * dx + dy * dy).sqrt();
                    
                    // Check if nodes overlap
                    if distance < min_distance {
                        overlaps_exist = true;
                        
                        // Calculate repulsion vector
                        let force = min_distance - distance;
                        let force_x = if distance > 0.1 { force * dx / distance } else { rng.gen::<f64>() * 2.0 - 1.0 };
                        let force_y = if distance > 0.1 { force * dy / distance } else { rng.gen::<f64>() * 2.0 - 1.0 };
                        
                        // Move nodes apart
                        let pos_i = nodes[i].1.position.unwrap_or((0.0, 0.0));
                        let pos_j = nodes[j].1.position.unwrap_or((0.0, 0.0));
                        
                        nodes[i].1.position = Some((pos_i.0 - force_x / 2.0, pos_i.1 - force_y / 2.0));
                        nodes[j].1.position = Some((pos_j.0 + force_x / 2.0, pos_j.1 + force_y / 2.0));
                    }
                }
            }
            
            iteration += 1;
        }
        
        Ok(())
    }
}

impl LayoutEngine for FcoseLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Initialize node positions if not already set
        self.initialize_positions(graph);
        
        // Run the force-directed algorithm for a fixed number of iterations
        let max_iterations = match self.options.quality.as_str() {
            "draft" => 30,
            "proof" => 100,
            _ => 50, // default
        };
        
        let mut _temperature = 1.0; // For simulated annealing
        
        for _i in 0..max_iterations {
            // Calculate repulsive forces between all pairs of nodes
            let repulsion_forces = self.calculate_repulsion(graph);
            
            // Calculate attractive forces along edges
            let attraction_forces = self.calculate_attraction(graph);
            
            // Combine forces
            let mut combined_forces = vec![(0.0, 0.0); graph.nodes.len()];
            for i in 0..graph.nodes.len() {
                combined_forces[i] = (
                    repulsion_forces[i].0 + attraction_forces[i].0,
                    repulsion_forces[i].1 + attraction_forces[i].1
                );
            }
            
            // Apply forces to update node positions
            self.apply_forces(graph, &combined_forces)?;
            
            // Cool down temperature for simulated annealing
            _temperature *= 0.95;
        }
        
        // Skip O(n²) overlap removal for large graphs — scale factor in the caller handles spacing
        if graph.nodes.len() < 500 {
            self.remove_overlaps(graph)?;
        }
        
        Ok(())
    }
    
    fn name(&self) -> &'static str {
        "Force-Directed (fCoSE)"
    }
    
    fn description(&self) -> &'static str {
        "Force-directed layout algorithm optimized for compound graphs"
    }
}

impl ForceDirectedLayout for FcoseLayoutEngine {
    fn calculate_repulsion(&self, graph: &Graph) -> Vec<(f64, f64)> {
        let node_count = graph.nodes.len();
        let mut forces = vec![(0.0, 0.0); node_count];
        let node_repulsion = self.options.node_repulsion;
        
        // Get node positions as a vector for easier indexing
        let nodes: Vec<(&String, &crate::types::Node)> = graph.nodes.iter().collect();
        
        // Calculate repulsive forces between all pairs of nodes
        for i in 0..node_count {
            let (_, node_i) = nodes[i];
            let pos_i = node_i.position.unwrap_or((0.0, 0.0));
            
            for j in 0..node_count {
                if i == j { continue; }
                
                let (_, node_j) = nodes[j];
                let pos_j = node_j.position.unwrap_or((0.0, 0.0));
                
                // Calculate distance between nodes
                let dx = pos_i.0 - pos_j.0;
                let dy = pos_i.1 - pos_j.1;
                let distance_squared = dx * dx + dy * dy;
                
                // Avoid division by zero
                if distance_squared < 0.1 {
                    continue;
                }
                
                // Calculate repulsive force (inverse square law)
                let force = node_repulsion / distance_squared;
                
                // Calculate force components
                let force_x = force * dx / distance_squared.sqrt();
                let force_y = force * dy / distance_squared.sqrt();
                
                // Add to total forces for node i
                forces[i] = (forces[i].0 + force_x, forces[i].1 + force_y);
            }
        }
        
        forces
    }
    
    fn calculate_attraction(&self, graph: &Graph) -> Vec<(f64, f64)> {
        let node_count = graph.nodes.len();
        let mut forces = vec![(0.0, 0.0); node_count];
        let ideal_edge_length = self.options.ideal_edge_length;
        
        // Get node positions and create a map from ID to index
        let nodes: Vec<(&String, &crate::types::Node)> = graph.nodes.iter().collect();
        let mut id_to_index = std::collections::HashMap::new();
        for (i, (id, _)) in nodes.iter().enumerate() {
            id_to_index.insert(*id, i);
        }
        
        // Calculate attractive forces along edges
        for edge in graph.edges.values() {
            if let (Some(&source_idx), Some(&target_idx)) = (id_to_index.get(&edge.source), id_to_index.get(&edge.target)) {
                let source_pos = nodes[source_idx].1.position.unwrap_or((0.0, 0.0));
                let target_pos = nodes[target_idx].1.position.unwrap_or((0.0, 0.0));
                
                // Calculate distance and direction
                let dx = target_pos.0 - source_pos.0;
                let dy = target_pos.1 - source_pos.1;
                let distance = (dx * dx + dy * dy).sqrt();
                
                // Avoid division by zero
                if distance < 0.1 {
                    continue;
                }
                
                // Calculate attractive force (spring force)
                let force = (distance - ideal_edge_length) / 3.0;
                
                // Calculate force components
                let force_x = force * dx / distance;
                let force_y = force * dy / distance;
                
                // Apply to both nodes in opposite directions
                forces[source_idx] = (forces[source_idx].0 + force_x, forces[source_idx].1 + force_y);
                forces[target_idx] = (forces[target_idx].0 - force_x, forces[target_idx].1 - force_y);
            }
        }
        
        forces
    }
    
    fn apply_forces(&self, graph: &mut Graph, forces: &[(f64, f64)]) -> Result<(), String> {
        // Get mutable references to nodes
        let mut nodes: Vec<(&String, &mut crate::types::Node)> = graph.nodes.iter_mut().collect();
        
        // Apply forces to update positions
        for (i, (_, node)) in nodes.iter_mut().enumerate() {
            if i >= forces.len() {
                break;
            }
            
            let (force_x, force_y) = forces[i];
            let current_pos = node.position.unwrap_or((0.0, 0.0));
            
            // Update position with damping
            let damping = 0.1;
            let new_x = current_pos.0 + force_x * damping;
            let new_y = current_pos.1 + force_y * damping;
            
            node.position = Some((new_x, new_y));
        }
        
        Ok(())
    }
}

/// Public interface for applying the fCoSE layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &FcoseOptions) -> Result<(), String> {
    let engine = FcoseLayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Graph, Node, Edge};
    use std::collections::HashMap;

    fn create_test_graph() -> Graph {
        let mut nodes = HashMap::new();
        let mut edges = HashMap::new();

        // Add two nodes with fixed positions
        let mut node1 = Node::default();
        node1.position = Some((0.0, 0.0));
        nodes.insert("1".to_string(), node1);

        let mut node2 = Node::default();
        node2.position = Some((100.0, 0.0));
        nodes.insert("2".to_string(), node2);

        // Add an edge between them
        let edge = Edge {
            id: "1-2".to_string(),
            source: "1".to_string(),
            target: "2".to_string(),
            weight: 1.0,
            ..Default::default()
        };
        edges.insert("1-2".to_string(), edge);

        Graph { nodes, edges }
    }

    #[test]
    fn test_fcose_options_default() {
        let options = FcoseOptions::default();
        assert_eq!(options.quality, "default");
        assert_eq!(options.node_repulsion, 4500.0);
        assert_eq!(options.ideal_edge_length, 50.0);
        assert_eq!(options.node_overlap, 10.0);
    }

    #[test]
    fn test_repulsion_force_calculation() {
        let graph = create_test_graph();
        let options = FcoseOptions::default();
        let engine = FcoseLayoutEngine::new(options);
        
        let forces = engine.calculate_repulsion(&graph);
        
        // For two nodes 100 units apart, we can calculate the expected repulsion
        // Force = node_repulsion / distance^2
        let expected_force = 4500.0 / (100.0 * 100.0);
        let force_magnitude = (forces[0].0 * forces[0].0 + forces[0].1 * forces[0].1).sqrt();
        
        // Allow for some floating point imprecision
        assert!((force_magnitude - expected_force).abs() < 0.001);
    }

    #[test]
    fn test_attraction_force_calculation() {
        let graph = create_test_graph();
        let options = FcoseOptions::default();
        let engine = FcoseLayoutEngine::new(options);
        
        let forces = engine.calculate_attraction(&graph);
        
        // For two nodes 100 units apart with ideal length 50:
        // Force = (distance - ideal_length) / 3
        let expected_force = (100.0 - 50.0) / 3.0;
        let force_magnitude = (forces[0].0 * forces[0].0 + forces[0].1 * forces[0].1).sqrt();
        
        // Allow for some floating point imprecision
        assert!((force_magnitude - expected_force).abs() < 0.001);
    }

    #[test]
    fn test_overlap_removal() {
        let mut graph = Graph {
            nodes: HashMap::new(),
            edges: HashMap::new(),
        };

        // Create two overlapping nodes
        let mut node1 = Node::default();
        node1.position = Some((0.0, 0.0));
        graph.nodes.insert("1".to_string(), node1);

        let mut node2 = Node::default();
        node2.position = Some((5.0, 0.0));  // Very close to node1
        graph.nodes.insert("2".to_string(), node2);

        let options = FcoseOptions::default();
        let engine = FcoseLayoutEngine::new(options);
        
        // Remove overlaps
        engine.remove_overlaps(&mut graph).unwrap();

        // Get final positions
        let pos1 = graph.nodes.get("1").unwrap().position.unwrap();
        let pos2 = graph.nodes.get("2").unwrap().position.unwrap();
        
        // Calculate final distance
        let dx = pos2.0 - pos1.0;
        let dy = pos2.1 - pos1.1;
        let final_distance = (dx * dx + dy * dy).sqrt();
        
        // Nodes should be at least min_distance apart (20.0 * (1.0 - 10.0/100.0))
        let min_distance = 20.0 * (1.0 - 10.0/100.0);
        assert!(final_distance >= min_distance);
    }
}
