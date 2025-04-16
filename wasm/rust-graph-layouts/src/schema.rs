use serde::{Deserialize, Serialize};
use crate::types::{Graph, LayoutAlgorithm};
use std::collections::HashMap;

/// Schema definitions for graph data structures
/// These schemas define the contract between frontend and backend

/// Request to get a graph by ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetGraphRequest {
    pub id: String,
}

/// Request to save a graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveGraphRequest {
    pub id: String,
    pub graph: Graph,
}

/// Request to delete a graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteGraphRequest {
    pub id: String,
}

/// Request to list all available graphs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListGraphsRequest {}

/// Request to apply a layout algorithm to a graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyLayoutRequest {
    pub graph_id: String,
    pub layout: LayoutAlgorithm,
}

/// Response containing a graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResponse {
    pub graph: Graph,
}

/// Response containing a list of graph IDs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphListResponse {
    pub graph_ids: Vec<String>,
}

/// Generic success response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: Option<String>,
}

/// Error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u32,
}

/// Request to upload a graph file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadGraphFileRequest {
    pub id: String,
    pub file_content: String,
    pub file_type: GraphFileType,
}

/// Response for a successful file upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadGraphFileResponse {
    pub graph: Graph,
    pub message: String,
}

/// Supported graph file types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphFileType {
    JSON,
    CSV,
    DOT,
}
