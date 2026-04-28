use rand::prelude::*;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("docs/sample/layout_graph.json");
    let layout_type = args.get(2).map(|s| s.as_str()).unwrap_or("fcose");
    let node_count = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    
    println!("Generating graph with {} nodes and {} layout to {}", 
             node_count, layout_type, output_path);
    
    let graph = generate_layout_graph(node_count, layout_type);
    
    // Create output directory if it doesn't exist
    if let Some(parent) = Path::new(output_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create output directory");
    }
    
    let mut file = File::create(output_path).expect("Failed to create output file");
    let json_string = serde_json::to_string_pretty(&graph).expect("Failed to serialize graph");
    file.write_all(json_string.as_bytes()).expect("Failed to write to file");
    
    println!("Graph generated successfully!");
}

fn generate_layout_graph(node_count: usize, layout_type: &str) -> Value {
    let mut rng = rand::thread_rng();
    
    // Generate nodes
    let mut nodes = Vec::new();
    
    // Create nodes with different shapes and sizes for layout testing
    for i in 1..=node_count {
        let size = rng.gen_range(10..50) as f64;
        let shape = match rng.gen_range(0..5) {
            0 => "ellipse",
            1 => "rectangle",
            2 => "triangle",
            3 => "diamond",
            _ => "hexagon",
        };
        
        // For some layouts, we'll add position hints
        let (x, y) = if layout_type == "preset" {
            (Some(rng.gen_range(0.0..1000.0)), Some(rng.gen_range(0.0..1000.0)))
        } else {
            (None, None)
        };
        
        // Create node with random attributes
        let mut node = json!({
            "id": format!("n{}", i),
            "label": format!("Node {}", i),
            "size": size,
            "shape": shape,
            "group": rng.gen_range(1..6)
        });
        
        // Add position if needed
        if let (Some(x_val), Some(y_val)) = (x, y) {
            if let Value::Object(ref mut map) = node {
                map.insert("x".to_string(), json!(x_val));
                map.insert("y".to_string(), json!(y_val));
            }
        }
        
        nodes.push(node);
    }
    
    // Generate edges with different patterns based on layout type
    let mut edges = Vec::new();
    
    match layout_type {
        "dagre" | "klay" => {
            // For hierarchical layouts, create a more tree-like structure
            for i in 1..node_count {
                let source_id = format!("n{}", (i / 3) + 1); // Each node connects to ~3 children
                let target_id = format!("n{}", i + 1);
                
                if source_id != target_id {
                    edges.push(json!({
                        "source": source_id,
                        "target": target_id,
                        "weight": rng.gen_range(1..10)
                    }));
                }
            }
        },
        "cise" => {
            // For cluster layouts, create distinct clusters
            let clusters = 5;
            let nodes_per_cluster = node_count / clusters;
            
            // Create intra-cluster edges (dense connections within clusters)
            for c in 0..clusters {
                let start = c * nodes_per_cluster + 1;
                let end = (c + 1) * nodes_per_cluster;
                
                for i in start..=end {
                    for j in i+1..=end {
                        if rng.gen_bool(0.7) { // 70% chance of connection within cluster
                            edges.push(json!({
                                "source": format!("n{}", i),
                                "target": format!("n{}", j),
                                "weight": rng.gen_range(5..10) // Stronger weights within clusters
                            }));
                        }
                    }
                }
            }
            
            // Create inter-cluster edges (sparse connections between clusters)
            for _ in 0..(clusters * 2) {
                let cluster1 = rng.gen_range(0..clusters);
                let cluster2 = rng.gen_range(0..clusters);
                
                if cluster1 != cluster2 {
                    let node1 = cluster1 * nodes_per_cluster + rng.gen_range(1..=nodes_per_cluster);
                    let node2 = cluster2 * nodes_per_cluster + rng.gen_range(1..=nodes_per_cluster);
                    
                    edges.push(json!({
                        "source": format!("n{}", node1),
                        "target": format!("n{}", node2),
                        "weight": rng.gen_range(1..3) // Weaker weights between clusters
                    }));
                }
            }
        },
        "concentric" => {
            // For concentric layouts, create a hub-and-spoke pattern
            // Central nodes (hubs)
            let hub_count = node_count / 10;
            for i in 1..=hub_count {
                let hub = format!("n{}", i);
                
                // Connect to many other nodes
                for j in hub_count+1..=node_count {
                    if rng.gen_bool(0.3) { // 30% chance of connection
                        edges.push(json!({
                            "source": hub,
                            "target": format!("n{}", j),
                            "weight": rng.gen_range(1..10)
                        }));
                    }
                }
            }
            
            // Add some connections between non-hub nodes
            for _ in 0..(node_count / 5) {
                let node1 = rng.gen_range(hub_count+1..=node_count);
                let node2 = rng.gen_range(hub_count+1..=node_count);
                
                if node1 != node2 {
                    edges.push(json!({
                        "source": format!("n{}", node1),
                        "target": format!("n{}", node2),
                        "weight": rng.gen_range(1..5)
                    }));
                }
            }
        },
        _ => {
            // For force-directed layouts (fcose, cose-bilkent), create a more random structure
            // but with some community structure
            
            // Create communities
            let communities = 3;
            let nodes_per_community = node_count / communities;
            
            // Create intra-community edges
            for c in 0..communities {
                let start = c * nodes_per_community + 1;
                let end = (c + 1) * nodes_per_community;
                
                for i in start..=end {
                    for j in i+1..=end {
                        if rng.gen_bool(0.3) { // 30% chance of connection within community
                            edges.push(json!({
                                "source": format!("n{}", i),
                                "target": format!("n{}", j),
                                "weight": rng.gen_range(1..10)
                            }));
                        }
                    }
                }
            }
            
            // Create inter-community edges
            for _ in 0..(node_count / 2) {
                let node1 = rng.gen_range(1..=node_count);
                let node2 = rng.gen_range(1..=node_count);
                
                if node1 != node2 {
                    edges.push(json!({
                        "source": format!("n{}", node1),
                        "target": format!("n{}", node2),
                        "weight": rng.gen_range(1..5)
                    }));
                }
            }
        }
    }
    
    // Create layout options based on the layout type
    let layout_options = match layout_type {
        "fcose" => json!({
            "name": "fcose",
            "quality": "default",
            "nodeRepulsion": 4500,
            "idealEdgeLength": 50,
            "nodeOverlap": 10
        }),
        "cose-bilkent" => json!({
            "name": "cose-bilkent",
            "nodeRepulsion": 4500,
            "nodeOverlap": 10,
            "idealEdgeLength": 50
        }),
        "cise" => json!({
            "name": "cise",
            "clusters": [], // Would be populated with actual cluster data
            "circleSpacing": 20,
            "nodeSpacing": 10
        }),
        "concentric" => json!({
            "name": "concentric",
            "minNodeSpacing": 10,
            "concentricBy": "degree",
            "levelWidth": 100
        }),
        "klay" => json!({
            "name": "klay",
            "layerSpacing": 50,
            "nodeSpacing": 20,
            "nodePlacement": "BRANDES_KOEPF",
            "crossMinimization": "LAYER_SWEEP",
            "cycleBreaking": "GREEDY",
            "edgeRouting": "ORTHOGONAL",
            "mergeEdges": false
        }),
        "dagre" => json!({
            "name": "dagre",
            "nodeSeparation": 50,
            "rankSeparation": 50,
            "rankDirection": "TB",
            "align": "UL",
            "acyclic": true,
            "ranker": "network-simplex"
        }),
        _ => json!({
            "name": "fcose", // Default to fcose
            "quality": "default",
            "nodeRepulsion": 4500,
            "idealEdgeLength": 50,
            "nodeOverlap": 10
        }),
    };
    
    json!({
        "nodes": nodes,
        "edges": edges,
        "layout": layout_options
    })
}
