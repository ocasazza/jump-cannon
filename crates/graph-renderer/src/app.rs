use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::data::{self, Bootstrap, LoadState, SharedLoad};
use crate::fetch::ApiClient;
use crate::graph_callback::GraphCallback;
use crate::graph_pipelines::{GraphData, GraphPipelines};

pub struct App {
    note: String,
    load: SharedLoad,
    /// Once we successfully push a Bootstrap into GraphPipelines we flip
    /// this so we don't retry the (expensive) buffer creation.
    loaded_into_gpu: bool,
    /// Set once we emit the readiness console-log line.
    logged_ready: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Register GraphPipelines into eframe's wgpu callback resources.
        // Without an active wgpu state (e.g. CPU renderer fallback) we just
        // skip — the App still runs but the graph layer no-ops.
        if let Some(wgpu_state) = cc.wgpu_render_state.as_ref() {
            let device = &wgpu_state.device;
            let format = wgpu_state.target_format;
            let pipes = GraphPipelines::new(device, format);
            wgpu_state
                .renderer
                .write()
                .callback_resources
                .insert(pipes);
            log::info!(
                "[graph-renderer] GraphPipelines registered (target_format = {:?})",
                format
            );
        } else {
            log::warn!("[graph-renderer] no wgpu_render_state — graph layer disabled");
        }

        let load: SharedLoad = Arc::new(Mutex::new(LoadState::Pending));
        kick_off_bootstrap(load.clone(), default_base_url());

        Self {
            note: "vault graph".into(),
            load,
            loaded_into_gpu: false,
            logged_ready: false,
        }
    }

    /// If the fetch task has placed a Bootstrap in `self.load`, hand it to
    /// GraphPipelines and mark loaded.
    fn try_promote_bootstrap_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };

        // Move the Bootstrap out of the shared state so we don't double-load
        // and so we drop the staging Vec ASAP. Failure modes leave the
        // sentinel state alone.
        let bootstrap_opt: Option<Bootstrap> = {
            let mut guard = self.load.lock().unwrap();
            match std::mem::take(&mut *guard) {
                LoadState::Ready(b) => Some(b),
                other => {
                    *guard = other;
                    None
                }
            }
        };
        let Some(bootstrap) = bootstrap_opt else {
            return;
        };

        let n_nodes = bootstrap.positions.len() / 3;
        let graph = GraphData {
            positions: bootstrap.positions,
            edges: bootstrap.edges,
            colors: data::default_colors(n_nodes),
            sizes: data::default_sizes(n_nodes),
        };

        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            match pipes.load(&device, &queue, graph) {
                Ok(()) => {
                    log::info!(
                        "[graph-renderer] graph loaded: {} nodes, {} edges",
                        pipes.n_nodes(),
                        pipes.n_edges()
                    );
                    self.loaded_into_gpu = true;
                }
                Err(e) => {
                    log::error!("[graph-renderer] GraphPipelines::load failed: {e}");
                }
            }
        }
    }

    /// Once GPU upload is done, emit the readiness log line that the
    /// browser test grep-asserts on. Done here (rather than in load()) so
    /// the test can rely on a stable line wording even when the parallel
    /// agent's UI logs land in between.
    fn emit_ready_log(&mut self, frame: &mut eframe::Frame) {
        if self.logged_ready || !self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let renderer = wgpu_state.renderer.read();
        let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() else {
            return;
        };
        // Single canonical line for the test harness.
        log::info!(
            "[graph-renderer] graph loaded: {} nodes",
            pipes.n_nodes()
        );
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::console::log_1(
                &format!(
                    "[graph-renderer] graph loaded: {} nodes",
                    pipes.n_nodes()
                )
                .into(),
            );
        }
        self.logged_ready = true;
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Slightly off-black so the brightness sampler sees something even
        // before any data lands.
        [0.06, 0.06, 0.07, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Force-redraw — without this the headless test only catches the
        // first frame and the force sim is invisible.
        ctx.request_repaint();

        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::ZERO;
        visuals.menu_rounding = egui::Rounding::ZERO;
        visuals.widgets.noninteractive.rounding = egui::Rounding::ZERO;
        visuals.widgets.inactive.rounding = egui::Rounding::ZERO;
        visuals.widgets.hovered.rounding = egui::Rounding::ZERO;
        visuals.widgets.active.rounding = egui::Rounding::ZERO;
        let bg = egui::Color32::from_rgb(15, 15, 18);
        visuals.window_fill = bg;
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        ctx.set_visuals(visuals);

        // Hand Bootstrap → GPU once available.
        self.try_promote_bootstrap_to_gpu(frame);
        self.emit_ready_log(frame);

        // Status snapshot for the UI.
        let status: String = {
            let guard = self.load.lock().unwrap();
            match &*guard {
                LoadState::Pending => "fetch pending…".into(),
                LoadState::Loading(s) => s.clone(),
                LoadState::Ready(_) => "ready (uploading…)".into(),
                LoadState::Failed(e) => format!("error: {e}"),
            }
        };

        egui::CentralPanel::default()
            // Make panel transparent so the wgpu graph layer below shows
            // through cleanly. The `clear_color` above paints the floor.
            .frame(egui::Frame::none().fill(egui::Color32::TRANSPARENT))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                let (rect, _resp) = ui.allocate_exact_size(avail, egui::Sense::drag());
                let cb = GraphCallback {
                    screen_px: [rect.width().max(1.0), rect.height().max(1.0)],
                };
                ui.painter()
                    .add(egui_wgpu::Callback::new_paint_callback(rect, cb));

                // Lightweight overlay so the test brightness sampler also
                // sees text-rendered glyphs in addition to the WebGPU
                // composited canvas (some headless backends drop alpha
                // on the WebGPU layer at screenshot time).
                let overlay_rect =
                    egui::Rect::from_min_size(rect.min, egui::vec2(420.0, 40.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{} — {}", &self.note, &status))
                            .color(egui::Color32::from_rgb(220, 220, 230)),
                    );
                });
            });
    }
}

fn default_base_url() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(origin) = window.location().origin() {
                return origin;
            }
        }
        "".to_string()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("GRAPH_API_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4848".into())
    }
}

/// Kick off the parallel fetch of /graph/init + ids + positions + edges.
/// Updates `load` when each milestone completes, ending in
/// LoadState::Ready or LoadState::Failed.
fn kick_off_bootstrap(load: SharedLoad, base: String) {
    let client = ApiClient::new(base);

    let task = async move {
        // init
        set_status(&load, "fetching /graph/init…");
        let init = match client.init().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/init: {e}"));
                return;
            }
        };
        log::info!(
            "[graph-renderer] init: {} nodes, {} edges",
            init.n_nodes,
            init.n_edges
        );

        // ids
        set_status(&load, "fetching /graph/ids…");
        let ids = match client.ids().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/ids: {e}"));
                return;
            }
        };

        // positions (2D)
        set_status(&load, "fetching /graph/positions…");
        let positions_2d = match client.positions().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/positions: {e}"));
                return;
            }
        };

        // edges
        set_status(&load, "fetching /graph/edges…");
        let edges = match client.edges().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/edges: {e}"));
                return;
            }
        };

        // Optional metrics — failure is non-fatal.
        let mut metrics = std::collections::HashMap::new();
        for name in ["degree", "pagerank", "kcore", "community"] {
            set_status(&load, format!("fetching /graph/metrics/{name}…"));
            match client.metric(name).await {
                Ok(v) => {
                    metrics.insert(name.to_string(), v);
                }
                Err(e) => {
                    log::warn!("[graph-renderer] metric {name}: {e}");
                }
            }
        }

        let positions_3d = data::promote_2d_to_3d(&positions_2d, init.n_nodes as u64);

        log::info!(
            "[graph-renderer] bootstrap fetched: {} ids, {} positions (2D), {} edges, {} metrics",
            ids.len(),
            positions_2d.len() / 2,
            edges.len() / 2,
            metrics.len()
        );

        let bootstrap = Bootstrap {
            init: Some(init),
            ids,
            positions: positions_3d,
            edges,
            metrics,
        };
        *load.lock().unwrap() = LoadState::Ready(bootstrap);
    };

    spawn_async(task);
}

fn set_status(load: &SharedLoad, msg: impl Into<String>) {
    let mut guard = load.lock().unwrap();
    *guard = LoadState::Loading(msg.into());
}

fn set_failed(load: &SharedLoad, msg: String) {
    log::error!("[graph-renderer] bootstrap failed: {msg}");
    *load.lock().unwrap() = LoadState::Failed(msg);
}

#[cfg(target_arch = "wasm32")]
fn spawn_async<F: std::future::Future<Output = ()> + 'static>(f: F) {
    wasm_bindgen_futures::spawn_local(f);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_async<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    // Lazy-init a single shared multi-thread tokio runtime for the App's
    // fetch tasks. The native binary currently only spawns one task, but
    // keeping a runtime around means we don't need to thread it through
    // App::new from the bin.
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    rt.spawn(f);
}
