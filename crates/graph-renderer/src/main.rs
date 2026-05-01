// Entrypoint built by trunk for `wasm32-unknown-unknown` and by cargo for
// the host. On wasm32 the actual work happens after this `main` returns —
// `#[wasm_bindgen(start)]` annotations in the imported library trigger
// after the wasm module is initialized.
//
// Phase B re-integrates the existing wgpu Renderer (still on disk in
// src/renderer.rs) via an egui_wgpu callback; for now this just opens an
// eframe window with a placeholder UI.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();
    let opts = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default().with_title("vault graph"),
        ..Default::default()
    };
    eframe::run_native(
        "vault graph",
        opts,
        Box::new(|cc| Ok(Box::new(graph_renderer::App::new(cc)))),
    )
}

// On wasm32 trunk builds this `[[bin]]` target as the wasm artifact, so we
// drive the WebRunner from `main` directly. wasm-bindgen's `start` shim
// exists in the lib too but only fires for crates pulled in as cdylib —
// the bin produces a wasm with a regular `_start`, so doing it here is
// the simplest path.
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;

    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);
    web_sys::console::log_1(&"[graph-renderer] wasm main fired".into());

    wasm_bindgen_futures::spawn_local(async move {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas: web_sys::HtmlCanvasElement = document
            .create_element("canvas")
            .unwrap()
            .dyn_into()
            .unwrap();
        canvas.set_id("graph-canvas");
        canvas.set_width(1200);
        canvas.set_height(800);
        let style = canvas.style();
        let _ = style.set_property("display", "block");
        let _ = style.set_property("width", "100vw");
        let _ = style.set_property("height", "100vh");
        document.body().unwrap().append_child(&canvas).unwrap();

        log::info!("[graph-renderer] eframe WebRunner starting");

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(graph_renderer::App::new(cc)))),
            )
            .await
            .expect("eframe start");
    });
}
