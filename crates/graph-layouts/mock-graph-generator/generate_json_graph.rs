use rand::prelude::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("docs/sample/generated_graph.json");
    let node_count = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    
    println!("Generating JSON graph with {} nodes to {}", node_count, output_path);
    
    let graph = generate_tech_knowledge_graph(node_count);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let mut file = File::create(output_path).expect("Failed to create output file");
    let json_string = serde_json::to_string_pretty(&graph).expect("Failed to serialize graph");
    file.write_all(json_string.as_bytes()).expect("Failed to write to file");
    
    println!("Graph generated successfully!");
}

fn generate_tech_knowledge_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Define node types and their proportions
    let node_types = [
        ("concept", 0.3),
        ("person", 0.25),
        ("organization", 0.2),
        ("paper", 0.15),
        ("application", 0.1),
    ];
    
    // Define edge types
    let edge_types = [
        "includes", "related", "used_in", "uses", "contributed_to", 
        "affiliated_with", "authored", "implements", "cites", "developed_by",
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
            "concept" => {
                let concepts = [
                    "Distributed Systems", "Blockchain", "Quantum Computing", 
                    "Cloud Computing", "Edge Computing", "Internet of Things",
                    "Big Data", "Data Science", "Artificial Intelligence",
                    "Cybersecurity", "DevOps", "Microservices",
                    "Serverless", "Containerization", "Web3",
                    "Virtual Reality", "Augmented Reality", "5G",
                    "Robotics", "Bioinformatics", "Green Computing",
                ];
                let concept = concepts[type_count % concepts.len()];
                (
                    format!("Concept: {}", concept),
                    json!({
                        "importance": (rng.gen::<f64>() * 0.3 + 0.7).round() * 100.0 / 100.0
                    })
                )
            },
            "person" => {
                let names = [
                    "Ada Lovelace", "Alan Turing", "Grace Hopper", 
                    "Tim Berners-Lee", "Linus Torvalds", "Donald Knuth",
                    "Barbara Liskov", "Vint Cerf", "Margaret Hamilton",
                    "John McCarthy", "Guido van Rossum", "Ken Thompson",
                    "Dennis Ritchie", "Bjarne Stroustrup", "James Gosling",
                    "Brendan Eich", "Anders Hejlsberg", "Yukihiro Matsumoto",
                ];
                let name = names[type_count % names.len()];
                (
                    format!("Person: {}", name),
                    json!({
                        "importance": (rng.gen::<f64>() * 0.2 + 0.75).round() * 100.0 / 100.0
                    })
                )
            },
            "organization" => {
                let orgs = [
                    "IBM", "Microsoft", "Apple", "Amazon", "Google", "Facebook",
                    "Intel", "AMD", "NVIDIA", "Oracle", "SAP", "Salesforce",
                    "MIT", "Stanford", "Berkeley", "CMU", "ETH Zurich", "Oxford",
                    "CERN", "NASA", "DARPA", "IEEE", "ACM", "W3C",
                ];
                let org = orgs[type_count % orgs.len()];
                (
                    format!("Organization: {}", org),
                    json!({
                        "importance": (rng.gen::<f64>() * 0.3 + 0.6).round() * 100.0 / 100.0,
                        "founded": rng.gen_range(1900..2020)
                    })
                )
            },
            "paper" => {
                let papers = [
                    "Bitcoin: A Peer-to-Peer Electronic Cash System",
                    "A Mathematical Theory of Communication",
                    "Computing Machinery and Intelligence",
                    "On Computable Numbers",
                    "The Anatomy of a Large-Scale Hypertextual Web Search Engine",
                    "MapReduce: Simplified Data Processing on Large Clusters",
                    "The PageRank Citation Ranking",
                    "Attention Is All You Need",
                    "Design Patterns: Elements of Reusable Object-Oriented Software",
                    "A Relational Model of Data for Large Shared Data Banks",
                ];
                let paper = papers[type_count % papers.len()];
                (
                    format!("Paper: {}", paper),
                    json!({
                        "importance": (rng.gen::<f64>() * 0.2 + 0.7).round() * 100.0 / 100.0,
                        "year": rng.gen_range(1950..2023)
                    })
                )
            },
            "application" => {
                let apps = [
                    "Web Search", "Social Media", "E-commerce", 
                    "Cloud Storage", "Video Streaming", "Music Streaming",
                    "Ride Sharing", "Food Delivery", "Navigation",
                    "Email", "Messaging", "Video Conferencing",
                    "Code Repositories", "Project Management", "CRM",
                    "ERP", "Database Systems", "Operating Systems",
                ];
                let app = apps[type_count % apps.len()];
                (
                    format!("Application: {}", app),
                    json!({
                        "importance": (rng.gen::<f64>() * 0.3 + 0.6).round() * 100.0 / 100.0,
                        "users_millions": rng.gen_range(1..5000)
                    })
                )
            },
            _ => unreachable!(),
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
                ("concept", "concept") => edge_types[rng.gen_range(0..2)], // includes or related
                ("concept", "application") => "used_in",
                ("application", "concept") => "uses",
                ("person", "concept") => "contributed_to",
                ("person", "organization") => "affiliated_with",
                ("person", "paper") => "authored",
                ("paper", "concept") => "describes",
                ("paper", "paper") => "cites",
                ("organization", "application") => "developed",
                _ => edge_types[rng.gen_range(0..edge_types.len())],
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
