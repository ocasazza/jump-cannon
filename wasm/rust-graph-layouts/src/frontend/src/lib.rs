mod app;
mod app_graph;
mod utils;
mod ui;
mod renderer;
mod layout;
mod file_parser;

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub fn start(canvas_id: &str) -> Result<(), JsValue> {
        // Initialize logger for WebAssembly
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        
        // Start the web app
        let web_options = eframe::WebOptions::default();
        
        // Clone the canvas_id to ensure it's owned by the async block
        let canvas_id_owned = canvas_id.to_owned();
        
        wasm_bindgen_futures::spawn_local(async move {
            eframe::WebRunner::new()
                .start(
                    &canvas_id_owned,
                    web_options,
                    Box::new(|_cc| Box::new(crate::app::App::default())),
                )
                .await
                .expect("Failed to start eframe");
        });
        
        Ok(())
    }
}

// Re-export the start function for wasm
#[cfg(target_arch = "wasm32")]
pub use web::start;
