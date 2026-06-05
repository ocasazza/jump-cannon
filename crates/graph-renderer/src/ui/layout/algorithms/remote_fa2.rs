//! Renderer-side `RemoteFa2Layout` — a `PhysicsLayout` that consumes the
//! graph-api `/graph/layout/stream` WebSocket and writes the latched
//! positions buffer into the shared wgpu positions buffer each frame.
//!
//! Wire format (matches `graph-api/src/server.rs` ws_handler):
//!   `[u64 LE frame][u32 LE n_nodes][f32 LE positions; n_nodes * 3]`
//!
//! There is no compute pass — the layout is purely a "remote sink".

use std::sync::{Arc, Mutex};

use eframe::egui;
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, Graph, LayoutDescriptor, LayoutId, LayoutKind,
    LayoutRequirements, PhysicsLayout,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::layout::registry::LayoutFactory;

const LAYOUT_ID: LayoutId = "remote-fa2";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteFa2Settings {
    pub url: String,
    pub reconnect_backoff_ms: u32,
    /// The graph-compute worker engine id this generic bridge should
    /// request via the `?layout_id=` query on the stream URL. The
    /// `/graph/layout/stream` handler self-selects the worker engine
    /// per-connection from this id, so the UI never needs to PUT
    /// `/compute/layout`. `#[serde(default)]` keeps older persisted
    /// sessionStorage blobs (which predate this field) deserializing.
    #[serde(default = "default_layout_id")]
    pub layout_id: String,
}

fn default_layout_id() -> String { "fa2-bh".to_string() }

impl Default for RemoteFa2Settings {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8080/graph/layout/stream".to_string(),
            reconnect_backoff_ms: 1000,
            layout_id: default_layout_id(),
        }
    }
}

/// Shared latch — the WS consumer task drops the latest decoded positions
/// vec here. `step_with_encoder` `take()`s it and uploads to the GPU.
type Latch = Arc<Mutex<Option<Vec<f32>>>>;

pub struct RemoteFa2Layout {
    settings: RemoteFa2Settings,
    latch: Latch,
    n_nodes: u32,
    /// Set once `init_with_device` has spawned the consumer task. The
    /// task itself is detached — we never join it; reconnects loop
    /// internally. We track only whether we already spawned for this
    /// settings url so we don't spawn duplicates if `init_with_device`
    /// is called repeatedly.
    spawned_url: Option<String>,
}

impl RemoteFa2Layout {
    fn create(settings: RemoteFa2Settings) -> Self {
        Self {
            settings,
            latch: Arc::new(Mutex::new(None)),
            n_nodes: 0,
            spawned_url: None,
        }
    }
}

impl PhysicsLayout for RemoteFa2Layout {
    type Settings = RemoteFa2Settings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "Remote (compute)",
            description:
                "Stream positions from the remote graph-compute worker over WebSocket. \
                 The worker engine is requested per-connection via the stream's \
                 `?layout_id=` query.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self { Self::create(settings) }

    fn init_with_device(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        graph: &Graph,
        _positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        self.n_nodes = graph.nodes.len() as u32;

        // Only spawn one consumer per (re)used url. If the user changed
        // the url OR the engine (layout_id) via settings, allow a re-spawn
        // (the previous task will continue running its inner reconnect loop
        // pointing at the old url; this is leaky but acceptable for now —
        // settings churn is rare and the connection is small).
        //
        // The engine selection rides in the `?layout_id=` query — the
        // stream handler self-selects the worker engine from it. We fold
        // it into the spawn url so the dedup key respawns the consumer
        // whenever the picked engine changes.
        let sep = if self.settings.url.contains('?') { '&' } else { '?' };
        let url = format!(
            "{}{}layout_id={}",
            self.settings.url, sep, self.settings.layout_id
        );
        if self.spawned_url.as_deref() == Some(url.as_str()) {
            return Ok(());
        }
        self.spawned_url = Some(url.clone());

        let backoff_ms = self.settings.reconnect_backoff_ms.max(100);
        let latch = Arc::clone(&self.latch);
        spawn_ws_consumer(url, backoff_ms, latch);
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        let positions = match self.latch.lock() {
            Ok(mut g) => g.take(),
            Err(_) => return,
        };
        let Some(positions) = positions else { return };
        if positions.len() == 3 * (self.n_nodes as usize) && self.n_nodes > 0 {
            queue.write_buffer(positions_buf, 0, bytemuck::cast_slice(&positions));
        }
        // else: silently drop frames where n_nodes mismatches the local
        // topology — guards against an initial frame from a worker that
        // hasn't yet picked up the same graph load.
    }

    fn set_settings(&mut self, settings: Self::Settings) { self.settings = settings; }
    fn settings(&self) -> &Self::Settings { &self.settings }
}

// ---- Frame parsing ---------------------------------------------------------

fn parse_frame(bytes: &[u8]) -> Option<(u64, u32, Vec<f32>)> {
    if bytes.len() < 12 {
        return None;
    }
    let frame = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let n = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let body = &bytes[12..];
    if body.len() != (n as usize) * 12 {
        return None;
    }
    // bytemuck on a 4-byte-aligned slice — `body` is from a `Vec<u8>` so
    // alignment isn't guaranteed; copy via `from_le_bytes` to stay safe
    // across platforms.
    let mut out = Vec::with_capacity((n as usize) * 3);
    for chunk in body.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some((frame, n, out))
}

// ---- WS consumer task ------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_ws_consumer(url: String, backoff_ms: u32, latch: Latch) {
    // Use a dedicated background thread driving a current-thread tokio
    // runtime. We don't assume a global runtime exists — the renderer's
    // tokio usage in `fetch.rs` is per-request via reqwest's blocking
    // wrappers, not a long-lived runtime we can spawn onto.
    std::thread::Builder::new()
        .name("remote-fa2-ws".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::error!("remote_fa2: failed to build tokio runtime: {e}");
                    return;
                }
            };
            rt.block_on(ws_consumer_loop(url, backoff_ms, latch));
        })
        .ok();
}

#[cfg(not(target_arch = "wasm32"))]
async fn ws_consumer_loop(url: String, base_backoff_ms: u32, latch: Latch) {
    use futures::stream::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let mut backoff = base_backoff_ms as u64;
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((mut stream, _resp)) => {
                log::info!("remote_fa2: connected to {url}");
                backoff = base_backoff_ms as u64; // reset on success
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(Message::Binary(bytes)) => {
                            if let Some((_frame, _n, positions)) = parse_frame(&bytes) {
                                if let Ok(mut g) = latch.lock() {
                                    *g = Some(positions);
                                }
                            }
                        }
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => continue,
                    }
                }
                log::warn!("remote_fa2: stream closed; will reconnect in {backoff}ms");
            }
            Err(e) => {
                log::warn!("remote_fa2: connect {url} failed: {e}; retry in {backoff}ms");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        // Exponential backoff capped at 30s.
        backoff = (backoff.saturating_mul(2)).min(30_000);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn spawn_ws_consumer(url: String, backoff_ms: u32, latch: Latch) {
    wasm_bindgen_futures::spawn_local(async move {
        ws_consumer_loop(url, backoff_ms, latch).await;
    });
}

#[cfg(target_arch = "wasm32")]
async fn ws_consumer_loop(url: String, base_backoff_ms: u32, latch: Latch) {
    use futures::stream::StreamExt;
    use gloo_net::websocket::{futures::WebSocket, Message};

    let mut backoff = base_backoff_ms as u64;
    loop {
        match WebSocket::open(&url) {
            Ok(ws) => {
                log::info!("remote_fa2: connected to {url}");
                backoff = base_backoff_ms as u64;
                let (_sink, mut stream) = ws.split();
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(Message::Bytes(bytes)) => {
                            if let Some((_frame, _n, positions)) = parse_frame(&bytes) {
                                if let Ok(mut g) = latch.lock() {
                                    *g = Some(positions);
                                }
                            }
                        }
                        Ok(Message::Text(_)) => continue,
                        Err(_) => break,
                    }
                }
                log::warn!("remote_fa2: stream closed; will reconnect in {backoff}ms");
            }
            Err(e) => {
                log::warn!("remote_fa2: connect {url} failed: {e:?}; retry in {backoff}ms");
            }
        }
        gloo_timers::future::TimeoutFuture::new(backoff as u32).await;
        backoff = (backoff.saturating_mul(2)).min(30_000);
    }
}

// ---- Factory + UI ----------------------------------------------------------

pub fn factory() -> LayoutFactory {
    LayoutFactory::Physics {
        descriptor: <RemoteFa2Layout as PhysicsLayout>::descriptor(),
        build: build_layout,
        default_settings: default_settings_json,
        ui: render_ui,
    }
}

fn default_settings_json() -> Value {
    serde_json::to_value(RemoteFa2Settings::default()).unwrap_or(Value::Null)
}

fn build_layout(json: &Value) -> Box<dyn DynPhysicsLayout> {
    let s: RemoteFa2Settings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| RemoteFa2Settings::default());
    Box::new(BoxedPhysics::new(RemoteFa2Layout::create(s)))
}

fn render_ui(ui: &mut egui::Ui, json: &mut Value) {
    let mut s: RemoteFa2Settings =
        serde_json::from_value(json.clone()).unwrap_or_else(|_| RemoteFa2Settings::default());
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("url");
        if ui
            .add(egui::TextEdit::singleline(&mut s.url).desired_width(f32::INFINITY))
            .changed()
        {
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("reconnect ms");
        if ui
            .add(
                egui::DragValue::new(&mut s.reconnect_backoff_ms)
                    .range(100..=30_000)
                    .speed(50.0),
            )
            .changed()
        {
            changed = true;
        }
    });

    if changed {
        if let Ok(v) = serde_json::to_value(&s) {
            *json = v;
        }
    }
}
