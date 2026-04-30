//! WASM entrypoints invoked from `assets/main.js`. Wraps `Renderer` in a
//! JS-friendly handle and surfaces:
//!
//! - camera ops (pan / rotate / zoom / fit-to-bounds / reset)
//! - the live force sim driver (`init_layout` / `update_layout_options` /
//!   `step` / `set_cursor_force`)
//! - microscope focus plane (`set_focus_plane`)
//! - cursor world projection helper (`cursor_world_at`) so JS can map
//!   screen + depth → world for the 6DoF cursor force tool.

use crate::{GraphData, Renderer, RendererConfig};
use graph_layouts::GpuForceOptions;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WebRenderer {
    inner: Option<Renderer>,
    /// Mirror of the current sim options so `set_cursor_force` can mutate
    /// just the cursor fields without the JS side having to pass the full
    /// JSON every move.
    sim_opts: GpuForceOptions,
}

#[wasm_bindgen]
impl WebRenderer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: None,
            sim_opts: GpuForceOptions::default(),
        }
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

    /// Initialize the GPU force-sim layout against the renderer's device +
    /// shared positions buffer. `options_json` is the JSON-encoded
    /// `GpuForceOptions` shape.
    pub fn init_layout(
        &mut self,
        edges: js_sys::Uint32Array,
        options_json: String,
    ) -> Result<(), JsValue> {
        let opts: GpuForceOptions = serde_json::from_str(&options_json)
            .map_err(|e| JsValue::from_str(&format!("parse options: {e}")))?;
        self.sim_opts = opts.clone();
        let edges_vec = edges.to_vec();
        let r = self
            .inner
            .as_mut()
            .ok_or_else(|| JsValue::from_str("renderer not initialised"))?;
        r.init_layout(&edges_vec, opts)
            .map_err(|e| JsValue::from_str(&e))
    }

    pub fn update_layout_options(&mut self, options_json: String) -> Result<(), JsValue> {
        let opts: GpuForceOptions = serde_json::from_str(&options_json)
            .map_err(|e| JsValue::from_str(&format!("parse options: {e}")))?;
        self.sim_opts = opts.clone();
        if let Some(r) = self.inner.as_mut() {
            r.update_layout_options(opts);
        }
        Ok(())
    }

    /// Step the layout sim and render one frame.
    pub fn step(&mut self) -> Result<(), JsValue> {
        if let Some(r) = &mut self.inner {
            r.step()
                .map_err(|e| JsValue::from_str(&format!("step: {e:?}")))?;
        }
        Ok(())
    }

    /// Render one frame WITHOUT stepping the layout. Useful for warm-up
    /// rendering before the layout is initialised. (`step()` already covers
    /// the both-with-layout case.)
    pub fn render(&mut self) -> Result<(), JsValue> {
        if let Some(r) = &mut self.inner {
            r.step()
                .map_err(|e| JsValue::from_str(&format!("render: {e:?}")))?;
        }
        Ok(())
    }

    /// Microscope focus plane controls. `z` is world-space; `thickness` is
    /// the full focus band width. Outside ±thickness the alpha falls to
    /// ~15%.
    pub fn set_focus_plane(&mut self, z: f32, thickness: f32) {
        if let Some(r) = &mut self.inner {
            r.set_focus_plane(z, thickness);
        }
    }

    /// DoF tuning: CoC pixels per world-unit-out-of-focus and the hard
    /// cap on the bokeh disc.
    pub fn set_dof_params(&mut self, blur_strength: f32, max_coc: f32) {
        if let Some(r) = &mut self.inner {
            r.set_dof_params(blur_strength, max_coc);
        }
    }

    /// 6DoF cursor force. Pass radius=0 to disable. Strength sign
    /// convention: positive = repel, negative = attract.
    pub fn set_cursor_force(&mut self, x: f32, y: f32, z: f32, radius: f32, strength: f32) {
        self.sim_opts.cursor_pos = [x, y, z];
        self.sim_opts.cursor_radius = radius;
        self.sim_opts.cursor_strength = strength;
        if let Some(r) = &mut self.inner {
            r.update_layout_options(self.sim_opts.clone());
        }
    }

    /// Project a screen-space point + a camera-space depth into world
    /// coordinates. JS uses this to compute where the 6DoF cursor force
    /// should live: `screen_x/y` in NDC ([-1, 1], y up), `depth` is the
    /// distance forward from the camera.
    pub fn cursor_world_at(&self, ndc_x: f32, ndc_y: f32, depth: f32) -> Vec<f32> {
        let Some(r) = &self.inner else {
            return vec![0.0, 0.0, 0.0];
        };
        let (origin, dir) = r.camera.raycast(ndc_x, ndc_y);
        let p = origin + dir * depth.max(1.0);
        vec![p.x, p.y, p.z]
    }

    pub fn raycast(&self, x: f32, y: f32) -> Option<u32> {
        self.inner.as_ref().and_then(|r| r.raycast(x, y))
    }

    /// True once the GPU-force layout has settled and is auto-halted.
    /// JS uses this to flip the stats display ("settled") and to skip
    /// per-frame work that's only meaningful when the sim is moving.
    pub fn sim_halted(&self) -> bool {
        self.inner.as_ref().map(|r| r.sim_halted()).unwrap_or(false)
    }

    /// Wake the sim back up. Call from JS whenever something perturbs the
    /// graph (cursor force engaged, slider dragged, preset switched).
    pub fn sim_wake(&mut self) {
        if let Some(r) = self.inner.as_mut() {
            r.sim_wake();
        }
    }

    /// Most recent max-per-node kinetic-energy proxy (|vel|^2). 0 before
    /// the first readback completes. Useful for stats overlays.
    pub fn sim_max_ke(&self) -> f32 {
        self.inner.as_ref().map(|r| r.sim_max_ke()).unwrap_or(0.0)
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
            // Default fallback box; prefer cam_fit_bounds for real data.
            let min = glam::Vec3::splat(-1000.0);
            let max = glam::Vec3::splat(1000.0);
            r.camera.fit_to_bounds(min, max);
        }
    }

    pub fn cam_fit_bounds(
        &mut self,
        min_x: f32,
        min_y: f32,
        min_z: f32,
        max_x: f32,
        max_y: f32,
        max_z: f32,
    ) {
        if let Some(r) = &mut self.inner {
            r.cam_fit_bounds(
                glam::Vec3::new(min_x, min_y, min_z),
                glam::Vec3::new(max_x, max_y, max_z),
            );
        }
    }

    pub fn cam_reset(&mut self) {
        if let Some(r) = &mut self.inner {
            r.camera.reset();
        }
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
}
