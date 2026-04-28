use rand::prelude::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("docs/sample/large_graph.json");
    let node_count = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1000);
    let edge_density = args.get(3).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.01);
    
    println!("Generating large graph with {} nodes and {:.2}% edge density to {}", 
             node_count, edge_density * 100.0, output_path);
    
    let start_time = Instant::now();
    let graph = generate_large_graph(node_count, edge_density);
    let generation_time = start_time.elapsed();
    
    println!("Graph generation completed in {:.2?}", generation_time);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let serialization_start = Instant::now();
    let json_string = serde_json::to_string(&graph).expect("Failed to serialize graph");
    let serialization_time = serialization_start.elapsed();
    
    println!("JSON serialization completed in {:.2?}", serialization_time);
    
    let write_start = Instant::now();
    let mut file = File::create(output_path).expect("Failed to create output file");
    file.write_all(json_string.as_bytes()).expect("Failed to write to file");
    let write_time = write_start.elapsed();
    
    println!("File write completed in {:.2?}", write_time);
    
    let node_count = graph["nodes"].as_array().unwrap().len();
    let edge_count = graph["edges"].as_array().unwrap().len();
    let file_size = json_string.len() / 1024; // Size in KB
    
    println!("Graph statistics:");
    println!("  - Nodes: {}", node_count);
    println!("  - Edges: {}", edge_count);
    println!("  - File size: {} KB", file_size);
    println!("  - Total time: {:.2?}", start_time.elapsed());
}

fn generate_large_graph(node_count: usize, edge_density: f64) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    println!("Generating {} nodes...", node_count);
    let node_start = Instant::now();
    
    let mut nodes = Vec::with_capacity(node_count);
    
    for i in 1..=node_count {
        // Progress indicator for large graphs
        if i % 1000 == 0 || i == node_count {
            println!("  - Generated {} nodes ({:.1}%)", i, (i as f64 / node_count as f64) * 100.0);
        }
        
        // Create node with minimal attributes to keep file size manageable
        let types = ["data", "process", "entity", "concept", "resource"];
        nodes.push(json!({
            "id": format!("n{}", i),
            "label": format!("Node {}", i),
            "type": types[rng.gen_range(0..4)]
        }));
    }
    
    let node_time = node_start.elapsed();
    println!("Node generation completed in {:.2?}", node_time);
    
    // Generate edges
    // For large graphs, we use edge_density to control the number of edges
    // edge_density of 1.0 would mean every node connects to every other node (n*(n-1)/2 edges)
    // Typically we want much lower densities like 0.01 (1%) for large graphs
    
    let max_possible_edges = node_count * (node_count - 1) / 2;
    let target_edge_count = (max_possible_edges as f64 * edge_density) as usize;
    
    println!("Generating approximately {} edges (density: {:.2}%)...", 
             target_edge_count, edge_density * 100.0);
    
    let edge_start = Instant::now();
    let mut edges = Vec::with_capacity(target_edge_count);
    
    // For very large graphs, generating all possible edges and then sampling
    // would be inefficient. Instead, we'll use a probabilistic approach.
    
    // For small to medium graphs or higher densities, we can use direct edge generation
    if node_count <= 10000 || edge_density > 0.1 {
        let mut edge_count = 0;
        let edge_probability = edge_density;
        
        // Track progress
        let progress_interval = std::cmp::max(1, target_edge_count / 10);
        
        // Generate random edges based on probability
        let types = ["connects", "relates", "depends", "references", "associates"];
        for i in 1..=node_count {
            for j in (i+1)..=node_count {
                if rng.gen_bool(edge_probability) {
                    let tt = 
                    edges.push(json!({
                        "source": format!("n{}", i),
                        "target": format!("n{}", j),
                        "type": types[rng.gen_range(0..4)]
                    }));
                    
                    edge_count += 1;
                    
                    // Progress indicator
                    if edge_count % progress_interval == 0 || edge_count == target_edge_count {
                        println!("  - Generated {} edges ({:.1}%)", 
                                 edge_count, 
                                 (edge_count as f64 / target_edge_count as f64) * 100.0);
                    }
                    
                    // Stop if we've reached the target
                    if edge_count >= target_edge_count {
                        break;
                    }
                }
            }
            
            // Stop if we've reached the target
            if edge_count >= target_edge_count {
                break;
            }
        }
    } else {
        // For very large graphs with low density, use a different approach
        // Generate exactly the target number of edges by sampling random node pairs
        
        let mut edge_set = std::collections::HashSet::new();
        let progress_interval = std::cmp::max(1, target_edge_count / 10);
        
        while edge_set.len() < target_edge_count {
            let source = rng.gen_range(1..=node_count);
            let mut target = rng.gen_range(1..=node_count);
            
            // Avoid self-loops
            while target == source {
                target = rng.gen_range(1..=node_count);
            }
            
            // Ensure source < target to avoid duplicates
            let (min_node, max_node) = if source < target {
                (source, target)
            } else {
                (target, source)
            };
            
            // Add to set if it's a new edge
            if edge_set.insert((min_node, max_node)) {
                // Progress indicator
                if edge_set.len() % progress_interval == 0 || edge_set.len() == target_edge_count {
                    println!("  - Generated {} edges ({:.1}%)", 
                             edge_set.len(), 
                             (edge_set.len() as f64 / target_edge_count as f64) * 100.0);
                }
            }
        }
        
        // Convert the set to JSON edges
        let types = ["connects", "relates", "depends", "references", "associates"];
        for (source, target) in edge_set {
            edges.push(json!({
                "source": format!("n{}", source),
                "target": format!("n{}", target),
                "type": types[rng.gen_range(0..4)]
            }));
        }
    }
    
    let edge_time = edge_start.elapsed();
    println!("Edge generation completed in {:.2?}", edge_time);
    println!("Generated {} edges", edges.len());
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}
