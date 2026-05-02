//! Surface-free wgpu state for the graph layer.
//!
//! Owns:
//!   - node + edge render pipelines (built against eframe's surface format)
//!   - shared positions/colors/sizes/edges storage buffers
//!   - camera + effects uniforms
//!   - GpuForceLayout (compute) bound to the same positions buffer
//!
//! Driven by `egui_wgpu::CallbackTrait`:
//!   - `prepare()` calls `compute_step` (records compute dispatch into the
//!     supplied encoder) + `write_uniforms` (camera/effects)
//!   - `paint()` records the edge + node draws into the rpass that egui_wgpu
//!     already opened on the eframe surface texture.
//!
//! Because egui_wgpu's render pass has no depth attachment, the pipelines
//! here are built with `depth_stencil: None`. The original Renderer ran a
//! standalone pass with a Depth32Float buffer; we lose early-z but both
//! pipelines already had `depth_write_enabled = false`, so order is the only
//! thing that mattered (edges before nodes — preserved here).

use crate::camera::Camera;
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use graph_layouts::{
    Edge as GlEdge, Graph as GlGraph, GpuForceLayout, GpuForceOptions, Node as GlNode,
};
use wgpu::util::DeviceExt;

#[derive(Clone)]
pub struct GraphData {
    pub positions: Vec<f32>, // [x0,y0,z0, ...] length = 3*n
    pub edges: Vec<u32>,     // [src,tgt, ...] length = 2*m
    pub colors: Vec<f32>,    // [r,g,b,a, ...] length = 4*n
    pub sizes: Vec<f32>,     // length = n
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    cam_pos: [f32; 3],
    _pad0: f32,
    screen: [f32; 2],
    _pad1: [f32; 2],
}

// Mirrors `EffectsUniform` in shaders/{node,edge}.wgsl byte-for-byte.
// Total size: 16 f32 = 64 bytes. The vec4 (`edge_color`) sits at offset 32
// so its 16-byte WGSL alignment is naturally satisfied.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct EffectsUniform {
    focus_plane_z: f32,
    focus_thickness: f32,
    cursor_radius_visual: f32,
    blur_strength: f32,
    max_coc: f32,
    edge_alpha_mul: f32,
    edge_dist_min: f32,
    edge_dist_max: f32,
    edge_color: [f32; 4],
    edge_min_transparency: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Default for EffectsUniform {
    fn default() -> Self {
        Self {
            focus_plane_z: 800.0,
            // 1e9 = "DoF off" sentinel — node.wgsl skips the bokeh path
            // entirely while focus_thickness >= 1e6.
            focus_thickness: 1.0e9,
            cursor_radius_visual: 0.0,
            blur_strength: 0.05,
            max_coc: 60.0,
            // Cosmograph-style edge defaults: thin alpha lines that stack
            // on a near-black background. linkColor #3a4880 → linear-ish
            // (0.227, 0.282, 0.502, 1.0); alpha-mul 0.6 mimics the demo's
            // density. distance range 10..400 with min-transparency 0.6
            // means long edges hold ~40% visibility, never disappearing.
            edge_alpha_mul: 0.6,
            edge_dist_min: 10.0,
            edge_dist_max: 400.0,
            edge_color: [0.227, 0.282, 0.502, 1.0],
            edge_min_transparency: 0.6,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        }
    }
}

/// All wgpu state for the graph layer minus the surface (eframe owns that).
pub struct GraphPipelines {
    pub camera: Camera,

    color_format: wgpu::TextureFormat,
    node_pipeline: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,
    node_bgl: wgpu::BindGroupLayout,
    edge_bgl: wgpu::BindGroupLayout,

    /// Lazily populated once the fetch task hands us bootstrap data.
    buffers: Option<Buffers>,

    /// Per-frame screen size, written into the camera uniform on prepare().
    /// egui_wgpu hands us a ScreenDescriptor with size_in_pixels; we mirror
    /// it here so paint() can also read it for aspect calc on resize.
    screen_px: [f32; 2],

    /// CPU mirror of the effects so partial setters don't clobber.
    effects: EffectsUniform,
}

struct Buffers {
    positions: wgpu::Buffer,
    colors: wgpu::Buffer,
    sizes: wgpu::Buffer,
    #[allow(dead_code)]
    edges: wgpu::Buffer,
    n_nodes: u32,
    n_edges: u32,
    camera_uniform: wgpu::Buffer,
    effects_uniform: wgpu::Buffer,
    node_bind_group: wgpu::BindGroup,
    edge_bind_group: wgpu::BindGroup,

    layout: Option<GpuForceLayout>,
    /// CPU mirrors. positions/sizes used for raycast + fit; colors_base
    /// is the per-node base RGBA so set_selected can multiply alpha
    /// without losing the underlying tint.
    positions_cpu: Vec<f32>,
    sizes_cpu: Vec<f32>,
    colors_base: Vec<f32>,
    sizes_base: Vec<f32>,
    edges_cpu: Vec<u32>,
}

impl GraphPipelines {
    /// Build pipelines against the eframe-supplied device + surface format.
    /// Buffers are deferred until `load()`.
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let node_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("node bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                ro_storage_entry(2, wgpu::ShaderStages::VERTEX),
                ro_storage_entry(3, wgpu::ShaderStages::VERTEX),
                ro_storage_entry(4, wgpu::ShaderStages::VERTEX),
            ],
        });
        let edge_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("edge bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                ro_storage_entry(2, wgpu::ShaderStages::VERTEX),
                ro_storage_entry(3, wgpu::ShaderStages::VERTEX),
            ],
        });

        let node_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("node.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/node.wgsl").into()),
        });
        let edge_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("edge.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/edge.wgsl").into()),
        });

        let node_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("node pipeline layout"),
            bind_group_layouts: &[&node_bgl],
            push_constant_ranges: &[],
        });
        let edge_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("edge pipeline layout"),
            bind_group_layouts: &[&edge_bgl],
            push_constant_ranges: &[],
        });

        // No depth attachment in egui_wgpu's pass — drop the depth state.
        let node_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("node pipeline"),
            layout: Some(&node_pl),
            vertex: wgpu::VertexState {
                module: &node_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &node_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let edge_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("edge pipeline"),
            layout: Some(&edge_pl),
            vertex: wgpu::VertexState {
                module: &edge_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &edge_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            camera: Camera::new(1.0),
            color_format,
            node_pipeline,
            edge_pipeline,
            node_bgl,
            edge_bgl,
            buffers: None,
            screen_px: [1.0, 1.0],
            effects: EffectsUniform::default(),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.buffers.is_some()
    }

    pub fn n_nodes(&self) -> u32 {
        self.buffers.as_ref().map(|b| b.n_nodes).unwrap_or(0)
    }
    pub fn n_edges(&self) -> u32 {
        self.buffers.as_ref().map(|b| b.n_edges).unwrap_or(0)
    }

    /// Upload buffers + initialise the compute layout. Call once from the
    /// App once the fetch task delivers Bootstrap.
    pub fn load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: GraphData,
    ) -> Result<(), String> {
        let _ = self.color_format; // (kept for future re-creation paths)

        let n_nodes = (graph.positions.len() / 3) as u32;
        let n_edges = (graph.edges.len() / 2) as u32;

        // positions: vec4-padded so WGSL `array<vec3<f32>>` (16-byte stride)
        // sees what we expect.
        let mut positions_padded: Vec<f32> = Vec::with_capacity(n_nodes as usize * 4);
        for i in 0..n_nodes as usize {
            positions_padded.extend_from_slice(&[
                graph.positions[i * 3],
                graph.positions[i * 3 + 1],
                graph.positions[i * 3 + 2],
                0.0,
            ]);
        }
        if positions_padded.is_empty() {
            positions_padded.extend_from_slice(&[0.0; 4]);
        }
        let positions = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_storage"),
            contents: bytemuck::cast_slice(&positions_padded),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });

        let mut colors = graph.colors.clone();
        if colors.is_empty() {
            colors.extend_from_slice(&[0.0; 4]);
        }
        let colors_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("colors_storage"),
            contents: bytemuck::cast_slice(&colors),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let mut sizes = graph.sizes.clone();
        if sizes.is_empty() {
            sizes.push(0.0);
        }
        let sizes_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sizes_storage"),
            contents: bytemuck::cast_slice(&sizes),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let mut edges_packed: Vec<u32> = graph.edges.clone();
        if edges_packed.is_empty() {
            edges_packed.extend_from_slice(&[0, 0]);
        }
        let edges_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edges_storage"),
            contents: bytemuck::cast_slice(&edges_packed),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let cam_uniform_init = CameraUniform {
            view_proj: self.camera.view_proj(),
            view: self.camera.view(),
            cam_pos: self.camera.position.to_array(),
            _pad0: 0.0,
            screen: self.screen_px,
            _pad1: [0.0, 0.0],
        };
        let camera_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera uniform"),
            contents: bytemuck::bytes_of(&cam_uniform_init),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let effects_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("effects uniform"),
            contents: bytemuck::bytes_of(&self.effects),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let node_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("node bg"),
            layout: &self.node_bgl,
            entries: &[
                bg_entry(0, &camera_uniform),
                bg_entry(1, &effects_uniform),
                bg_entry(2, &positions),
                bg_entry(3, &colors_buf),
                bg_entry(4, &sizes_buf),
            ],
        });
        let edge_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edge bg"),
            layout: &self.edge_bgl,
            entries: &[
                bg_entry(0, &camera_uniform),
                bg_entry(1, &effects_uniform),
                bg_entry(2, &positions),
                bg_entry(3, &edges_buf),
            ],
        });

        // Initialise the GPU force layout against the same positions buffer.
        let layout = match build_layout(device, queue, &graph.positions, &graph.edges, &positions) {
            Ok(l) => Some(l),
            Err(e) => {
                log::warn!("[graph-renderer] init_layout failed: {e}");
                None
            }
        };

        let colors_base = graph.colors.clone();
        let sizes_base = graph.sizes.clone();
        self.buffers = Some(Buffers {
            positions,
            colors: colors_buf,
            sizes: sizes_buf,
            edges: edges_buf,
            n_nodes,
            n_edges,
            camera_uniform,
            effects_uniform,
            node_bind_group,
            edge_bind_group,
            layout,
            positions_cpu: graph.positions,
            sizes_cpu: graph.sizes,
            colors_base,
            sizes_base,
            edges_cpu: graph.edges,
        });

        // Auto-fit the camera to the loaded graph so the bootstrap frame
        // shows something visible (the server's 2D ring is ±radius for
        // ~10k nodes ~ 0..1000 world units).
        self.fit_to_loaded_bounds();

        Ok(())
    }

    fn fit_to_loaded_bounds(&mut self) {
        let Some(b) = &self.buffers else { return };
        if b.positions_cpu.is_empty() {
            return;
        }
        let mut mn = Vec3::splat(f32::INFINITY);
        let mut mx = Vec3::splat(f32::NEG_INFINITY);
        for chunk in b.positions_cpu.chunks_exact(3) {
            let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
            mn = mn.min(p);
            mx = mx.max(p);
        }
        if mn.is_finite() && mx.is_finite() {
            self.camera.fit_to_bounds(mn, mx);
        }
    }

    /// Called from `egui_wgpu::CallbackTrait::prepare`.
    /// Records compute dispatch into the supplied encoder.
    pub fn compute_step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let Some(b) = &mut self.buffers else { return };
        if let Some(l) = b.layout.as_mut() {
            l.step_with_encoder(device, queue, encoder, &b.positions);
        }
    }

    /// Camera + effects uniform writes. Called from prepare().
    pub fn write_uniforms(&self, queue: &wgpu::Queue, screen_px: [f32; 2]) {
        let Some(b) = &self.buffers else { return };
        let cam = CameraUniform {
            view_proj: self.camera.view_proj(),
            view: self.camera.view(),
            cam_pos: self.camera.position.to_array(),
            _pad0: 0.0,
            screen: screen_px,
            _pad1: [0.0, 0.0],
        };
        queue.write_buffer(&b.camera_uniform, 0, bytemuck::bytes_of(&cam));
        queue.write_buffer(&b.effects_uniform, 0, bytemuck::bytes_of(&self.effects));
    }

    /// Apply the supplied screen size + aspect to the camera. Called every
    /// frame from prepare() since the egui paint rect can change.
    pub fn set_screen(&mut self, screen_px: [f32; 2]) {
        self.screen_px = screen_px;
        self.camera.aspect = (screen_px[0] / screen_px[1]).max(0.0001);
    }

    /// Called from `egui_wgpu::CallbackTrait::paint`.
    /// Records the edge + node draws into egui's render pass.
    pub fn draw(&self, rpass: &mut wgpu::RenderPass<'static>) {
        let Some(b) = &self.buffers else { return };
        if b.n_edges > 0 {
            rpass.set_pipeline(&self.edge_pipeline);
            rpass.set_bind_group(0, &b.edge_bind_group, &[]);
            rpass.draw(0..(b.n_edges * 2), 0..1);
        }
        if b.n_nodes > 0 {
            rpass.set_pipeline(&self.node_pipeline);
            rpass.set_bind_group(0, &b.node_bind_group, &[]);
            rpass.draw(0..6, 0..b.n_nodes);
        }
    }

    pub fn raycast(&self, ndc_x: f32, ndc_y: f32) -> Option<u32> {
        let b = self.buffers.as_ref()?;
        let (origin, dir) = self.camera.raycast(ndc_x, ndc_y);
        let mut best: Option<(f32, u32)> = None;
        let tan_half = (self.camera.fov_y * 0.5).tan();
        let screen_h = self.screen_px[1].max(1.0);
        let two_tan_over_h = 2.0 * tan_half / screen_h;
        for i in 0..b.n_nodes as usize {
            if i * 3 + 2 >= b.positions_cpu.len() {
                break;
            }
            let center = Vec3::new(
                b.positions_cpu[i * 3],
                b.positions_cpu[i * 3 + 1],
                b.positions_cpu[i * 3 + 2],
            );
            let px = b.sizes_cpu.get(i).copied().unwrap_or(4.0);
            let dist = (center - origin).length().max(1.0);
            let r = (two_tan_over_h * dist * px).max(1.0);
            let oc = origin - center;
            let bb = oc.dot(dir);
            let c = oc.dot(oc) - r * r;
            let disc = bb * bb - c;
            if disc < 0.0 {
                continue;
            }
            let t = -bb - disc.sqrt();
            if t > 0.0 && best.map(|(bt, _)| t < bt).unwrap_or(true) {
                best = Some((t, i as u32));
            }
        }
        best.map(|(_, i)| i)
    }

    pub fn edges_cpu(&self) -> &[u32] {
        self.buffers.as_ref().map(|b| b.edges_cpu.as_slice()).unwrap_or(&[])
    }
    pub fn positions_cpu(&self) -> &[f32] {
        self.buffers.as_ref().map(|b| b.positions_cpu.as_slice()).unwrap_or(&[])
    }

    /// Replace per-node base RGBA. Length must equal `n_nodes * 4`.
    /// Caller can then call `set_selected` to overlay dimming.
    pub fn update_colors(&mut self, queue: &wgpu::Queue, colors: Vec<f32>) {
        let Some(b) = self.buffers.as_mut() else { return };
        if colors.len() != b.n_nodes as usize * 4 {
            log::warn!(
                "[graph-renderer] update_colors: len {} != n*4 {}",
                colors.len(),
                b.n_nodes * 4
            );
            return;
        }
        b.colors_base = colors.clone();
        queue.write_buffer(&b.colors, 0, bytemuck::cast_slice(&colors));
    }

    /// Replace per-node screen-space radius (px). Length must equal `n_nodes`.
    pub fn update_sizes(&mut self, queue: &wgpu::Queue, sizes: Vec<f32>) {
        let Some(b) = self.buffers.as_mut() else { return };
        if sizes.len() != b.n_nodes as usize {
            log::warn!(
                "[graph-renderer] update_sizes: len {} != n {}",
                sizes.len(),
                b.n_nodes
            );
            return;
        }
        b.sizes_base = sizes.clone();
        b.sizes_cpu = sizes.clone();
        queue.write_buffer(&b.sizes, 0, bytemuck::cast_slice(&sizes));
    }

    /// Apply a per-node alpha multiplier from the query selection. When
    /// `selected` is `None` the base RGBA is restored. Otherwise nodes
    /// not in the set drop to 0.18 alpha.
    pub fn set_selected(&mut self, queue: &wgpu::Queue, selected: Option<&std::collections::HashSet<u32>>) {
        let Some(b) = self.buffers.as_mut() else { return };
        if b.colors_base.len() != b.n_nodes as usize * 4 {
            return;
        }
        let mut out: Vec<f32> = b.colors_base.clone();
        if let Some(set) = selected {
            for i in 0..b.n_nodes as usize {
                let off = i * 4 + 3;
                if !set.contains(&(i as u32)) {
                    out[off] = b.colors_base[off] * 0.18;
                }
            }
        }
        queue.write_buffer(&b.colors, 0, bytemuck::cast_slice(&out));
    }

    /// Update the focal plane center + thickness (effects uniform).
    pub fn set_focus_plane(&mut self, z: f32, thickness: f32) {
        self.effects.focus_plane_z = z;
        self.effects.focus_thickness = thickness.max(1.0);
    }

    /// Update DoF blur strength + max circle-of-confusion.
    pub fn set_dof_params(&mut self, blur: f32, max_coc: f32) {
        self.effects.blur_strength = blur.max(0.0);
        self.effects.max_coc = max_coc.max(0.0);
    }

    /// Update cosmograph-style edge appearance. `color` is RGBA in 0..1.
    /// `dist_range = (min, max)` is the visibility distance range from
    /// the reference (`linkVisibilityDistanceRange`). `min_transparency`
    /// is the floor at long distances (`linkVisibilityMinTransparency`).
    pub fn set_edge_style(
        &mut self,
        color: [f32; 4],
        alpha_mul: f32,
        dist_range: (f32, f32),
        min_transparency: f32,
    ) {
        self.effects.edge_color = color;
        self.effects.edge_alpha_mul = alpha_mul.max(0.0);
        let lo = dist_range.0.max(0.0);
        let hi = dist_range.1.max(lo + 0.001);
        self.effects.edge_dist_min = lo;
        self.effects.edge_dist_max = hi;
        self.effects.edge_min_transparency = min_transparency.clamp(0.0, 1.0);
    }

    /// Push the cursor force into the GPU layout. radius=0 disables.
    pub fn set_cursor_force(&mut self, world: [f32; 3], radius: f32, strength: f32) {
        let Some(b) = self.buffers.as_mut() else { return };
        let Some(l) = b.layout.as_mut() else { return };
        let mut opts = l.options().clone();
        opts.cursor_pos = world;
        opts.cursor_radius = radius;
        opts.cursor_strength = strength;
        l.set_options(opts);
        // Mirror the visual radius into the effects uniform so the
        // shader can render a hint ring at the cursor.
        self.effects.cursor_radius_visual = radius;
    }

    /// Replace the entire layout option block. Wakes the sim.
    pub fn update_layout_options(&mut self, opts: GpuForceOptions) {
        let Some(b) = self.buffers.as_mut() else { return };
        let Some(l) = b.layout.as_mut() else { return };
        l.set_options(opts);
    }

    /// True once the GPU force layout has settled (max-KE under the
    /// configured `energy_threshold` for `HALT_FRAMES` consecutive
    /// readbacks). False if no layout is initialised or auto-halt is
    /// disabled (`energy_threshold == 0.0`). Drives the Stats panel
    /// running/settled indicator.
    pub fn is_halted(&self) -> bool {
        self.buffers
            .as_ref()
            .and_then(|b| b.layout.as_ref())
            .map(|l| l.is_halted())
            .unwrap_or(false)
    }

    /// Current layout options snapshot, if a layout exists.
    pub fn layout_options(&self) -> Option<GpuForceOptions> {
        self.buffers
            .as_ref()
            .and_then(|b| b.layout.as_ref())
            .map(|l| l.options().clone())
    }

    /// Centroid of currently-loaded node positions (CPU mirror —
    /// reflects the bootstrap state, not the live GPU sim, which is
    /// fine for follow-centroid steering at this scale).
    pub fn centroid(&self) -> Option<Vec3> {
        let b = self.buffers.as_ref()?;
        if b.positions_cpu.is_empty() {
            return None;
        }
        let mut sum = Vec3::ZERO;
        let mut n = 0usize;
        for chunk in b.positions_cpu.chunks_exact(3) {
            sum += Vec3::new(chunk[0], chunk[1], chunk[2]);
            n += 1;
        }
        if n == 0 {
            None
        } else {
            Some(sum / n as f32)
        }
    }

    /// Axis-aligned bounds of the loaded node positions.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let b = self.buffers.as_ref()?;
        if b.positions_cpu.is_empty() {
            return None;
        }
        let mut mn = Vec3::splat(f32::INFINITY);
        let mut mx = Vec3::splat(f32::NEG_INFINITY);
        for chunk in b.positions_cpu.chunks_exact(3) {
            let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
            mn = mn.min(p);
            mx = mx.max(p);
        }
        if mn.is_finite() && mx.is_finite() {
            Some((mn, mx))
        } else {
            None
        }
    }

    /// Re-fit the camera to the current bounds. Used by the camera
    /// section's "Fit" button and the auto-fit toggle.
    pub fn fit_camera(&mut self) {
        if let Some((mn, mx)) = self.bounds() {
            self.camera.fit_to_bounds(mn, mx);
        }
    }
}

fn build_layout(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    positions: &[f32],
    edges: &[u32],
    positions_buf: &wgpu::Buffer,
) -> Result<GpuForceLayout, String> {
    let n = positions.len() / 3;
    let width = format!("{}", n.max(1) - 1).len().max(1);

    let mut g = GlGraph::new();
    for i in 0..n {
        let id = format!("{:0width$}", i, width = width);
        let mut node = GlNode::new(id.clone());
        if i * 3 + 2 < positions.len() {
            node.position3 = Some([
                positions[i * 3],
                positions[i * 3 + 1],
                positions[i * 3 + 2],
            ]);
        }
        g.add_node(node);
    }
    for (e_i, chunk) in edges.chunks_exact(2).enumerate() {
        let s = chunk[0] as usize;
        let t = chunk[1] as usize;
        if s >= n || t >= n {
            continue;
        }
        let sid = format!("{:0width$}", s, width = width);
        let tid = format!("{:0width$}", t, width = width);
        g.add_edge(GlEdge::new(format!("e{}", e_i), sid, tid));
    }

    let mut layout = GpuForceLayout::new(GpuForceOptions::default());
    layout.init_with_device(device, queue, &g, positions_buf)?;
    Ok(layout)
}

fn uniform_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn ro_storage_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bg_entry(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}
