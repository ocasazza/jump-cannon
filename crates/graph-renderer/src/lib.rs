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
pub mod generate;
pub mod graph_callback;
pub mod graph_pipelines;
pub mod job;
pub mod perf;
pub mod proto;
pub mod timeline;
pub mod ui;
/// Local Web Worker client for the `LocalWorker` generate backend (wasm-only).
#[cfg(target_arch = "wasm32")]
pub mod worker;
pub use app::App;

/// Test-support re-exports. Doc-hidden, not part of the stable public
/// API — they exist so the headless `tests/regressions.rs` harness can
/// mount the GENUINE promoted-node body (`render_node_body`) rather than
/// a hand-copied mirror that could silently drift from production.
#[doc(hidden)]
pub mod test_support {
    pub use crate::app::{render_node_body, AnchoredChannels};
}

// Static asset bundle is still embedded so graph-api can serve the
// trunk-built dist/ when run with embedded assets.
use include_dir::{include_dir, Dir};
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");
pub fn assets() -> &'static Dir<'static> {
    &ASSETS
}
