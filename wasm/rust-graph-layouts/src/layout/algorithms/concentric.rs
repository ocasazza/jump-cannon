use crate::types::{Graph, ConcentricLayoutOptions};
use crate::layout::traits::{LayoutEngine, HierarchicalLayout};

pub struct ConcentricLayoutEngine {
    options: ConcentricLayoutOptions,
}

impl ConcentricLayoutEngine {
    pub fn new(options: ConcentricLayoutOptions) -> Self {
        Self { options }
    }
}

impl LayoutEngine for ConcentricLayoutEngine {
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String> {
        // Step 1: Assign nodes to levels based on the concentric_by property
        let levels = self.assign_levels(graph)?;
        
        // Step 2: Position nodes in concentric circles
        self.position_nodes(graph, &levels)?;
        
        Ok(())
    }
    
    fn name(&self) -> &'static str {
        "Concentric"
    }
    
    fn description(&self) -> &'static str {
        "Concentric layout algorithm that positions nodes in concentric circles"
    }
}

impl HierarchicalLayout for ConcentricLayoutEngine {
    fn assign_levels(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String> {
        let mut levels = Vec::new();
        
        // TODO: Implement level assignment based on concentric_by property
        // For example, if concentric_by is "degree":
        // 1. Calculate node degrees
        // 2. Group nodes by degree
        // 3. Sort groups by degree value
        // 4. Each group becomes a level
        
        match self.options.concentric_by.as_str() {
            "degree" => {
                // Calculate node degrees
                let mut node_degrees: Vec<(String, usize)> = graph.nodes.keys()
                    .map(|id| {
                        let degree = graph.edges.values()
                            .filter(|e| e.source == *id || e.target == *id)
                            .count();
                        (id.clone(), degree)
                    })
                    .collect();
                
                // Sort by degree
                node_degrees.sort_by_key(|(_, degree)| *degree);
                
                // Group by degree
                let mut current_degree = None;
                let mut current_level = Vec::new();
                
                for (id, degree) in node_degrees {
                    match current_degree {
                        Some(d) if d == degree => {
                            current_level.push(id);
                        }
                        _ => {
                            if !current_level.is_empty() {
                                levels.push(std::mem::take(&mut current_level));
                            }
                            current_level.push(id);
                            current_degree = Some(degree);
                        }
                    }
                }
                
                if !current_level.is_empty() {
                    levels.push(current_level);
                }
            }
            "id" => {
                // Simple level assignment based on node IDs
                levels.push(graph.nodes.keys().cloned().collect());
            }
            _ => return Err(format!("Unsupported concentric_by value: {}", self.options.concentric_by)),
        }
        
        Ok(levels)
    }
    
    fn position_nodes(&self, graph: &mut Graph, levels: &[Vec<String>]) -> Result<(), String> {
        let center_x = 0.0;
        let center_y = 0.0;
        
        // Position nodes in concentric circles
        for (level_idx, level) in levels.iter().enumerate() {
            let radius = (level_idx + 1) as f64 * self.options.level_width;
            let angle_step = 2.0 * std::f64::consts::PI / level.len() as f64;
            
            for (node_idx, node_id) in level.iter().enumerate() {
                if let Some(node) = graph.nodes.get_mut(node_id) {
                    let angle = angle_step * node_idx as f64;
                    let x = center_x + radius * angle.cos();
                    let y = center_y + radius * angle.sin();
                    node.position = Some((x, y));
                }
            }
        }
        
        Ok(())
    }
}

/// Public interface for applying the Concentric layout algorithm
pub fn apply_layout(graph: &mut Graph, options: &ConcentricLayoutOptions) -> Result<(), String> {
    let engine = ConcentricLayoutEngine::new(options.clone());
    engine.apply_layout(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Node, Edge};

    #[test]
    fn test_level_assignment() {
        let mut graph = Graph::new();
        
        // Create a star-shaped graph
        let center = Node::new("center");
        graph.add_node(center);
        
        for i in 0..5 {
            let node = Node::new(format!("node{}", i));
            graph.add_node(node);
            
            let edge = Edge::new(
                format!("edge{}", i),
                "center".to_string(),
                format!("node{}", i),
            );
            graph.add_edge(edge);
        }
        
        let mut options = ConcentricLayoutOptions::default();
        options.concentric_by = "degree".to_string();
        
        let engine = ConcentricLayoutEngine::new(options);
        let levels = engine.assign_levels(&graph).unwrap();
        
        // Center node should be in first level (highest degree)
        assert_eq!(levels[0], vec!["center"]);
        
        // Other nodes should be in second level (all same degree)
        assert_eq!(levels[1].len(), 5);
    }
}
