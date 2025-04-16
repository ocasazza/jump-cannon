use shared::{
    schema::GraphFileType,
    types::{Graph, Node, Edge},
};
use std::collections::{HashMap, HashSet};

/// Parse a graph file based on its type
pub fn parse_graph_file(content: &str, file_type: &GraphFileType) -> Result<Graph, String> {
    match file_type {
        GraphFileType::JSON => parse_json_graph(content),
        GraphFileType::CSV => parse_csv_graph(content),
        GraphFileType::DOT => parse_dot_graph(content),
    }
}

/// Parse a JSON graph file
fn parse_json_graph(content: &str) -> Result<Graph, String> {
    // Try to parse as a complete Graph structure first
    match serde_json::from_str::<Graph>(content) {
        Ok(graph) => Ok(graph),
        Err(_) => {
            // If that fails, try to parse as a nodes/edges structure
            #[derive(serde::Deserialize)]
            struct GraphData {
                nodes: Vec<NodeData>,
                edges: Vec<EdgeData>,
            }
            
            #[derive(serde::Deserialize)]
            struct NodeData {
                id: String,
                #[serde(default)]
                label: Option<String>,
                #[serde(default)]
                x: Option<f64>,
                #[serde(default)]
                y: Option<f64>,
                #[serde(flatten)]
                extra: HashMap<String, serde_json::Value>,
            }
            
            #[derive(serde::Deserialize)]
            struct EdgeData {
                id: Option<String>,
                source: String,
                target: String,
                #[serde(flatten)]
                extra: HashMap<String, serde_json::Value>,
            }
            
            match serde_json::from_str::<GraphData>(content) {
                Ok(data) => {
                    let mut graph = Graph::new();
                    
                    // Add nodes
                    for node_data in data.nodes {
                        let mut node = Node::new(node_data.id);
                        
                        // Set position if available
                        if let (Some(x), Some(y)) = (node_data.x, node_data.y) {
                            node.position = Some((x, y));
                        }
                        
                        // Add label as metadata if available
                        if let Some(label) = node_data.label {
                            node = node.with_metadata("label", label);
                        }
                        
                        // Add any extra fields as metadata
                        for (key, value) in node_data.extra {
                            if let Ok(value_str) = serde_json::to_string(&value) {
                                node = node.with_metadata(key, value_str);
                            }
                        }
                        
                        graph.add_node(node);
                    }
                    
                    // Add edges
                    for (i, edge_data) in data.edges.iter().enumerate() {
                        let edge_id = edge_data.id.clone().unwrap_or_else(|| format!("e{}", i));
                        let mut edge = Edge::new(edge_id, edge_data.source.clone(), edge_data.target.clone());
                        
                        // Add any extra fields as metadata
                        for (key, value) in &edge_data.extra {
                            if let Ok(value_str) = serde_json::to_string(value) {
                                edge = edge.with_metadata(key.clone(), value_str);
                            }
                        }
                        
                        graph.add_edge(edge);
                    }
                    
                    Ok(graph)
                },
                Err(e) => Err(format!("Failed to parse JSON graph: {}", e)),
            }
        }
    }
}

/// Parse a CSV graph file
fn parse_csv_graph(content: &str) -> Result<Graph, String> {
    let mut graph = Graph::new();
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    
    // First, try to determine if this is a node list or an edge list
    let headers = reader.headers()
        .map_err(|e| format!("Failed to read CSV headers: {}", e))?;
    
    if headers.iter().any(|h| h == "source" || h == "target") {
        // This is likely an edge list
        parse_csv_edge_list(&mut graph, content)
    } else {
        // This is likely a node list
        parse_csv_node_list(&mut graph, content)
    }
}

/// Parse a CSV file as a node list
fn parse_csv_node_list(graph: &mut Graph, content: &str) -> Result<Graph, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    
    let headers = reader.headers()
        .map_err(|e| format!("Failed to read CSV headers: {}", e))?
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<String>>();
    
    // Find the ID column index
    let id_index = headers.iter().position(|h| h.to_lowercase() == "id")
        .ok_or_else(|| "CSV must have an 'id' column".to_string())?;
    
    // Find optional position columns
    let x_index = headers.iter().position(|h| h.to_lowercase() == "x");
    let y_index = headers.iter().position(|h| h.to_lowercase() == "y");
    
    for result in reader.records() {
        let record = result.map_err(|e| format!("Failed to read CSV record: {}", e))?;
        
        if record.len() <= id_index {
            return Err(format!("CSV record has fewer fields than expected: {}", record.len()));
        }
        
        let id = record[id_index].to_string();
        let mut node = Node::new(id);
        
        // Set position if available
        if let (Some(x_idx), Some(y_idx)) = (x_index, y_index) {
            if record.len() > x_idx && record.len() > y_idx {
                if let (Ok(x), Ok(y)) = (record[x_idx].parse::<f64>(), record[y_idx].parse::<f64>()) {
                    node.position = Some((x, y));
                }
            }
        }
        
        // Add all other columns as metadata
        for (i, header) in headers.iter().enumerate() {
            if i != id_index && i != x_index.unwrap_or(usize::MAX) && i != y_index.unwrap_or(usize::MAX) {
                if i < record.len() {
                    node = node.with_metadata(header.clone(), record[i].to_string());
                }
            }
        }
        
        graph.add_node(node);
    }
    
    Ok(graph.clone())
}

/// Parse a CSV file as an edge list
fn parse_csv_edge_list(graph: &mut Graph, content: &str) -> Result<Graph, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    
    let headers = reader.headers()
        .map_err(|e| format!("Failed to read CSV headers: {}", e))?
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<String>>();
    
    // Find required columns
    let source_index = headers.iter().position(|h| h.to_lowercase() == "source")
        .ok_or_else(|| "CSV must have a 'source' column".to_string())?;
    let target_index = headers.iter().position(|h| h.to_lowercase() == "target")
        .ok_or_else(|| "CSV must have a 'target' column".to_string())?;
    
    // Find optional ID column
    let id_index = headers.iter().position(|h| h.to_lowercase() == "id");
    
    // Track nodes we've seen
    let mut node_ids = HashSet::new();
    
    for (i, result) in reader.records().enumerate() {
        let record = result.map_err(|e| format!("Failed to read CSV record: {}", e))?;
        
        if record.len() <= source_index || record.len() <= target_index {
            return Err(format!("CSV record has fewer fields than expected: {}", record.len()));
        }
        
        let source = record[source_index].to_string();
        let target = record[target_index].to_string();
        
        // Add nodes if they don't exist
        if !node_ids.contains(&source) {
            graph.add_node(Node::new(source.clone()));
            node_ids.insert(source.clone());
        }
        
        if !node_ids.contains(&target) {
            graph.add_node(Node::new(target.clone()));
            node_ids.insert(target.clone());
        }
        
        // Create edge
        let edge_id = if let Some(idx) = id_index {
            if record.len() > idx {
                record[idx].to_string()
            } else {
                format!("e{}", i)
            }
        } else {
            format!("e{}", i)
        };
        
        let mut edge = Edge::new(edge_id, source, target);
        
        // Add all other columns as metadata
        for (i, header) in headers.iter().enumerate() {
            if i != source_index && i != target_index && i != id_index.unwrap_or(usize::MAX) {
                if i < record.len() {
                    edge = edge.with_metadata(header.clone(), record[i].to_string());
                }
            }
        }
        
        graph.add_edge(edge);
    }
    
    Ok(graph.clone())
}

/// Parse a DOT graph file
fn parse_dot_graph(content: &str) -> Result<Graph, String> {
    let mut graph = Graph::new();
    let mut lines = content.lines();
    let mut node_ids = HashSet::new();
    
    // Skip until we find the graph definition
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with("digraph") || line.starts_with("graph") {
            break;
        }
    }
    
    // Parse the graph content
    for line in lines {
        let line = line.trim();
        
        // Skip comments and empty lines
        if line.is_empty() || line.starts_with("//") || line.starts_with("#") {
            continue;
        }
        
        // Skip graph attributes and closing brace
        if line.starts_with("}") || (line.contains("=") && !line.contains("->") && !line.contains("--")) {
            continue;
        }
        
        // Check if this is an edge definition
        if line.contains("->") || line.contains("--") {
            // This is an edge
            let parts: Vec<&str> = if line.contains("->") {
                line.split("->").collect()
            } else {
                line.split("--").collect()
            };
            
            if parts.len() < 2 {
                continue;
            }
            
            let source = parts[0].trim().trim_matches('"').to_string();
            let mut target_parts = parts[1].trim().split(';').collect::<Vec<&str>>();
            let target_with_attrs = target_parts.remove(0);
            
            // Extract target and attributes
            let target_parts: Vec<&str> = target_with_attrs.split('[').collect();
            let target = target_parts[0].trim().trim_matches('"').to_string();
            
            // Add nodes if they don't exist
            if !node_ids.contains(&source) {
                graph.add_node(Node::new(source.clone()));
                node_ids.insert(source.clone());
            }
            
            if !node_ids.contains(&target) {
                graph.add_node(Node::new(target.clone()));
                node_ids.insert(target.clone());
            }
            
            // Create edge
            let edge_id = format!("e{}_{}", source, target);
            let edge = Edge::new(edge_id, source, target);
            graph.add_edge(edge);
        } else if !line.contains("->") && !line.contains("--") && line.contains("[") {
            // This is a node with attributes
            let parts: Vec<&str> = line.split('[').collect();
            if parts.len() < 1 {
                continue;
            }
            
            let node_id = parts[0].trim().trim_matches('"').to_string();
            
            if !node_ids.contains(&node_id) {
                graph.add_node(Node::new(node_id.clone()));
                node_ids.insert(node_id);
            }
        } else if !line.contains("[") && !line.contains("]") && !line.contains("->") && !line.contains("--") {
            // This is a simple node
            let node_id = line.trim().trim_matches('"').trim_end_matches(';').to_string();
            
            if !node_id.is_empty() && !node_ids.contains(&node_id) {
                graph.add_node(Node::new(node_id.clone()));
                node_ids.insert(node_id);
            }
        }
    }
    
    Ok(graph)
}
