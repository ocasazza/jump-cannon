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
use glam::{Mat4, Vec3, Vec4};
use graph_layouts::{
    BoxedPhysics, DynPhysicsLayout, Edge as GlEdge, Graph as GlGraph, GpuForceLayout,
    GpuForceOptions, Node as GlNode,
};
use serde_json::Value;
use wgpu::util::DeviceExt;

use crate::ui::layout::registry::LayoutFactory;

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
    /// Fat-line pixel width for the edge quad expansion (vertex shader).
    /// 1.0 ≈ "old LineList" thickness; default 1.5 reads better on dense
    /// graphs without overpowering the stacked-alpha effect.
    edge_width: f32,
    /// Asymptotic alpha floor at very long edges. The fade curve smooths
    /// from `edge_min_transparency` toward this value past `edge_dist_max`,
    /// then 1/(1+x)-tails toward (but never reaches) it. Replaces what
    /// used to be `_pad1`; offset is unchanged so the 64-byte uniform
    /// layout stays bit-identical.
    edge_fade_floor: f32,
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
            edge_width: 1.5,
            edge_fade_floor: 0.02,
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
    /// Per-node alpha multiplier driven by Focus mode. 1.0 = full / not
    /// focused. <1.0 = dim. Coexists with `colors_base`/`set_selected`
    /// (the query path); they multiply on the GPU since this lives in a
    /// separate storage buffer.
    #[allow(dead_code)]
    dim_alpha: wgpu::Buffer,
    node_bind_group: wgpu::BindGroup,
    edge_bind_group: wgpu::BindGroup,

    layout: Option<Box<dyn DynPhysicsLayout>>,
    /// Cached graph the layout was initialised against. Needed so
    /// `swap_physics_layout` can re-init a freshly-built layout against
    /// the same topology without forcing the caller to re-supply it.
    layout_graph: Option<GlGraph>,
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
                ro_storage_entry(5, wgpu::ShaderStages::VERTEX),
            ],
        });
        let edge_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("edge bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                ro_storage_entry(2, wgpu::ShaderStages::VERTEX),
                ro_storage_entry(3, wgpu::ShaderStages::VERTEX),
                ro_storage_entry(4, wgpu::ShaderStages::VERTEX),
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
                // Fat lines: each edge expands into a screen-space quad
                // (6 verts, 2 tris) in the vertex shader for a constant
                // pixel width. See `shaders/edge.wgsl`.
                topology: wgpu::PrimitiveTopology::TriangleList,
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

        // Per-node focus dim factor — initialised to all-1.0 so the
        // default state is "no focus, full alpha everywhere".
        let dim_init: Vec<f32> = vec![1.0_f32; n_nodes.max(1) as usize];
        let dim_alpha_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dim_alpha_storage"),
            contents: bytemuck::cast_slice(&dim_init),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
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
                bg_entry(5, &dim_alpha_buf),
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
                bg_entry(4, &dim_alpha_buf),
            ],
        });

        // Initialise the GPU force layout against the same positions buffer.
        // TODO(layout-step-3): take the active LayoutFactory in here so we
        // can construct any registered physics backend, not just gpu-force.
        let layout_graph = build_topology_graph(&graph.positions, &graph.edges);
        let layout: Option<Box<dyn DynPhysicsLayout>> = {
            let mut boxed: Box<dyn DynPhysicsLayout> = Box::new(BoxedPhysics::new(
                GpuForceLayout::new(GpuForceOptions::default()),
            ));
            match boxed.init_with_device(device, queue, &layout_graph, &positions) {
                Ok(()) => Some(boxed),
                Err(e) => {
                    log::warn!("[graph-renderer] init_layout failed: {e}");
                    None
                }
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
            dim_alpha: dim_alpha_buf,
            node_bind_group,
            edge_bind_group,
            layout,
            layout_graph: Some(layout_graph),
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
            // 6 verts per edge for the fat-line quad expansion.
            rpass.draw(0..(b.n_edges * 6), 0..1);
        }
        if b.n_nodes > 0 {
            rpass.set_pipeline(&self.node_pipeline);
            rpass.set_bind_group(0, &b.node_bind_group, &[]);
            rpass.draw(0..6, 0..b.n_nodes);
        }
    }

    /// Screen-space picking. Coordinate spaces, spelled out:
    ///
    /// - `ndc_x`, `ndc_y`: normalized device coordinates of the cursor in
    ///   [-1, 1], y-up. Caller (workspace.rs / app.rs) computes these from the
    ///   *same egui rect that the wgpu callback painted into*:
    ///       ndc_x =  (px - rect.left) / rect.width  * 2 - 1
    ///       ndc_y = -((py - rect.top) / rect.height * 2 - 1)
    ///   That rect's width/height also feeds `screen_px` here, and
    ///   `set_screen` derives `camera.aspect` from the same numbers, so the
    ///   projection matrix below matches the rect the user clicked in.
    /// - `clip = view_proj * vec4(world, 1)`: 4D clip-space coordinate.
    ///   `clip.w` is positive view-space depth (RH look_to_rh + perspective_rh).
    /// - `node_ndc = clip.xy / clip.w`: NDC of the node center, in [-1,1].
    /// - `dist_px`: Euclidean pixel distance between cursor and node center
    ///   in the canvas's logical-pixel space (same units as `screen_px`).
    ///
    /// Picking algorithm:
    ///   1. Project every node to NDC; skip if behind camera (`clip.w <= 0`)
    ///      or far outside the viewport (with a small slop so nodes whose
    ///      drawn quad straddles the edge are still hittable).
    ///   2. Compute `dist_px` from cursor to node center.
    ///   3. Keep candidates with `dist_px <= max(R_PICK_PX, node_radius_px)`.
    ///   4. Among candidates, pick the one with the smallest `clip.w`
    ///      (frontmost). Falls back to smallest `dist_px` if depths tie.
    ///
    /// Why screen-space, not world-space ray/sphere: with the N-aware physics
    /// defaults pushing spring lengths past 400, a far node "near" the ray
    /// in world units can win over the obviously-clicked near node, because
    /// its scaled-by-distance world radius balloons. Screen-space distance
    /// is the metric the user actually sees, so it's the metric we pick on.
    ///
    /// Caller passes `screen_px` from the *click-frame's* egui rect
    /// (workspace.rs captures this when consuming `resp.clicked()`). We do
    /// **not** trust `self.screen_px` / `self.camera.aspect` here, because
    /// those are written by the GraphCallback's `prepare()` which runs
    /// *after* `App::update` returns — i.e. the click is consumed one tick
    /// before this frame's `set_screen`. If the dock layout reflowed this
    /// frame, the cached values would still describe the previous rect,
    /// and the projection used here would be off-by-aspect.
    pub fn raycast(&self, ndc_x: f32, ndc_y: f32, screen_px: [f32; 2]) -> Option<u32> {
        let b = self.buffers.as_ref()?;

        // 24 logical-pixel pick tolerance. Default node draw radius is ~4 px;
        // 24 px is roughly the size of a comfortable click target (Material
        // and HIG both put minimum touch targets at 44, but desktop mouse
        // input is much more precise — 24 gives forgiveness on a single-pixel
        // node without letting clicks across blank space steal a hit).
        // Per-node radius (pipes.sizes_cpu) overrides this floor for nodes
        // drawn larger than 24 px so the "visible disc" is always hittable.
        const R_PICK_PX: f32 = 24.0;

        // Build a projection from the *click-frame* rect, independent of
        // whatever aspect the cached camera currently holds.
        let width_px = screen_px[0].max(1.0);
        let height_px = screen_px[1].max(1.0);
        let aspect = (width_px / height_px).max(0.0001);
        let view = Mat4::look_to_rh(
            self.camera.position,
            self.camera.forward(),
            Vec3::Y,
        );
        let proj = Mat4::perspective_rh(
            self.camera.fov_y,
            aspect,
            self.camera.znear,
            self.camera.zfar,
        );
        let view_proj = proj * view;
        // Half-extents convert NDC delta -> pixel delta (NDC spans 2 units).
        let half_w = 0.5 * width_px;
        let half_h = 0.5 * height_px;

        // (dist_px, depth_w, idx) — we minimise on (depth_w, dist_px) so the
        // frontmost node wins, with screen-space distance as the tiebreaker.
        let mut best: Option<(f32, f32, u32)> = None;

        for i in 0..b.n_nodes as usize {
            if i * 3 + 2 >= b.positions_cpu.len() {
                break;
            }
            let center = Vec3::new(
                b.positions_cpu[i * 3],
                b.positions_cpu[i * 3 + 1],
                b.positions_cpu[i * 3 + 2],
            );
            // clip: 4D clip-space (pre-perspective-divide).
            let clip: Vec4 = view_proj * Vec4::new(center.x, center.y, center.z, 1.0);
            // Behind camera (or on the camera plane): not pickable.
            if clip.w <= 1e-4 {
                continue;
            }
            let inv_w = 1.0 / clip.w;
            // node_ndc_*: NDC of the node center, in [-1, 1].
            let node_ndc_x = clip.x * inv_w;
            let node_ndc_y = clip.y * inv_w;

            // Pixel-space delta between cursor and node center.
            let dx_px = (node_ndc_x - ndc_x) * half_w;
            let dy_px = (node_ndc_y - ndc_y) * half_h;
            let dist_px = (dx_px * dx_px + dy_px * dy_px).sqrt();

            // Tolerance: max of the global pick floor and the node's own
            // drawn radius (so a 40-px node is hittable across its whole disc).
            let node_r_px = b.sizes_cpu.get(i).copied().unwrap_or(4.0);
            let tol_px = R_PICK_PX.max(node_r_px);
            if dist_px > tol_px {
                continue;
            }

            // Note: we deliberately don't hard-cull on `node_ndc.abs() > 1`,
            // because a node whose center sits just outside the viewport
            // can still have its drawn disc overlap the cursor — the
            // `dist_px <= tol_px` test above already handles that case.

            let candidate = (dist_px, clip.w, i as u32);
            best = Some(match best {
                None => candidate,
                Some(prev) => {
                    // Prefer smaller depth (frontmost). Tiebreak on dist_px.
                    if clip.w < prev.1 - 1e-3 {
                        candidate
                    } else if (clip.w - prev.1).abs() <= 1e-3 && dist_px < prev.0 {
                        candidate
                    } else {
                        prev
                    }
                }
            });
        }
        best.map(|(_, _, i)| i)
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

    /// Push the per-node focus dim mask. `members` lists the node ids
    /// that belong to the focused community (always includes the focused
    /// node itself). When `focused` is `None` or `members` is empty the
    /// buffer is reset to all-1.0 (no dimming).
    ///
    /// This path is independent from `set_selected` (the QueryModel
    /// dimming): they multiply on the GPU because the node shader does
    /// `color.a *= dim_alpha[i]` on top of the colors-buffer alpha.
    pub fn set_focus_set(
        &mut self,
        queue: &wgpu::Queue,
        focused: Option<u32>,
        members: &std::collections::HashSet<u32>,
    ) {
        let Some(b) = self.buffers.as_mut() else { return };
        let n = b.n_nodes as usize;
        if n == 0 {
            return;
        }
        let dim_others: f32 = 0.25;
        let mut out: Vec<f32> = vec![1.0; n];
        if focused.is_some() && !members.is_empty() {
            for v in out.iter_mut() {
                *v = dim_others;
            }
            for &m in members {
                if (m as usize) < n {
                    out[m as usize] = 1.0;
                }
            }
            if let Some(f) = focused {
                if (f as usize) < n {
                    out[f as usize] = 1.0;
                }
            }
        }
        queue.write_buffer(&b.dim_alpha, 0, bytemuck::cast_slice(&out));
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
        width_px: f32,
        fade_floor: f32,
    ) {
        self.effects.edge_color = color;
        self.effects.edge_alpha_mul = alpha_mul.max(0.0);
        let lo = dist_range.0.max(0.0);
        let hi = dist_range.1.max(lo + 0.001);
        self.effects.edge_dist_min = lo;
        self.effects.edge_dist_max = hi;
        self.effects.edge_min_transparency = min_transparency.clamp(0.0, 1.0);
        self.effects.edge_width = width_px.max(0.0);
        // Floor must stay below the long-distance asymptote, otherwise
        // the smoothstep would invert. Clamp into a safe range.
        self.effects.edge_fade_floor = fade_floor.clamp(0.0, 0.5);
    }

    /// Push the cursor force into the GPU layout. radius=0 disables.
    ///
    /// JSON round-trip per cursor move is wasteful — Step 1 punts on the
    /// optimisation; a typed cursor sink trait method would let us skip
    /// the deserialise/reserialise cycle.
    pub fn set_cursor_force(&mut self, world: [f32; 3], radius: f32, strength: f32) {
        let Some(b) = self.buffers.as_mut() else { return };
        let Some(l) = b.layout.as_mut() else { return };
        let json = l.settings_json();
        if let Ok(mut opts) = serde_json::from_value::<GpuForceOptions>(json) {
            opts.cursor_pos = world;
            opts.cursor_radius = radius;
            opts.cursor_strength = strength;
            if let Ok(v) = serde_json::to_value(&opts) {
                let _ = l.set_settings_json(&v);
            }
        }
        // Mirror the visual radius into the effects uniform so the
        // shader can render a hint ring at the cursor.
        self.effects.cursor_radius_visual = radius;
    }

    /// Replace the entire physics layout settings block from JSON. The
    /// `update_layout_options` wrapper below preserves typed access for
    /// the cursor / cool-down paths still living in `app.rs`.
    pub fn set_physics_layout_settings_json(&mut self, settings: &serde_json::Value) {
        let Some(b) = self.buffers.as_mut() else { return };
        let Some(l) = b.layout.as_mut() else { return };
        if let Err(e) = l.set_settings_json(settings) {
            log::warn!("[graph-renderer] set_physics_layout_settings_json: {e}");
        }
    }

    /// Backward-compat wrapper. TODO(layout-step-3): drop in favour of
    /// the JSON path once `app.rs` and `web.rs` (legacy renderer) no
    /// longer need typed `GpuForceOptions`.
    pub fn update_layout_options(&mut self, opts: GpuForceOptions) {
        let Some(b) = self.buffers.as_mut() else { return };
        let Some(l) = b.layout.as_mut() else { return };
        if let Ok(v) = serde_json::to_value(&opts) {
            let _ = l.set_settings_json(&v);
        }
    }

    /// Drop the existing physics layout and construct a new one via the
    /// supplied factory + JSON settings. Re-initialises against the
    /// current node topology and positions buffer.
    ///
    /// Step 1 only registers gpu-force, so this branch never fires in
    /// practice — but the wiring is here so Step 3 (multi-backend swap)
    /// is a one-line call.
    pub fn swap_physics_layout(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        factory: &LayoutFactory,
        settings: &serde_json::Value,
    ) {
        let Some(b) = self.buffers.as_mut() else { return };
        let LayoutFactory::Physics { build, .. } = factory else {
            log::warn!("[graph-renderer] swap_physics_layout: factory is not Physics");
            return;
        };
        // Sync the cached topology graph's `position3` from the live CPU
        // positions mirror, so a fresh physics layout's `init_with_device`
        // (which uploads `precompute().initial_positions` straight from
        // `node.position3`) resumes from whatever the previous layout
        // — including a one-shot static solve — left behind, instead of
        // jumping back to the bootstrap positions.
        if let Some(graph) = b.layout_graph.as_mut() {
            // The renderer-side helper `build_topology_graph` builds ids
            // as zero-padded indices, so the same scheme indexes back in.
            let n = b.n_nodes as usize;
            let width = format!("{}", n.max(1) - 1).len().max(1);
            for i in 0..n {
                if i * 3 + 2 >= b.positions_cpu.len() {
                    break;
                }
                let id = format!("{:0width$}", i, width = width);
                if let Some(node) = graph.nodes.get_mut(&id) {
                    node.position3 = Some([
                        b.positions_cpu[i * 3],
                        b.positions_cpu[i * 3 + 1],
                        b.positions_cpu[i * 3 + 2],
                    ]);
                }
            }
        }
        let Some(graph) = b.layout_graph.as_ref() else { return };
        let mut new_layout = build(settings);
        match new_layout.init_with_device(device, queue, graph, &b.positions) {
            Ok(()) => {
                b.layout = Some(new_layout);
            }
            Err(e) => {
                log::warn!("[graph-renderer] swap_physics_layout init failed: {e}");
            }
        }
    }

    /// Run a one-shot Static layout against the cached topology graph and
    /// upload the result into the shared positions buffer. Drops any
    /// active physics layout so `compute_step` becomes a no-op until the
    /// caller swaps a new physics backend in (e.g. by switching the
    /// algorithm ComboBox back to "GPU force").
    pub fn run_static_solve(
        &mut self,
        queue: &wgpu::Queue,
        factory: &LayoutFactory,
        settings: &Value,
    ) -> Result<(), String> {
        let LayoutFactory::Static { build, .. } = factory else {
            return Err("run_static_solve: factory is not Static".to_string());
        };
        let b = self
            .buffers
            .as_mut()
            .ok_or_else(|| "run_static_solve: no buffers loaded".to_string())?;
        let graph = b
            .layout_graph
            .as_ref()
            .ok_or_else(|| "run_static_solve: no cached topology graph".to_string())?;

        let dyn_layout = build();
        let positions = dyn_layout.solve_dyn(settings, graph)?;
        let n_nodes = b.n_nodes as usize;
        if positions.len() != n_nodes * 3 {
            return Err(format!(
                "run_static_solve: solver returned {} floats, expected {}",
                positions.len(),
                n_nodes * 3,
            ));
        }

        // Re-pack into the vec4-padded layout the WGSL storage buffer
        // expects (matches the format `load()` builds at bootstrap).
        let mut padded: Vec<f32> = Vec::with_capacity(n_nodes * 4);
        for i in 0..n_nodes {
            padded.extend_from_slice(&[
                positions[i * 3],
                positions[i * 3 + 1],
                positions[i * 3 + 2],
                0.0,
            ]);
        }
        if padded.is_empty() {
            padded.extend_from_slice(&[0.0; 4]);
        }
        queue.write_buffer(&b.positions, 0, bytemuck::cast_slice(&padded));

        // Refresh the CPU mirror so raycasts and `bounds()` see the new
        // positions instead of the pre-solve state.
        b.positions_cpu = positions;

        // Tear down any active physics layout so `compute_step` doesn't
        // immediately stomp the freshly-written positions.
        b.layout = None;

        Ok(())
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

    /// Most recent max-KE readback from the GPU force sim, if any.
    /// Used by the renderer to throttle repaint cadence while the sim
    /// is warming up (high KE → user can't perceive 60fps detail).
    pub fn last_max_ke(&self) -> f32 {
        self.buffers
            .as_ref()
            .and_then(|b| b.layout.as_ref())
            .map(|l| l.last_max_ke())
            .unwrap_or(0.0)
    }

    /// Current layout options snapshot, if a layout exists.
    /// Returns `None` if the active layout's settings can't decode into
    /// `GpuForceOptions` (e.g. once Step 3 swaps in a non-gpu-force
    /// physics backend, callers will need a typed-per-backend path).
    pub fn layout_options(&self) -> Option<GpuForceOptions> {
        let l = self.buffers.as_ref().and_then(|b| b.layout.as_ref())?;
        serde_json::from_value::<GpuForceOptions>(l.settings_json()).ok()
    }

    /// Active physics layout's settings as raw JSON. Step 1 only needs
    /// this for the change-detect hash in `App::layout_key`.
    pub fn layout_settings_json(&self) -> Option<serde_json::Value> {
        self.buffers
            .as_ref()
            .and_then(|b| b.layout.as_ref())
            .map(|l| l.settings_json())
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

/// Build a `graph_layouts::Graph` topology mirror from the renderer's
/// flat position/edge buffers. The id padding scheme matches what the
/// pre-refactor `build_layout` used so existing tests that probe the
/// node id format keep passing.
fn build_topology_graph(positions: &[f32], edges: &[u32]) -> GlGraph {
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
    g
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
