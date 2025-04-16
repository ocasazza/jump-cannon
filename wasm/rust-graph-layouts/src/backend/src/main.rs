use axum::{
    routing::{get, post, delete},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use shared::api::{GRAPHS_PATH, LAYOUT_PATH, UPLOAD_PATH, API_BASE_PATH};
use crate::storage::InMemoryStorage;

mod storage;
mod handlers;

#[tokio::main]
async fn main() {
    // Initialize logger
    env_logger::init();
    
    // Create storage
    let storage = Arc::new(InMemoryStorage::new()) as Arc<dyn storage::GraphStorage>;
    
    // CORS configuration
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    
    // Create router
    let app = Router::new()
        // Graph routes
        .route(&format!("{}{}", API_BASE_PATH, GRAPHS_PATH), get(handlers::list_graphs))
        .route(&format!("{}{}", API_BASE_PATH, GRAPHS_PATH), post(handlers::save_graph))
        .route(&format!("{}{}/:id", API_BASE_PATH, GRAPHS_PATH), get(handlers::get_graph))
        .route(&format!("{}{}/:id", API_BASE_PATH, GRAPHS_PATH), delete(handlers::delete_graph))
        // Layout routes
        .route(&format!("{}{}", API_BASE_PATH, LAYOUT_PATH), post(handlers::apply_layout))
        // Upload routes
        .route(&format!("{}{}", API_BASE_PATH, UPLOAD_PATH), post(handlers::upload_graph_file))
        // Add CORS and state
        .layer(cors)
        .with_state(storage);
    
    // Run server
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Server listening on {}", addr);
    
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
