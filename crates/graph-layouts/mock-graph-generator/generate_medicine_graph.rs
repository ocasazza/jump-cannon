use rand::prelude::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("docs/sample/medicine_graph.json");
    let node_count = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    
    println!("Generating medicine domain graph with {} nodes to {}", 
             node_count, output_path);
    
    let graph = generate_medicine_graph(node_count);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let mut file = File::create(output_path).expect("Failed to create output file");
    let json_string = serde_json::to_string_pretty(&graph).expect("Failed to serialize graph");
    file.write_all(json_string.as_bytes()).expect("Failed to write to file");
    
    println!("Medicine domain graph generated successfully!");
}

fn generate_medicine_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Define node types and their proportions
    let node_types = [
        ("disease", 0.2),
        ("drug", 0.2),
        ("symptom", 0.15),
        ("treatment", 0.1),
        ("organ", 0.1),
        ("doctor", 0.1),
        ("specialty", 0.05),
        ("test", 0.1),
    ];
    
    // Define edge types
    let edge_types = [
        "treats", "causes", "indicates", "affects", "specializes_in", 
        "performs", "prescribes", "diagnoses", "part_of", "related_to",
    ];
    
    // Generate nodes
    let mut nodes = Vec::new();
    let mut node_counts_by_type: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    
    for i in 1..=node_count {
        // Determine node type based on proportions
        let node_type = {
            let r: f64 = rng.gen();
            let mut cumulative = 0.0;
            let mut selected = node_types[0].0;
            
            for (t, proportion) in &node_types {
                cumulative += proportion;
                if r <= cumulative {
                    selected = t;
                    break;
                }
            }
            selected
        };
        
        // Count nodes by type
        *node_counts_by_type.entry(node_type).or_insert(0) += 1;
        let type_count = node_counts_by_type[node_type];
        
        // Generate node data based on type
        let (label, extra_fields) = match node_type {
            "disease" => {
                let disease_name = format!("Disease {}", type_count);
                (
                    format!("Disease: {}", disease_name),
                    json!({
                        "prevalence": format!("{}%", (rng.gen::<f64>() * 20.0).round() / 10.0),
                        "chronic": rng.gen_bool(0.5),
                        "severity": rng.gen_range(1..10)
                    })
                )
            },
            "drug" => {
                let drug_name = format!("Drug {}", type_count);
                (
                    format!("Drug: {}", drug_name),
                    json!({
                        "approved_year": rng.gen_range(1950..2023),
                        "prescription_required": rng.gen_bool(0.7),
                        "class_id": rng.gen_range(1..5)
                    })
                )
            },
            "symptom" => {
                let symptom_name = format!("Symptom {}", type_count);
                (
                    format!("Symptom: {}", symptom_name),
                    json!({
                        "common": rng.gen_bool(0.6),
                        "severity": (rng.gen::<f64>() * 10.0).round() / 10.0
                    })
                )
            },
            "treatment" => {
                let treatment_name = format!("Treatment {}", type_count);
                (
                    format!("Treatment: {}", treatment_name),
                    json!({
                        "invasive": rng.gen_bool(0.4),
                        "cost_level": rng.gen_range(1..4),
                        "effectiveness": (rng.gen::<f64>() * 10.0).round() / 10.0
                    })
                )
            },
            "organ" => {
                let organ_name = format!("Organ {}", type_count);
                (
                    format!("Organ: {}", organ_name),
                    json!({
                        "system_id": rng.gen_range(1..5),
                        "vital": rng.gen_bool(0.3)
                    })
                )
            },
            "doctor" => {
                let doctor_name = format!("Doctor {}", type_count);
                (
                    format!("Doctor: {}", doctor_name),
                    json!({
                        "years_training": rng.gen_range(4..15),
                        "surgical": rng.gen_bool(0.4)
                    })
                )
            },
            "specialty" => {
                let specialty_name = format!("Specialty {}", type_count);
                (
                    format!("Specialty: {}", specialty_name),
                    json!({
                        "subspecialties": rng.gen_range(2..8),
                        "established": rng.gen_range(1800..1980)
                    })
                )
            },
            "test" => {
                let test_name = format!("Test {}", type_count);
                (
                    format!("Test: {}", test_name),
                    json!({
                        "invasive": rng.gen_bool(0.3),
                        "accuracy": (rng.gen::<f64>() * 30.0 + 70.0).round() / 100.0,
                        "cost": rng.gen_range(50..5000)
                    })
                )
            },
            _ => (format!("Node {}", i), json!({})),
        };
        
        // Create node with random position
        let x = rng.gen_range(100.0..1200.0);
        let y = rng.gen_range(100.0..900.0);
        
        let mut node = json!({
            "id": format!("n{}", i),
            "label": label,
            "x": x,
            "y": y,
            "type": node_type
        });
        
        // Add extra fields
        if let Value::Object(ref mut map) = node {
            if let Value::Object(extra) = extra_fields {
                for (k, v) in extra.iter() {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
        
        nodes.push(node);
    }
    
    // Generate edges
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
            let edge_type = match (source_type, target_type) {
                ("drug", "disease") => "treats",
                ("disease", "symptom") => "causes",
                ("symptom", "disease") => "indicates",
                ("disease", "organ") => "affects",
                ("doctor", "specialty") => "specializes_in",
                ("doctor", "treatment") => "performs",
                ("doctor", "drug") => "prescribes",
                ("test", "disease") => "diagnoses",
                ("organ", "organ") => "connected_to",
                ("treatment", "disease") => "treats",
                _ => {
                    let idx = rng.gen_range(0..edge_types.len());
                    edge_types[idx]
                },
            };
            
            let weight = (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0;
            
            edges.push(json!({
                "source": source_id,
                "target": target_id,
                "type": edge_type,
                "weight": weight
            }));
        }
    }
    
    json!({
        "nodes": nodes,
        "edges": edges
    })
}
