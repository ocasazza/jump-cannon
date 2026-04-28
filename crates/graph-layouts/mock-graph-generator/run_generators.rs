use std::process::Command;
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    // Create docs/sample directory if it doesn't exist
    std::fs::create_dir_all("docs/sample").expect("Failed to create docs/sample directory");
    
    // Create README.md in the bin directory
    create_readme();
    
    // Run all generators
    println!("Running all graph generators...");
    
    // JSON Graph
    run_generator("generate_json_graph", &["docs/sample/tech_knowledge_graph.json", "40"]);
    
    // CSV Graphs
    run_generator("generate_csv_graph", &["nodes", "docs/sample/node_list.csv", "30"]);
    run_generator("generate_csv_graph", &["edges", "docs/sample/edge_list.csv", "45"]);
    
    // DOT Graph
    run_generator("generate_dot_graph", &["docs/sample/knowledge_graph.dot", "25", "directed"]);
    run_generator("generate_dot_graph", &["docs/sample/undirected_graph.dot", "20", "undirected"]);
    
    // Layout Graphs
    run_generator("generate_layout_graph", &["docs/sample/fcose_layout.json", "fcose", "30"]);
    run_generator("generate_layout_graph", &["docs/sample/dagre_layout.json", "dagre", "25"]);
    run_generator("generate_layout_graph", &["docs/sample/concentric_layout.json", "concentric", "20"]);
    
    // Domain Graphs
    run_generator("generate_domain_graph", &["programming", "docs/sample/programming_graph.json", "35"]);
    run_generator("generate_domain_graph", &["science", "docs/sample/science_graph.json", "30"]);
    run_generator("generate_domain_graph", &["business", "docs/sample/business_graph.json", "25"]);
    run_generator("generate_medicine_graph", &["docs/sample/medicine_graph.json", "30"]);
    
    // Large Graph (smaller version for sample)
    run_generator("generate_large_graph", &["docs/sample/medium_graph.json", "200", "0.05"]);
    
    println!("All generators completed successfully!");
    println!("Sample graphs have been created in the docs/sample directory.");
    println!("See bin/README.md for usage instructions for each generator.");
}

fn run_generator(name: &str, args: &[&str]) {
    println!("Running {}...", name);
    
    let status = Command::new("cargo")
        .args(["run", "--bin", name])
        .args(args)
        .status()
        .expect(&format!("Failed to execute {}", name));
    
    if !status.success() {
        eprintln!("Warning: {} exited with non-zero status", name);
    }
}

fn create_readme() {
    let readme_path = Path::new("bin/README.md");
    let mut file = File::create(readme_path).expect("Failed to create README.md");
    
    let content = r#"# Knowledge Graph Generators

This directory contains Rust scripts for generating various types of graph data for testing and development purposes.

## Prerequisites

These scripts require Rust and Cargo to be installed. They also depend on the following crates:
- `rand`: For random data generation
- `serde_json`: For JSON serialization/deserialization
- `csv`: For CSV file handling

Add these dependencies to your Cargo.toml:

```toml
[dependencies]
rand = "0.8"
serde_json = "1.0"
csv = "1.1"
```

## Available Generators

### JSON Graph Generator

Generates a knowledge graph in JSON format with nodes and edges.

```bash
cargo run --bin generate_json_graph [output_path] [node_count]
```

- `output_path`: Path to save the generated JSON file (default: "docs/sample/generated_graph.json")
- `node_count`: Number of nodes to generate (default: 50)

### CSV Graph Generator

Generates either a node list or edge list in CSV format.

```bash
cargo run --bin generate_csv_graph [format] [output_path] [count]
```

- `format`: Either "nodes" or "edges" (default: "nodes")
- `output_path`: Path to save the generated CSV file (default depends on format)
- `count`: Number of nodes or edges to generate (default: 50)

### DOT Graph Generator

Generates a graph in DOT format for use with Graphviz.

```bash
cargo run --bin generate_dot_graph [output_path] [node_count] [is_directed]
```

- `output_path`: Path to save the generated DOT file (default: "docs/sample/generated_graph.dot")
- `node_count`: Number of nodes to generate (default: 30)
- `is_directed`: "directed" for a directed graph, anything else for undirected (default: directed)

### Layout Graph Generator

Generates a graph with layout-specific structure and options.

```bash
cargo run --bin generate_layout_graph [output_path] [layout_type] [node_count]
```

- `output_path`: Path to save the generated JSON file (default: "docs/sample/layout_graph.json")
- `layout_type`: One of "fcose", "cose-bilkent", "cise", "concentric", "klay", "dagre" (default: "fcose")
- `node_count`: Number of nodes to generate (default: 50)

### Domain Graph Generator

Generates a domain-specific knowledge graph.

```bash
cargo run --bin generate_domain_graph [domain] [output_path] [node_count]
```

- `domain`: One of "programming", "science", "business", "medicine" (default: "programming")
- `output_path`: Path to save the generated JSON file (default: "docs/sample/{domain}_graph.json")
- `node_count`: Number of nodes to generate (default: 50)

### Large Graph Generator

Generates a large graph for performance testing.

```bash
cargo run --bin generate_large_graph [output_path] [node_count] [edge_density]
```

- `output_path`: Path to save the generated JSON file (default: "docs/sample/large_graph.json")
- `node_count`: Number of nodes to generate (default: 1000)
- `edge_density`: Edge density between 0.0 and 1.0 (default: 0.01)

### Run All Generators

Run all generators with predefined parameters to create a set of sample graphs.

```bash
cargo run --bin run_generators
```

This will create various sample graphs in the docs/sample directory.

## Output Formats

### JSON Format

The JSON format consists of a root object with "nodes" and "edges" arrays:

```json
{
  "nodes": [
    {
      "id": "n1",
      "label": "Node 1",
      "x": 100,
      "y": 100,
      "type": "concept",
      "additional_field1": "value1",
      ...
    },
    ...
  ],
  "edges": [
    {
      "source": "n1",
      "target": "n2",
      "type": "relates_to",
      "weight": 0.8,
      ...
    },
    ...
  ]
}
```

### CSV Format

#### Node List CSV

```
id,label,x,y,type,importance,description,...
n1,Node 1,100,200,concept,0.8,"Description of node 1",...
n2,Node 2,150,250,person,0.7,"Description of node 2",...
...
```

#### Edge List CSV

```
id,source,target,type,weight,label
e1,n1,n2,relates_to,0.8,"Relates to"
e2,n2,n3,depends_on,0.6,"Depends on"
...
```

### DOT Format

```dot
digraph KnowledgeGraph {
  // Graph attributes
  graph [rankdir=LR, splines=true, overlap=false];
  node [shape=box, style="rounded,filled", fontname=Arial];
  edge [fontname=Arial];

  // Nodes
  n1 [label="Node 1", fillcolor=lightblue, shape=ellipse];
  n2 [label="Node 2", fillcolor=lightgreen, shape=box];
  
  // Edges
  n1 -> n2 [label="relates_to", weight=8];
  n2 -> n3 [label="depends_on", weight=6];
}
```
"#;
    
    file.write_all(content.as_bytes()).expect("Failed to write to README.md");
    println!("Created bin/README.md with usage instructions");
}
