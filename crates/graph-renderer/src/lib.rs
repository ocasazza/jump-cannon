//! eframe + wgpu placeholder. Phase B will wire the existing wgpu `Renderer`
//! (still present in src/renderer.rs but not referenced) into an egui_wgpu
//! callback. For Phase A this crate only exposes a placeholder `App`.
//!
//! The actual wasm entrypoint lives in the `[[bin]]` target (`src/main.rs`)
//! since trunk drives the wasm build via `--bin graph-renderer`.

mod app;
pub mod ui;
pub use app::App;

// The static asset bundle is still embedded so graph-api can serve
// the trunk-built dist/ when run with embedded assets. After running
// `trunk build` once, `assets/dist/` is populated and gets baked into the
// graph-api binary at compile time.
use include_dir::{include_dir, Dir};
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");
pub fn assets() -> &'static Dir<'static> {
    &ASSETS
}
