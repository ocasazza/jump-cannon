use rand::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;

// Common utility function to generate a node type based on proportions
pub fn determine_node_type<'a>(rng: &mut ThreadRng, node_types: &[(&'a str, f64)]) -> &'a str {
    let r: f64 = rng.gen();
    let mut cumulative = 0.0;
    let mut selected = node_types[0].0;
    
    for (t, proportion) in node_types {
        cumulative += proportion;
        if r <= cumulative {
            selected = t;
            break;
        }
    }
    selected
}

// Common utility function to generate edges
pub fn generate_edges<F>(
    nodes: &[Value], 
    node_count: usize, 
    edge_types: &[&str], 
    edge_type_mapper: F,
    rng: &mut ThreadRng
) -> Vec<Value> 
where 
    F: Fn(&str, &str) -> &str
{
    let mut edges = Vec::new();
    let edge_count = (node_count as f64 * 1.5).round() as usize; // ~1.5 edges per node
    
    for _ in 0..edge_count {
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        // Avoid self-loops
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        if let (Value::Object(source), Value::Object(target)) = (&nodes[source_idx], &nodes[target_idx]) {
            let source_id = source["id"].as_str().unwrap();
            let target_id = target["id"].as_str().unwrap();
            let source_type = source["type"].as_str().unwrap();
            let target_type = target["type"].as_str().unwrap();
            
            // Choose an appropriate edge type based on node types
            let edge_type = edge_type_mapper(source_type, target_type);
            
            let weight = (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0;
            
            edges.push(json!({
                "source": source_id,
                "target": target_id,
                "type": edge_type,
                "weight": weight
            }));
        }
    }
    
    edges
}
