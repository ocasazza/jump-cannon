// Entrypoint built by trunk for `wasm32-unknown-unknown` and by cargo for
// the host. On wasm32 the actual work happens after this `main` returns —
// `#[wasm_bindgen(start)]` annotations in the imported library trigger
// after the wasm module is initialized.
//
// Phase B+C wires the wgpu graph layer into eframe via egui_wgpu callbacks
// (see `graph_pipelines` + `graph_callback`). The compute side
// (graph-layouts::GpuForceLayout) needs `max_storage_buffers_per_shader_stage`
// >= 9, which is above the WebGPU downlevel default of 8 — we override
// eframe's wgpu_options device_descriptor below to request the bump.

use std::sync::Arc;

/// Build a `wgpu::DeviceDescriptor` that bumps the storage-buffer limit
/// to the value the GpuForceLayout compute shader needs (9). Falls back
/// to the adapter's max if it's lower than what we request.
fn device_descriptor_factory(
) -> Arc<dyn Fn(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> + Send + Sync> {
    Arc::new(|adapter: &wgpu::Adapter| {
        let adapter_limits = adapter.limits();
        // Mirror the original standalone Renderer's limit derivation:
        // downlevel_defaults (which supports compute) raised by the adapter,
        // with max_storage_buffers_per_shader_stage bumped to 10 so the
        // compute shader's 9-buffer binding doesn't trip validation.
        let mut limits =
            wgpu::Limits::downlevel_defaults().using_resolution(adapter_limits.clone());
        limits.max_storage_buffers_per_shader_stage = limits
            .max_storage_buffers_per_shader_stage
            .max(10)
            .min(adapter_limits.max_storage_buffers_per_shader_stage);

        wgpu::DeviceDescriptor {
            label: Some("graph-renderer device (raised limits)"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::default(),
        }
    })
}

fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew {
            supported_backends: wgpu::Backends::PRIMARY | wgpu::Backends::BROWSER_WEBGPU,
            power_preference: wgpu::PowerPreference::HighPerformance,
            device_descriptor: device_descriptor_factory(),
        },
        ..Default::default()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();
    let opts = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default().with_title("vault graph"),
        wgpu_options: wgpu_options(),
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

        let web_options = eframe::WebOptions {
            wgpu_options: wgpu_options(),
            ..Default::default()
        };
        eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(graph_renderer::App::new(cc)))),
            )
            .await
            .expect("eframe start");
    });
}
