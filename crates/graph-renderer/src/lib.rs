//! Rust + wgpu renderer. Compiles native (winit + Vulkan/Metal) and WASM
//! (web-sys + WebGPU). The native binary is `graph-renderer-native`; the
//! WASM bundle is loaded by `assets/main.js` and renders into the page's
//! <canvas>.
//
// Future: shares the wgpu device with graph-layouts so the compute output
// (positions buffer) is read by the vertex shader without a CPU copy.

pub mod camera;
pub mod input;
pub mod renderer;

#[cfg(feature = "wasm")]
pub mod web;

// Re-exports useful for both native and WASM.
pub use camera::Camera;
pub use renderer::{GraphData, Renderer, RendererConfig};

// The static asset bundle is still embedded so graph-api can serve
// index.html / main.js / style.css / pkg/* without reading from disk.
use include_dir::{include_dir, Dir};
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");
pub fn assets() -> &'static Dir<'static> {
    &ASSETS
}
