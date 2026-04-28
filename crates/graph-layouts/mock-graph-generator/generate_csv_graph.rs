mod programming_graph;

use rand::prelude::*;
use std::fs::File;
use std::io::{Write, BufWriter};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_format = args.get(1).map(|s| s.as_str()).unwrap_or("nodes");
    let output_path = args.get(2).map(|s| s.as_str()).unwrap_or(
        if output_format == "nodes" {
            "docs/sample/generated_nodes.csv"
        } else {
            "docs/sample/generated_edges.csv"
        }
    );
    let count = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    
    println!("Generating CSV {} with {} entries to {}", 
             if output_format == "nodes" { "node list" } else { "edge list" },
             count,
             output_path);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let file = File::create(output_path).expect("Failed to create output file");
    let mut writer = BufWriter::new(file);
    
    match output_format {
        "nodes" => generate_node_list(&mut writer, count),
        "edges" => generate_edge_list(&mut writer, count),
        _ => {
            eprintln!("Invalid format: {}. Use 'nodes' or 'edges'.", output_format);
            std::process::exit(1);
        }
    }
    
    println!("CSV file generated successfully!");
}

fn generate_node_list(writer: &mut BufWriter<File>, count: usize) {
    let mut rng = rand::thread_rng();
    
    // Write header
    writeln!(writer, "id,label,x,y,type,importance,description,created_date").expect("Failed to write header");
    
    // Node types
    let node_types = ["person", "project", "technology", "company", "event"];
    
    for i in 1..=count {
        let id = format!("n{}", i);
        let node_type = node_types[rng.gen_range(0..node_types.len())];
        
        // Generate label based on type
        let label = match node_type {
            "person" => {
                let first_names = ["John", "Jane", "Alice", "Bob", "Charlie", "Diana", "Edward", "Fiona"];
                let last_names = ["Smith", "Johnson", "Williams", "Brown", "Jones", "Miller", "Davis", "Garcia"];
                format!("{} {}", 
                    first_names[rng.gen_range(0..first_names.len())],
                    last_names[rng.gen_range(0..last_names.len())]
                )
            },
            "project" => {
                let prefixes = ["Project", "Operation", "Initiative", "Plan", "Program"];
                let suffixes = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Omega", "Phoenix", "Horizon"];
                format!("{} {}", 
                    prefixes[rng.gen_range(0..prefixes.len())],
                    suffixes[rng.gen_range(0..suffixes.len())]
                )
            },
            "technology" => {
                let techs = [
                    "Blockchain", "AI", "Machine Learning", "Cloud Computing", 
                    "IoT", "5G", "Quantum Computing", "AR/VR", "Robotics"
                ];
                techs[rng.gen_range(0..techs.len())].to_string()
            },
            "company" => {
                let prefixes = ["Tech", "Global", "Advanced", "Next", "Future", "Smart", "Cyber", "Digital"];
                let suffixes = ["Systems", "Solutions", "Technologies", "Innovations", "Dynamics", "Networks"];
                format!("{}{}", 
                    prefixes[rng.gen_range(0..prefixes.len())],
                    suffixes[rng.gen_range(0..suffixes.len())]
                )
            },
            "event" => {
                let prefixes = ["Annual", "Global", "International", "Tech", "Innovation"];
                let suffixes = ["Conference", "Summit", "Symposium", "Workshop", "Hackathon", "Meetup"];
                format!("{} {}", 
                    prefixes[rng.gen_range(0..prefixes.len())],
                    suffixes[rng.gen_range(0..suffixes.len())]
                )
            },
            _ => format!("Node {}", i),
        };
        
        // Generate random position
        let x = rng.gen_range(0.0..1000.0);
        let y = rng.gen_range(0.0..1000.0);
        
        // Generate importance score
        let importance = (rng.gen::<f64>() * 0.5 + 0.5).round() * 100.0 / 100.0;
        
        // Generate description
        let description = match node_type {
            "person" => format!("A professional in the field of {}", 
                ["technology", "science", "business", "research", "education"][rng.gen_range(0..5)]),
            "project" => format!("A {} project focused on {}", 
                ["research", "development", "innovation", "experimental"][rng.gen_range(0..4)],
                ["sustainability", "efficiency", "growth", "transformation"][rng.gen_range(0..4)]),
            "technology" => format!("A {} technology for {}", 
                ["emerging", "established", "cutting-edge", "revolutionary"][rng.gen_range(0..4)],
                ["businesses", "consumers", "industries", "researchers"][rng.gen_range(0..4)]),
            "company" => format!("A company specializing in {}", 
                ["software", "hardware", "services", "consulting", "research"][rng.gen_range(0..5)]),
            "event" => format!("An event held in {} focusing on {}", 
                ["2023", "2024", "2025", "annually"][rng.gen_range(0..4)],
                ["innovation", "networking", "education", "collaboration"][rng.gen_range(0..4)]),
            _ => "".to_string(),
        };
        
        // Generate creation date (between 2010 and 2025)
        let year = rng.gen_range(2010..=2025);
        let month = rng.gen_range(1..=12);
        let day = rng.gen_range(1..=28);
        let created_date = format!("{:04}-{:02}-{:02}", year, month, day);
        
        // Write CSV row, escaping any commas in the description
        writeln!(
            writer,
            "{},{},{},{},{},{},\"{}\",{}",
            id, label, x, y, node_type, importance, description, created_date
        ).expect("Failed to write node");
    }
}

fn generate_edge_list(writer: &mut BufWriter<File>, count: usize) {
    let mut rng = rand::thread_rng();
    
    // Write header
    writeln!(writer, "id,source,target,type,weight,label").expect("Failed to write header");
    
    // Calculate how many nodes we need
    let node_count = (count as f64 / 1.5).ceil() as usize; // Assuming ~1.5 edges per node
    
    // Edge types with descriptions
    let edge_types = [
        ("knows", "Person knows person"),
        ("works_with", "Person works with person"),
        ("uses", "Person/Company uses technology"),
        ("develops", "Person/Company develops technology/project"),
        ("attends", "Person attends event"),
        ("organizes", "Company organizes event"),
        ("funds", "Company funds project"),
        ("leads", "Person leads project"),
        ("partners_with", "Company partners with company"),
        ("employs", "Company employs person"),
    ];
    
    for i in 1..=count {
        let id = format!("e{}", i);
        
        // Generate random source and target nodes
        let source = format!("n{}", rng.gen_range(1..=node_count));
        
        // Avoid self-loops
        let mut target_id = rng.gen_range(1..=node_count);
        while format!("n{}", target_id) == source {
            target_id = rng.gen_range(1..=node_count);
        }
        let target = format!("n{}", target_id);
        
        // Choose random edge type
        let (edge_type, label) = edge_types[rng.gen_range(0..edge_types.len())];
        
        // Generate random weight
        let weight = (rng.gen::<f64>() * 0.8 + 0.2).round() * 100.0 / 100.0;
        
        // Write CSV row
        writeln!(
            writer,
            "{},{},{},{},{},\"{}\"",
            id, source, target, edge_type, weight, label
        ).expect("Failed to write edge");
    }
}
