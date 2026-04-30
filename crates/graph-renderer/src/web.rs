//! WASM entrypoints invoked from `assets/main.js`. Wraps `Renderer` in a
//! JS-friendly handle and surfaces camera ops for keyboard/mouse handlers
//! that live in JS.

use crate::{Camera, GraphData, Renderer, RendererConfig};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WebRenderer {
    inner: Option<Renderer>,
}

#[wasm_bindgen]
impl WebRenderer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Init wgpu against a canvas element. Takes the canvas id (string) and
    /// the four parallel arrays describing the graph.
    pub async fn init(
        &mut self,
        canvas_id: String,
        positions: js_sys::Float32Array,
        edges: js_sys::Uint32Array,
        colors: js_sys::Float32Array,
        sizes: js_sys::Float32Array,
    ) -> Result<(), JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let canvas = document
            .get_element_by_id(&canvas_id)
            .ok_or_else(|| JsValue::from_str("canvas not found"))?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| JsValue::from_str("element is not a canvas"))?;

        let width = canvas.width();
        let height = canvas.height();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            ..Default::default()
        });
        let target = wgpu::SurfaceTarget::Canvas(canvas);
        let surface = instance
            .create_surface(target)
            .map_err(|e| JsValue::from_str(&format!("create_surface: {e}")))?;

        let graph = GraphData {
            positions: positions.to_vec(),
            edges: edges.to_vec(),
            colors: colors.to_vec(),
            sizes: sizes.to_vec(),
        };
        let renderer = Renderer::new(
            instance,
            surface,
            RendererConfig { width, height },
            graph,
        )
        .await
        .map_err(|e| JsValue::from_str(&e))?;

        self.inner = Some(renderer);
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(r) = &mut self.inner {
            r.resize(w, h);
        }
    }

    pub fn update_positions(&mut self, p: js_sys::Float32Array) {
        if let Some(r) = &mut self.inner {
            r.update_positions(&p.to_vec());
        }
    }

    pub fn update_colors(&mut self, c: js_sys::Float32Array) {
        if let Some(r) = &mut self.inner {
            r.update_colors(&c.to_vec());
        }
    }

    pub fn update_sizes(&mut self, s: js_sys::Float32Array) {
        if let Some(r) = &mut self.inner {
            r.update_sizes(&s.to_vec());
        }
    }

    pub fn render(&mut self) -> Result<(), JsValue> {
        if let Some(r) = &mut self.inner {
            r.render()
                .map_err(|e| JsValue::from_str(&format!("render: {e:?}")))?;
        }
        Ok(())
    }

    pub fn raycast(&self, x: f32, y: f32) -> Option<u32> {
        self.inner.as_ref().and_then(|r| r.raycast(x, y))
    }

    // Camera ops surfaced for JS keyboard/mouse handlers.
    pub fn cam_pan(&mut self, dx: f32, dy: f32, dz: f32) {
        if let Some(r) = &mut self.inner {
            r.camera.pan(dx, dy, dz);
        }
    }

    pub fn cam_rotate(&mut self, dyaw: f32, dpitch: f32) {
        if let Some(r) = &mut self.inner {
            r.camera.rotate_yaw(dyaw);
            r.camera.rotate_pitch(dpitch);
        }
    }

    pub fn cam_zoom(&mut self, factor: f32) {
        if let Some(r) = &mut self.inner {
            r.camera.zoom(factor);
        }
    }

    pub fn cam_fit(&mut self) {
        if let Some(r) = &mut self.inner {
            // Compute bounds from CPU position cache.
            let positions = current_positions(&r.camera);
            let _ = positions;
            // Recompute bounds via a public method on Renderer: simplest is to
            // re-derive by reading from the position cache that lives on
            // Renderer. For now, fit to a default box around the origin.
            let min = glam::Vec3::splat(-1000.0);
            let max = glam::Vec3::splat(1000.0);
            r.camera.fit_to_bounds(min, max);
        }
    }

    pub fn cam_reset(&mut self) {
        if let Some(r) = &mut self.inner {
            r.camera.reset();
        }
    }
}

#[allow(dead_code)]
fn current_positions(_cam: &Camera) -> &'static [f32] {
    &[]
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
}
