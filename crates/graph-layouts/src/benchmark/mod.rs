use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
#[cfg(feature = "cli")]
use chrono::Utc;
use serde_json;

use crate::types::{Graph, GraphFile};
use crate::layout::algorithms::fcose::{FcoseOptions, apply_layout};

pub struct BenchmarkResult {
    pub graph_name: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub layout_name: String,
    pub execution_time_ms: f64,
    pub average_edge_length: f64,
    pub node_distribution_score: f64,
    #[cfg(feature = "cli")]
    pub timestamp: String,
}

impl BenchmarkResult {
    pub fn to_csv_header() -> String {
        "timestamp,graph_name,layout_name,node_count,edge_count,execution_time_ms,average_edge_length,node_distribution_score\n".to_string()
    }

    #[cfg(feature = "cli")]
    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{:.2},{:.2},{:.2}\n",
            self.timestamp,
            self.graph_name,
            self.layout_name,
            self.node_count,
            self.edge_count,
            self.execution_time_ms,
            self.average_edge_length,
            self.node_distribution_score
        )
    }

    #[cfg(not(feature = "cli"))]
    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{:.2},{:.2},{:.2}\n",
            "",
            self.graph_name,
            self.layout_name,
            self.node_count,
            self.edge_count,
            self.execution_time_ms,
            self.average_edge_length,
            self.node_distribution_score
        )
    }
}

pub fn calculate_metrics(graph: &Graph) -> (f64, f64) {
    let mut total_edge_length = 0.0;
    let mut edge_count = 0;

    // Calculate average edge length
    for edge in graph.edges.values() {
        if let (Some(source_pos), Some(target_pos)) = (
            graph.nodes.get(&edge.source).and_then(|n| n.position),
            graph.nodes.get(&edge.target).and_then(|n| n.position)
        ) {
            let dx = target_pos.0 - source_pos.0;
            let dy = target_pos.1 - source_pos.1;
            total_edge_length += (dx * dx + dy * dy).sqrt();
            edge_count += 1;
        }
    }
    let average_edge_length = if edge_count > 0 {
        total_edge_length / edge_count as f64
    } else {
        0.0
    };

    // Calculate node distribution score (standard deviation of distances to center)
    let mut center_x = 0.0;
    let mut center_y = 0.0;
    let node_count = graph.nodes.len() as f64;

    for node in graph.nodes.values() {
        if let Some(pos) = node.position {
            center_x += pos.0;
            center_y += pos.1;
        }
    }
    center_x /= node_count;
    center_y /= node_count;

    let mut total_variance = 0.0;
    for node in graph.nodes.values() {
        if let Some(pos) = node.position {
            let dx = pos.0 - center_x;
            let dy = pos.1 - center_y;
            total_variance += dx * dx + dy * dy;
        }
    }
    let node_distribution_score = (total_variance / node_count).sqrt();

    (average_edge_length, node_distribution_score)
}

pub fn run_benchmark(graph_path: &str) -> Result<BenchmarkResult, String> {
    // Load graph from JSON file
    let graph_content = fs::read_to_string(graph_path)
        .map_err(|e| format!("Failed to read graph file: {}", e))?;
    
    // Parse as GraphFile first, then convert to Graph
    let graph_file: GraphFile = serde_json::from_str(&graph_content)
        .map_err(|e| format!("Failed to parse graph JSON: {}", e))?;
    let mut graph: Graph = graph_file.into();

    let graph_name = Path::new(graph_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Run layout with default options
    let options = FcoseOptions::default();
    let start_time = std::time::Instant::now();
    apply_layout(&mut graph, &options)?;
    let execution_time = start_time.elapsed();

    // Calculate metrics
    let (average_edge_length, node_distribution_score) = calculate_metrics(&graph);

    Ok(BenchmarkResult {
        graph_name,
        node_count: graph.nodes.len(),
        edge_count: graph.edges.len(),
        layout_name: "fcose".to_string(),
        execution_time_ms: execution_time.as_secs_f64() * 1000.0,
        average_edge_length,
        node_distribution_score,
        #[cfg(feature = "cli")]
        timestamp: Utc::now().to_rfc3339(),
    })
}

pub fn run_all_benchmarks(output_path: &str) -> Result<(), String> {
    let sample_dir = "docs/sample";
    let mut results = Vec::new();

    // Collect all JSON files from sample directory
    let entries = fs::read_dir(sample_dir)
        .map_err(|e| format!("Failed to read sample directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            match run_benchmark(path.to_str().unwrap()) {
                Ok(result) => results.push(result),
                Err(e) => eprintln!("Failed to benchmark {}: {}", path.display(), e),
            }
        }
    }

    // Write results to CSV
    let mut file = File::create(output_path)
        .map_err(|e| format!("Failed to create output file: {}", e))?;

    // Write header
    file.write_all(BenchmarkResult::to_csv_header().as_bytes())
        .map_err(|e| format!("Failed to write CSV header: {}", e))?;

    // Write results
    for result in results {
        file.write_all(result.to_csv_row().as_bytes())
            .map_err(|e| format!("Failed to write result row: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Node, Edge};
    use std::collections::HashMap;

    #[test]
    fn test_metrics_calculation() {
        let mut graph = Graph {
            nodes: HashMap::new(),
            edges: HashMap::new(),
        };

        // Create a simple graph with two nodes and one edge
        let mut node1 = Node::new("1");
        node1.position = Some((0.0, 0.0));
        graph.nodes.insert("1".to_string(), node1);

        let mut node2 = Node::new("2");
        node2.position = Some((100.0, 0.0));
        graph.nodes.insert("2".to_string(), node2);

        let edge = Edge::new("1-2", "1", "2");
        graph.edges.insert("1-2".to_string(), edge);

        let (avg_edge_length, distribution_score) = calculate_metrics(&graph);

        // Edge length should be 100.0
        assert!((avg_edge_length - 100.0).abs() < 0.001);

        // Distribution score should be 50.0 (standard deviation from center)
        assert!((distribution_score - 50.0).abs() < 0.001);
    }
}
