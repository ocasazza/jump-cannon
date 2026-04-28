use rand::prelude::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let domain = args.get(1).map(|s| s.as_str()).unwrap_or("programming");
    
    // Create the output path string
    let default_output_path = format!("docs/sample/{}_graph.json", domain);
    
    // Get the output path from args or use the default
    let output_path = match args.get(2) {
        Some(path) => path.as_str(),
        None => &default_output_path,
    };
    
    let node_count = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    
    println!("Generating {} domain graph with {} nodes to {}", 
             domain, node_count, output_path);
    
    let graph = match domain {
        "programming" => generate_programming_graph(node_count),
        "science" => generate_science_graph(node_count),
        "business" => generate_business_graph(node_count),
        "medicine" => generate_medicine_graph(node_count),
        _ => {
            eprintln!("Unknown domain: {}. Using programming domain instead.", domain);
            generate_programming_graph(node_count)
        }
    };
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let mut file = File::create(output_path).expect("Failed to create output file");
    let json_string = serde_json::to_string_pretty(&graph).expect("Failed to serialize graph");
    file.write_all(json_string.as_bytes()).expect("Failed to write to file");
    
    println!("Domain graph generated successfully!");
}

fn generate_medicine_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    let mut nodes = Vec::new();
    for i in 1..=node_count {
        nodes.push(json!({
            "id": format!("n{}", i),
            "label": format!("Medicine Node {}", i),
            "x": rng.gen_range(100.0..1200.0),
            "y": rng.gen_range(100.0..900.0),
            "type": "medicine"
        }));
    }
    
    // Generate edges
    let mut edges = Vec::new();
    let edge_count = (node_count as f64 * 1.5).round() as usize;
    
    for _ in 0..edge_count {
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        let relation_types = ["treats", "causes", "indicates", "affects", "specializes_in"];
        let relation_type = relation_types[rng.gen_range(0..relation_types.len())];
        
        edges.push(json!({
            "source": format!("n{}", source_idx + 1),
            "target": format!("n{}", target_idx + 1),
            "type": relation_type,
            "weight": (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0
        }));
    }
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}

fn generate_programming_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    let mut nodes = Vec::new();
    for i in 1..=node_count {
        nodes.push(json!({
            "id": format!("n{}", i),
            "label": format!("Programming Node {}", i),
            "x": rng.gen_range(100.0..1200.0),
            "y": rng.gen_range(100.0..900.0),
            "type": "programming"
        }));
    }
    
    // Generate edges
    let mut edges = Vec::new();
    let edge_count = (node_count as f64 * 1.5).round() as usize;
    
    for _ in 0..edge_count {
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        let relation_types = ["uses", "implements", "extends", "depends_on", "created_by"];
        let relation_type = relation_types[rng.gen_range(0..relation_types.len())];
        
        edges.push(json!({
            "source": format!("n{}", source_idx + 1),
            "target": format!("n{}", target_idx + 1),
            "type": relation_type,
            "weight": (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0
        }));
    }
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}

fn generate_science_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    let mut nodes = Vec::new();
    for i in 1..=node_count {
        nodes.push(json!({
            "id": format!("n{}", i),
            "label": format!("Science Node {}", i),
            "x": rng.gen_range(100.0..1200.0),
            "y": rng.gen_range(100.0..900.0),
            "type": "science"
        }));
    }
    
    // Generate edges
    let mut edges = Vec::new();
    let edge_count = (node_count as f64 * 1.5).round() as usize;
    
    for _ in 0..edge_count {
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        let relation_types = ["related_to", "part_of", "discovered_by", "works_at", "published_in"];
        let relation_type = relation_types[rng.gen_range(0..relation_types.len())];
        
        edges.push(json!({
            "source": format!("n{}", source_idx + 1),
            "target": format!("n{}", target_idx + 1),
            "type": relation_type,
            "weight": (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0
        }));
    }
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}

fn generate_business_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    let mut nodes = Vec::new();
    for i in 1..=node_count {
        nodes.push(json!({
            "id": format!("n{}", i),
            "label": format!("Business Node {}", i),
            "x": rng.gen_range(100.0..1200.0),
            "y": rng.gen_range(100.0..900.0),
            "type": "business"
        }));
    }
    
    // Generate edges
    let mut edges = Vec::new();
    let edge_count = (node_count as f64 * 1.5).round() as usize;
    
    for _ in 0..edge_count {
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        let relation_types = ["competes_with", "part_of", "produces", "leads", "operates_in"];
        let relation_type = relation_types[rng.gen_range(0..relation_types.len())];
        
        edges.push(json!({
            "source": format!("n{}", source_idx + 1),
            "target": format!("n{}", target_idx + 1),
            "type": relation_type,
            "weight": (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0
        }));
    }
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}
