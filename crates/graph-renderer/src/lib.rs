//! Rust + eframe + wgpu graph viewer (native + WASM via trunk).
//!
//! Phase B+C wiring:
//!
//!   - `graph_pipelines` owns wgpu state for the graph layer (no surface;
//!     eframe owns that).
//!   - `graph_callback` adapts it to `egui_wgpu::CallbackTrait` so it
//!     records into eframe's render pass.
//!   - `fetch` + `proto` + `data` form the async data path: the App fires
//!     a bootstrap fetch on `App::new`, the response promotes 2D server
//!     positions into 3D, and the next `update()` hands them to the GPU.
//!
//! The bin target (`src/main.rs`) is what trunk builds for wasm32 and
//! cargo for the host.

mod app;
pub mod camera;
pub mod data;
pub mod fetch;
pub mod graph_callback;
pub mod graph_pipelines;
pub mod proto;

pub use app::App;

// Static asset bundle is still embedded so graph-api can serve the
// trunk-built dist/ when run with embedded assets.
use include_dir::{include_dir, Dir};
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");
pub fn assets() -> &'static Dir<'static> {
    &ASSETS
}
