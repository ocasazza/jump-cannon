use rand::prelude::*;
use std::fs::File;
use std::io::{Write, BufWriter};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("docs/sample/generated_graph.dot");
    let node_count = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(30);
    let is_directed = args.get(3).map(|s| s == "directed").unwrap_or(true);
    
    println!("Generating {} DOT graph with {} nodes to {}", 
             if is_directed { "directed" } else { "undirected" },
             node_count,
             output_path);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let file = File::create(output_path).expect("Failed to create output file");
    let mut writer = BufWriter::new(file);
    
    generate_dot_graph(&mut writer, node_count, is_directed);
    
    println!("DOT graph generated successfully!");
}

fn generate_dot_graph(writer: &mut BufWriter<File>, node_count: usize, is_directed: bool) {
    let mut rng = rand::thread_rng();
    
    // Write DOT header
    if is_directed {
        writeln!(writer, "digraph KnowledgeGraph {{").expect("Failed to write header");
    } else {
        writeln!(writer, "graph KnowledgeGraph {{").expect("Failed to write header");
    }
    
    // Write graph attributes
    writeln!(writer, "  // Graph attributes").expect("Failed to write comment");
    writeln!(writer, "  graph [rankdir=LR, splines=true, overlap=false, nodesep=0.8, ranksep=1.0];").expect("Failed to write graph attributes");
    writeln!(writer, "  node [shape=box, style=\"rounded,filled\", fontname=Arial, fontsize=12];").expect("Failed to write node attributes");
    writeln!(writer, "  edge [fontname=Arial, fontsize=10];").expect("Failed to write edge attributes");
    writeln!(writer, "").expect("Failed to write newline");
    
    // Define node categories and their attributes
    let node_categories = [
        ("concept", "lightblue", "ellipse"),
        ("person", "lightgreen", "box"),
        ("resource", "lightyellow", "folder"),
        ("tool", "lightcoral", "component"),
        ("process", "lavender", "diamond"),
    ];
    
    // Generate nodes
    writeln!(writer, "  // Nodes").expect("Failed to write comment");
    
    let mut node_types = Vec::with_capacity(node_count);
    
    for i in 1..=node_count {
        let node_id = format!("n{}", i);
        
        // Select random category
        let category_idx = rng.gen_range(0..node_categories.len());
        let (category, color, shape) = node_categories[category_idx];
        node_types.push(category);
        
        // Generate label based on category
        let label = match category {
            "concept" => {
                let concepts = [
                    "Data Structure", "Algorithm", "Design Pattern", 
                    "Architecture", "Paradigm", "Framework", 
                    "Protocol", "Standard", "Methodology"
                ];
                let adjectives = [
                    "Distributed", "Concurrent", "Parallel", 
                    "Functional", "Object-Oriented", "Reactive", 
                    "Asynchronous", "Event-Driven", "Declarative"
                ];
                format!("{} {}", 
                    adjectives[rng.gen_range(0..adjectives.len())],
                    concepts[rng.gen_range(0..concepts.len())]
                )
            },
            "person" => {
                let roles = [
                    "Developer", "Architect", "Designer", 
                    "Engineer", "Researcher", "Analyst", 
                    "Manager", "Consultant", "Specialist"
                ];
                let areas = [
                    "Software", "Systems", "Data", 
                    "Network", "Security", "Cloud", 
                    "Web", "Mobile", "AI"
                ];
                format!("{} {}", 
                    areas[rng.gen_range(0..areas.len())],
                    roles[rng.gen_range(0..roles.len())]
                )
            },
            "resource" => {
                let resource_types = [
                    "Database", "Repository", "Library", 
                    "API", "Service", "Platform", 
                    "SDK", "Toolkit", "Framework"
                ];
                let domains = [
                    "Development", "Testing", "Deployment", 
                    "Monitoring", "Analytics", "Integration", 
                    "Security", "Management", "Automation"
                ];
                format!("{} {}", 
                    domains[rng.gen_range(0..domains.len())],
                    resource_types[rng.gen_range(0..resource_types.len())]
                )
            },
            "tool" => {
                let tool_types = [
                    "Compiler", "Debugger", "Profiler", 
                    "Editor", "IDE", "Version Control", 
                    "Build System", "Package Manager", "Container"
                ];
                format!("{} Tool", tool_types[rng.gen_range(0..tool_types.len())])
            },
            "process" => {
                let processes = [
                    "Development", "Testing", "Deployment", 
                    "Integration", "Review", "Planning", 
                    "Monitoring", "Maintenance", "Optimization"
                ];
                let methodologies = [
                    "Agile", "DevOps", "CI/CD", 
                    "TDD", "BDD", "Lean", 
                    "Kanban", "Scrum", "Waterfall"
                ];
                format!("{} {}", 
                    methodologies[rng.gen_range(0..methodologies.len())],
                    processes[rng.gen_range(0..processes.len())]
                )
            },
            _ => format!("Node {}", i),
        };
        
        // Write node with attributes
        writeln!(
            writer,
            "  {} [label=\"{}\", fillcolor={}, shape={}, tooltip=\"{} node\"];",
            node_id, label, color, shape, category
        ).expect("Failed to write node");
    }
    
    writeln!(writer, "").expect("Failed to write newline");
    
    // Generate edges
    writeln!(writer, "  // Edges").expect("Failed to write comment");
    
    // Define edge types based on node categories
    let edge_types = [
        (("concept", "concept"), ["includes", "relates_to", "extends"]),
        (("concept", "person"), ["understood_by", "developed_by", "taught_by"]),
        (("concept", "resource"), ["implemented_in", "stored_in", "accessed_through"]),
        (("concept", "tool"), ["supported_by", "analyzed_with", "built_with"]),
        (("concept", "process"), ["applied_in", "part_of", "guides"]),
        
        (("person", "concept"), ["understands", "develops", "teaches"]),
        (("person", "person"), ["collaborates_with", "mentors", "reports_to"]),
        (("person", "resource"), ["uses", "maintains", "creates"]),
        (("person", "tool"), ["operates", "configures", "builds"]),
        (("person", "process"), ["follows", "improves", "manages"]),
        
        (("resource", "concept"), ["implements", "stores", "provides_access_to"]),
        (("resource", "person"), ["used_by", "maintained_by", "created_by"]),
        (("resource", "resource"), ["connects_to", "depends_on", "integrates_with"]),
        (("resource", "tool"), ["accessed_by", "managed_by", "deployed_with"]),
        (("resource", "process"), ["supports", "enables", "constrains"]),
        
        (("tool", "concept"), ["supports", "analyzes", "builds"]),
        (("tool", "person"), ["operated_by", "configured_by", "built_by"]),
        (("tool", "resource"), ["accesses", "manages", "deploys"]),
        (("tool", "tool"), ["works_with", "extends", "replaces"]),
        (("tool", "process"), ["facilitates", "automates", "monitors"]),
        
        (("process", "concept"), ["applies", "contains", "guided_by"]),
        (("process", "person"), ["followed_by", "improved_by", "managed_by"]),
        (("process", "resource"), ["supported_by", "enabled_by", "constrained_by"]),
        (("process", "tool"), ["facilitated_by", "automated_by", "monitored_by"]),
        (("process", "process"), ["precedes", "includes", "alternates_with"]),
    ];
    
    // Generate a reasonable number of edges
    let edge_count = node_count * 2; // Average of 2 edges per node
    
    for _ in 0..edge_count {
        // Select random source and target nodes
        let source_idx = rng.gen_range(0..node_count);
        let mut target_idx = rng.gen_range(0..node_count);
        
        // Avoid self-loops
        while target_idx == source_idx {
            target_idx = rng.gen_range(0..node_count);
        }
        
        let source_id = format!("n{}", source_idx + 1);
        let target_id = format!("n{}", target_idx + 1);
        
        let source_type = node_types[source_idx];
        let target_type = node_types[target_idx];
        
        // Find appropriate edge types for this node pair
        let relation = match edge_types.iter()
            .find(|((s, t), _)| *s == source_type && *t == target_type) {
                Some((_, relations)) => {
                    let idx = rng.gen_range(0..relations.len());
                    relations[idx]
                },
                None => "connects_to"
            };
        
        // Generate edge weight (1-10)
        let weight = rng.gen_range(1..=10);
        
        // Write edge with attributes
        if is_directed {
            writeln!(
                writer,
                "  {} -> {} [label=\"{}\", weight={}, tooltip=\"{} ({} weight)\"];",
                source_id, target_id, relation, weight, relation, weight
            ).expect("Failed to write edge");
        } else {
            writeln!(
                writer,
                "  {} -- {} [label=\"{}\", weight={}, tooltip=\"{} ({} weight)\"];",
                source_id, target_id, relation, weight, relation, weight
            ).expect("Failed to write edge");
        }
    }
    
    // Close graph
    writeln!(writer, "}}").expect("Failed to write closing brace");
}
