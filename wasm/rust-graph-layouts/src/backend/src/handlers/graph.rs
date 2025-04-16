use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use shared::{
    schema::{
        SaveGraphRequest, ApplyLayoutRequest, GraphResponse, GraphListResponse, 
        SuccessResponse, ErrorResponse, UploadGraphFileRequest, UploadGraphFileResponse,
    },
    types::{Graph, LayoutAlgorithm},
};
use std::sync::Arc;
use crate::storage::GraphStorage;
use crate::handlers::file_parser;

/// Handler for getting a graph by ID
pub async fn get_graph(
    Path(id): Path<String>,
    State(storage): State<Arc<dyn GraphStorage>>,
) -> impl IntoResponse {
    match storage.get_graph(&id).await {
        Ok(graph) => (
            StatusCode::OK,
            Json(GraphResponse { graph }),
        ).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
                code: 404,
            }),
        ).into_response(),
    }
}

/// Handler for saving a graph
pub async fn save_graph(
    State(storage): State<Arc<dyn GraphStorage>>,
    Json(request): Json<SaveGraphRequest>,
) -> impl IntoResponse {
    match storage.save_graph(&request.id, &request.graph).await {
        Ok(_) => (
            StatusCode::OK,
            Json(SuccessResponse {
                success: true,
                message: Some(format!("Graph '{}' saved successfully", request.id)),
            }),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
                code: 500,
            }),
        ).into_response(),
    }
}

/// Handler for deleting a graph
pub async fn delete_graph(
    Path(id): Path<String>,
    State(storage): State<Arc<dyn GraphStorage>>,
) -> impl IntoResponse {
    match storage.delete_graph(&id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(SuccessResponse {
                success: true,
                message: Some(format!("Graph '{}' deleted successfully", id)),
            }),
        ).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
                code: 404,
            }),
        ).into_response(),
    }
}

/// Handler for listing all graphs
pub async fn list_graphs(
    State(storage): State<Arc<dyn GraphStorage>>,
) -> impl IntoResponse {
    match storage.list_graphs().await {
        Ok(graph_ids) => (
            StatusCode::OK,
            Json(GraphListResponse { graph_ids }),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
                code: 500,
            }),
        ).into_response(),
    }
}

/// Handler for applying a layout algorithm to a graph
pub async fn apply_layout(
    State(storage): State<Arc<dyn GraphStorage>>,
    Json(request): Json<ApplyLayoutRequest>,
) -> impl IntoResponse {
    // Get the graph
    let graph_result = storage.get_graph(&request.graph_id).await;
    
    match graph_result {
        Ok(mut graph) => {
            // Apply the layout algorithm
            let result = apply_layout_algorithm(&mut graph, &request.layout);
            
            match result {
                Ok(_) => {
                    // Save the updated graph
                    match storage.save_graph(&request.graph_id, &graph).await {
                        Ok(_) => (
                            StatusCode::OK,
                            Json(GraphResponse { graph }),
                        ).into_response(),
                        Err(e) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(ErrorResponse {
                                error: e.to_string(),
                                code: 500,
                            }),
                        ).into_response(),
                    }
                }
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: e,
                        code: 400,
                    }),
                ).into_response(),
            }
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
                code: 404,
            }),
        ).into_response(),
    }
}

/// Handler for uploading a graph file
pub async fn upload_graph_file(
    State(storage): State<Arc<dyn GraphStorage>>,
    Json(request): Json<UploadGraphFileRequest>,
) -> impl IntoResponse {
    // Parse the file content based on the file type
    match file_parser::parse_graph_file(&request.file_content, &request.file_type) {
        Ok(graph) => {
            // Save the parsed graph
            match storage.save_graph(&request.id, &graph).await {
                Ok(_) => (
                    StatusCode::OK,
                    Json(UploadGraphFileResponse {
                        graph,
                        message: format!("Graph '{}' uploaded and parsed successfully", request.id),
                    }),
                ).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: e.to_string(),
                        code: 500,
                    }),
                ).into_response(),
            }
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: e,
                code: 400,
            }),
        ).into_response(),
    }
}

/// Apply a layout algorithm to a graph
/// This is a placeholder for the actual layout algorithm implementation
fn apply_layout_algorithm(graph: &mut Graph, layout: &LayoutAlgorithm) -> Result<(), String> {
    // This is where we would implement the actual layout algorithms
    // For now, we'll just set random positions for the nodes
    
    match layout {
        LayoutAlgorithm::Fcose(_) |
        LayoutAlgorithm::CoseBilkent(_) |
        LayoutAlgorithm::Cise(_) |
        LayoutAlgorithm::Concentric(_) |
        LayoutAlgorithm::KlayLayered(_) |
        LayoutAlgorithm::Dagre(_) => {
            // For now, just set random positions for all layouts
            // In a real implementation, each layout would have its own algorithm
            for node in graph.nodes.values_mut() {
                node.position = Some((
                    rand::random::<f64>() * 1000.0,
                    rand::random::<f64>() * 1000.0,
                ));
            }
            Ok(())
        }
    }
}
