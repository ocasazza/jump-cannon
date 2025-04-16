use crate::types::{Graph, FcoseLayoutOptions};
use crate::layout::traits::{LayoutEngine, ForceDirectedLayout};

pub struct FcoseLayoutEngine {
    options: FcoseLayoutOptions,
}

impl FcoseLayoutEngine {
    pub fn new(options: FcoseLayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for FcoseLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Initialize node positions if not already set
        self.initialize_positions(graph);
        
        // Run the force-directed algorithm for a fixed number of iterations
        let max_iterations = 50;
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
        
        // Apply overlap removal as a post-processing step
        self.remove_overlaps(graph)?;
        
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

impl FcoseLayoutEngine {
    /// Initialize random positions for nodes that don't have positions
    fn initialize_positions(&self, graph: &mut Graph) {
        let radius = 100.0;
        
        for node in graph.nodes.values_mut() {
            if node.position.is_none() {
                // Generate random angle and distance from center
                let angle = rand::random::<f64>() * 2.0 * std::f64::consts::PI;
                let distance = rand::random::<f64>() * radius;
                
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
                        let force_x = if distance > 0.1 { force * dx / distance } else { rand::random::<f64>() * 2.0 - 1.0 };
                        let force_y = if distance > 0.1 { force * dy / distance } else { rand::random::<f64>() * 2.0 - 1.0 };
                        
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

/// Public interface for applying the fCoSE layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &FcoseLayoutOptions) -> Result<(), String> {
    let engine = FcoseLayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}
