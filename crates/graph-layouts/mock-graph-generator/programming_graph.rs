use rand::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn generate_programming_graph(node_count: usize) -> Value {
    let mut rng = rand::thread_rng();
    
    // Define node types and their proportions
    let node_types = [
        ("language", 0.2),
        ("framework", 0.2),
        ("library", 0.15),
        ("concept", 0.15),
        ("tool", 0.1),
        ("platform", 0.1),
        ("person", 0.1),
    ];
    
    // Define edge types
    let edge_types = [
        "uses", "implements", "extends", "depends_on", "created_by", 
        "runs_on", "compiles_to", "inspired", "related_to", "part_of",
    ];
    
    // Define domain-specific data
    let languages = [
        "Python", "JavaScript", "Java", "C++", "Rust", "Go", "TypeScript", 
        "C#", "Ruby", "Swift", "Kotlin", "PHP", "Scala", "Haskell", "Elixir",
    ];
    
    let frameworks = [
        "React", "Angular", "Vue", "Django", "Spring", "Flask", "Express", 
        "Rails", "ASP.NET", "Laravel", "Symfony", "FastAPI", "Next.js", "Svelte",
    ];
    
    let libraries = [
        "TensorFlow", "PyTorch", "NumPy", "Pandas", "jQuery", "Redux", 
        "Lodash", "Axios", "Requests", "SQLAlchemy", "Hibernate", "Boost",
    ];
    
    let concepts = [
        "Object-Oriented Programming", "Functional Programming", "Concurrency", 
        "Parallelism", "Asynchronous Programming", "Reactive Programming", 
        "Design Patterns", "Microservices", "Serverless", "REST", "GraphQL",
    ];
    
    let tools = [
        "Git", "Docker", "Kubernetes", "VS Code", "IntelliJ", "Jenkins", 
        "GitHub Actions", "Travis CI", "npm", "pip", "Cargo", "Maven",
    ];
    
    let platforms = [
        "AWS", "Azure", "Google Cloud", "Heroku", "Netlify", "Vercel", 
        "DigitalOcean", "Linux", "Windows", "macOS", "iOS", "Android",
    ];
    
    let people = [
        "Linus Torvalds", "Guido van Rossum", "Brendan Eich", "Anders Hejlsberg", 
        "James Gosling", "Yukihiro Matsumoto", "Bjarne Stroustrup", "Graydon Hoare", 
        "Rich Hickey", "Ryan Dahl", "DHH", "Kent Beck",
    ];
    
    // Generate nodes
    let mut nodes = Vec::new();
    let mut node_counts_by_type: HashMap<&str, usize> = HashMap::new();
    
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
            "language" => {
                let lang = languages[type_count % languages.len()];
                (
                    format!("Language: {}", lang),
                    json!({
                        "year": rng.gen_range(1970..2023),
                        "paradigm": (["imperative", "object-oriented", "functional", "procedural"])[rng.gen_range(0..4)],
                        "popularity": (rng.gen::<f64>() * 100.0).round() / 10.0
                    })
                )
            },
            "framework" => {
                let framework = frameworks[type_count % frameworks.len()];
                (
                    format!("Framework: {}", framework),
                    json!({
                        "year": rng.gen_range(2000..2023),
                        "domain": (["web", "mobile", "desktop", "backend", "frontend"])[rng.gen_range(0..4)],
                        "popularity": (rng.gen::<f64>() * 100.0).round() / 10.0
                    })
                )
            },
            "library" => {
                let library = libraries[type_count % libraries.len()];
                (
                    format!("Library: {}", library),
                    json!({
                        "year": rng.gen_range(2000..2023),
                        "purpose": (["data", "ui", "networking", "utility", "ml"])[rng.gen_range(0..4)],
                        "popularity": (rng.gen::<f64>() * 100.0).round() / 10.0
                    })
                )
            },
            "concept" => {
                let concept = concepts[type_count % concepts.len()];
                (
                    format!("Concept: {}", concept),
                    json!({
                        "complexity": (rng.gen::<f64>() * 10.0).round() / 10.0,
                        "importance": (rng.gen::<f64>() * 10.0).round() / 10.0
                    })
                )
            },
            "tool" => {
                let tool = tools[type_count % tools.len()];
                (
                    format!("Tool: {}", tool),
                    json!({
                        "year": rng.gen_range(2000..2023),
                        "category": (["version control", "ci/cd", "editor", "package manager", "container"])[rng.gen_range(0..5)],
                        "popularity": (rng.gen::<f64>() * 100.0).round() / 10.0
                    })
                )
            },
            "platform" => {
                let platform = platforms[type_count % platforms.len()];
                (
                    format!("Platform: {}", platform),
                    json!({
                        "year": rng.gen_range(1990..2023),
                        "type": (["cloud", "os", "mobile", "web"])[rng.gen_range(0..3)],
                        "market_share": (rng.gen::<f64>() * 100.0).round() / 10.0
                    })
                )
            },
            "person" => {
                let person = people[type_count % people.len()];
                (
                    format!("Person: {}", person),
                    json!({
                        "contributions": rng.gen_range(1..10),
                        "influence": (rng.gen::<f64>() * 10.0).round() / 10.0
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
                ("language", "framework") => "implemented_in",
                ("framework", "language") => "uses",
                ("library", "language") => "written_in",
                ("person", "language") => "created",
                ("person", "framework") => "developed",
                ("framework", "library") => "depends_on",
                ("tool", "language") => "supports",
                ("platform", "language") => "runs",
                ("concept", "language") => "applied_in",
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
