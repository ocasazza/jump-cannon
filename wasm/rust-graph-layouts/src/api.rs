use serde::{Deserialize, Serialize};
use crate::schema::{
    GetGraphRequest, SaveGraphRequest, DeleteGraphRequest, ListGraphsRequest,
    ApplyLayoutRequest, GraphResponse as GraphResponseData, GraphListResponse, SuccessResponse, ErrorResponse,
    UploadGraphFileRequest, UploadGraphFileResponse, GraphFileType
};

/// API endpoints
pub const API_BASE_PATH: &str = "/api";
pub const GRAPHS_PATH: &str = "/graphs";
pub const LAYOUT_PATH: &str = "/layout";
pub const UPLOAD_PATH: &str = "/upload";

/// API routes
pub const GET_GRAPH_ROUTE: &str = "/api/graphs/:id";
pub const SAVE_GRAPH_ROUTE: &str = "/api/graphs";
pub const DELETE_GRAPH_ROUTE: &str = "/api/graphs/:id";
pub const LIST_GRAPHS_ROUTE: &str = "/api/graphs";
pub const APPLY_LAYOUT_ROUTE: &str = "/api/layout";
pub const UPLOAD_GRAPH_FILE_ROUTE: &str = "/api/upload";

/// API command enum for frontend to backend communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphCommand {
    GetGraph(GetGraphRequest),
    SaveGraph(SaveGraphRequest),
    DeleteGraph(DeleteGraphRequest),
    ListGraphs(ListGraphsRequest),
    ApplyLayout(ApplyLayoutRequest),
    UploadGraphFile(UploadGraphFileRequest),
}

/// API response enum for backend to frontend communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphResponse {
    Graph(GraphResponseData),
    GraphList(GraphListResponse),
    Success(SuccessResponse),
    Error(ErrorResponse),
    UploadSuccess(UploadGraphFileResponse),
}

/// Error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorCode {
    NotFound = 404,
    BadRequest = 400,
    InternalError = 500,
}

impl From<ErrorCode> for u32 {
    fn from(code: ErrorCode) -> Self {
        code as u32
    }
}
