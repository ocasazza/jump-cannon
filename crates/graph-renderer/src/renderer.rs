//! Target-agnostic wgpu state. Constructor takes an already-created
//! `wgpu::Surface`, so the native (winit window) and web (canvas) paths can
//! both feed it.
//!
//! ## Shared-buffer architecture (Wave 2)
//!
//! `Renderer` owns a single `positions_storage` buffer with usage
//! `STORAGE | VERTEX | COPY_SRC | COPY_DST`. Layout (std430): one
//! `vec3<f32>` per node (16-byte stride — vec3s are 16-aligned in WGSL).
//!
//! - The node + edge vertex shaders bind it as `storage<read>` and index
//!   into it from `instance_index` / `vertex_index`. No per-frame CPU copy.
//! - The compute shader from `graph-layouts::GpuForceLayout` writes into
//!   the same buffer (via `init_with_device` → `step_with_encoder`).
//! - Edges therefore automatically follow nodes — the edge vertex shader
//!   reads each endpoint's current position from the same storage array.
//!
//! Effects (microscope focus plane) live in a small uniform alongside the
//! camera so the fragment shaders can attenuate alpha by world-space Z.

use crate::camera::Camera;
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use graph_layouts::{Edge as GlEdge, Graph as GlGraph, GpuForceLayout, GpuForceOptions, Node as GlNode};
use wgpu::util::DeviceExt;

#[derive(Clone)]
pub struct GraphData {
    pub positions: Vec<f32>, // [x0,y0,z0, ...] length = 3*n
    pub edges: Vec<u32>,     // [src,tgt, ...] length = 2*m
    pub colors: Vec<f32>,    // [r,g,b,a, ...] length = 4*n
    pub sizes: Vec<f32>,     // length = n
}

#[derive(Clone, Copy)]
pub struct RendererConfig {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 3],
    _pad0: f32,
    screen: [f32; 2],
    _pad1: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct EffectsUniform {
    focus_plane_z: f32,
    focus_thickness: f32,
    cursor_radius_visual: f32,
    _pad: f32,
}

impl Default for EffectsUniform {
    fn default() -> Self {
        Self {
            focus_plane_z: 0.0,
            // Default huge thickness = effectively no focus attenuation
            // until the user moves the slider.
            focus_thickness: 1.0e9,
            cursor_radius_visual: 0.0,
            _pad: 0.0,
        }
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,

    node_pipeline: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,

    /// Shared with the compute layout engine. Layout: vec3+pad per node.
    positions_storage: wgpu::Buffer,
    colors_storage: wgpu::Buffer,
    sizes_storage: wgpu::Buffer,
    /// Edges-as-vec2<u32> storage buffer (src, tgt) per edge. Bound to the
    /// edge pipeline's bind group; kept on the struct so the binding stays
    /// alive for the lifetime of the renderer.
    #[allow(dead_code)]
    edges_storage: wgpu::Buffer,
    n_nodes: u32,
    n_edges: u32,

    camera_uniform_buffer: wgpu::Buffer,
    effects_uniform_buffer: wgpu::Buffer,
    /// Per-pipeline bind groups (each pipeline has its own bgl since the
    /// node pipeline binds the geometry storage and the edge pipeline
    /// additionally binds the edges array).
    node_bind_group: wgpu::BindGroup,
    edge_bind_group: wgpu::BindGroup,

    // CPU caches for raycasting / fit. Updated lazily via read_back_positions.
    node_positions_cpu: Vec<f32>,
    node_sizes_cpu: Vec<f32>,
    edges_cpu: Vec<u32>,

    layout: Option<GpuForceLayout>,
    /// Stable id ordering used when we built the layout's graph; needed so
    /// the renderer-side instance index lines up with the compute index.
    /// Currently we just use 0..n_nodes order matching the GraphData input;
    /// the layout uses sorted-id order. We synthesise integer ids
    /// `0..n_nodes` when initialising the layout so the orderings match.
    layout_initialized: bool,

    pub camera: Camera,
}

impl Renderer {
    pub async fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        config: RendererConfig,
        graph: GraphData,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| "no compatible adapter".to_string())?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("graph-renderer device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults()
                        .using_resolution(adapter.limits()),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
            .map_err(|e| format!("request_device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: config.width.max(1),
            height: config.height.max(1),
            present_mode: caps.present_modes[0],
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);
        let depth_view = create_depth_view(&device, surface_config.width, surface_config.height);

        // --- Camera uniform + effects uniform ---
        let camera = Camera::new(surface_config.width as f32 / surface_config.height as f32);
        let cam_uniform = CameraUniform {
            view_proj: camera.view_proj(),
            cam_pos: camera.position.to_array(),
            _pad0: 0.0,
            screen: [surface_config.width as f32, surface_config.height as f32],
            _pad1: [0.0, 0.0],
        };
        let camera_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera uniform"),
            contents: bytemuck::bytes_of(&cam_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let effects_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("effects uniform"),
            contents: bytemuck::bytes_of(&EffectsUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // --- Storage buffers (shared with compute) ---
        let n_nodes = (graph.positions.len() / 3) as u32;
        let n_edges = (graph.edges.len() / 2) as u32;

        // positions_storage: vec4-padded so the compute shader's
        // `array<vec3<f32>>` (16-byte stride in WGSL) sees what we expect.
        let mut positions_padded: Vec<f32> = Vec::with_capacity(n_nodes as usize * 4);
        for i in 0..n_nodes as usize {
            positions_padded.extend_from_slice(&[
                graph.positions[i * 3],
                graph.positions[i * 3 + 1],
                graph.positions[i * 3 + 2],
                0.0,
            ]);
        }
        // Ensure non-empty (wgpu rejects zero-sized storage buffers).
        if positions_padded.is_empty() {
            positions_padded.extend_from_slice(&[0.0; 4]);
        }
        let positions_storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_storage"),
            contents: bytemuck::cast_slice(&positions_padded),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });

        // colors: vec4 per node (already 16-byte aligned).
        let mut colors = graph.colors.clone();
        if colors.is_empty() {
            colors.extend_from_slice(&[0.0; 4]);
        }
        let colors_storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("colors_storage"),
            contents: bytemuck::cast_slice(&colors),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        // sizes: f32 per node — std430 array stride = 4.
        let mut sizes = graph.sizes.clone();
        if sizes.is_empty() {
            sizes.push(0.0);
        }
        let sizes_storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sizes_storage"),
            contents: bytemuck::cast_slice(&sizes),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        // edges: vec2<u32> per edge. WGSL std430 stride for vec2<u32> is 8.
        let mut edges_packed: Vec<u32> = graph.edges.clone();
        if edges_packed.is_empty() {
            edges_packed.extend_from_slice(&[0, 0]);
        }
        let edges_storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edges_storage"),
            contents: bytemuck::cast_slice(&edges_packed),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        // --- Bind group layouts ---
        // Node pipeline: camera + effects + positions + colors + sizes
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

        let node_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("node bg"),
            layout: &node_bgl,
            entries: &[
                bg_entry(0, &camera_uniform_buffer),
                bg_entry(1, &effects_uniform_buffer),
                bg_entry(2, &positions_storage),
                bg_entry(3, &colors_storage),
                bg_entry(4, &sizes_storage),
            ],
        });
        let edge_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edge bg"),
            layout: &edge_bgl,
            entries: &[
                bg_entry(0, &camera_uniform_buffer),
                bg_entry(1, &effects_uniform_buffer),
                bg_entry(2, &positions_storage),
                bg_entry(3, &edges_storage),
            ],
        });

        // --- Pipelines ---
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
                    format: surface_config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
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
                    format: surface_config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            depth_view,
            node_pipeline,
            edge_pipeline,
            positions_storage,
            colors_storage,
            sizes_storage,
            edges_storage,
            n_nodes,
            n_edges,
            camera_uniform_buffer,
            effects_uniform_buffer,
            node_bind_group,
            edge_bind_group,
            node_positions_cpu: graph.positions,
            node_sizes_cpu: graph.sizes,
            edges_cpu: graph.edges,
            layout: None,
            layout_initialized: false,
            camera,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.surface_config.width = w;
        self.surface_config.height = h;
        self.surface.configure(&self.device, &self.surface_config);
        self.depth_view = create_depth_view(&self.device, w, h);
        self.camera.aspect = w as f32 / h as f32;
    }

    /// Update positions in the shared storage buffer (CPU-driven path; skip
    /// when the live compute layout is running). Padded vec4 layout.
    pub fn update_positions(&mut self, positions: &[f32]) {
        // Re-pad to vec4.
        let mut padded: Vec<f32> = Vec::with_capacity(self.n_nodes as usize * 4);
        for i in 0..self.n_nodes as usize {
            padded.extend_from_slice(&[
                positions[i * 3],
                positions[i * 3 + 1],
                positions[i * 3 + 2],
                0.0,
            ]);
        }
        self.queue
            .write_buffer(&self.positions_storage, 0, bytemuck::cast_slice(&padded));
        self.node_positions_cpu = positions.to_vec();
    }

    pub fn update_colors(&mut self, colors: &[f32]) {
        self.queue
            .write_buffer(&self.colors_storage, 0, bytemuck::cast_slice(colors));
    }

    pub fn update_sizes(&mut self, sizes: &[f32]) {
        self.queue
            .write_buffer(&self.sizes_storage, 0, bytemuck::cast_slice(sizes));
        self.node_sizes_cpu = sizes.to_vec();
    }

    pub fn set_focus_plane(&mut self, z: f32, thickness: f32) {
        let eff = EffectsUniform {
            focus_plane_z: z,
            focus_thickness: thickness.max(1.0),
            cursor_radius_visual: 0.0,
            _pad: 0.0,
        };
        self.queue
            .write_buffer(&self.effects_uniform_buffer, 0, bytemuck::bytes_of(&eff));
    }

    pub fn cam_fit_bounds(&mut self, min: Vec3, max: Vec3) {
        self.camera.fit_to_bounds(min, max);
    }

    /// Build a `graph_layouts::Graph` from the renderer-side data and bind
    /// the `GpuForceLayout` to this renderer's device + queue + shared
    /// positions buffer.
    pub fn init_layout(
        &mut self,
        edges: &[u32],
        options: GpuForceOptions,
    ) -> Result<(), String> {
        // Build a synthetic id-keyed graph. Ids are decimal strings so the
        // sorted ordering used by GpuForceLayout is just numeric ascending.
        // To make sorted-string-of-int order match numeric order (so
        // index `i` in our renderer == index `i` in compute), we
        // zero-pad to a fixed width.
        let n = self.n_nodes as usize;
        let width = format!("{}", n.max(1) - 1).len().max(1);

        let mut g = GlGraph::new();
        // Seed each node with its current 3D position so the compute layer
        // doesn't randomise on first init. (positions cache holds 3 f32/node)
        for i in 0..n {
            let id = format!("{:0width$}", i, width = width);
            let mut node = GlNode::new(id.clone());
            if i * 3 + 2 < self.node_positions_cpu.len() {
                node.position3 = Some([
                    self.node_positions_cpu[i * 3],
                    self.node_positions_cpu[i * 3 + 1],
                    self.node_positions_cpu[i * 3 + 2],
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
            let eid = format!("e{}", e_i);
            g.add_edge(GlEdge::new(eid, sid, tid));
        }

        let mut layout = GpuForceLayout::new(options);
        layout.init_with_device(&self.device, &self.queue, &g, &self.positions_storage)?;
        self.layout = Some(layout);
        self.layout_initialized = true;
        Ok(())
    }

    pub fn update_layout_options(&mut self, options: GpuForceOptions) {
        if let Some(l) = self.layout.as_mut() {
            l.set_options(options);
        }
    }

    /// True once the GPU-force layout has settled (max-KE under the
    /// configured `energy_threshold` for `HALT_FRAMES` consecutive readbacks).
    /// Always false if no layout is initialised.
    pub fn sim_halted(&self) -> bool {
        self.layout.as_ref().map(|l| l.is_halted()).unwrap_or(false)
    }

    /// Wake a halted layout. Call from any path that perturbs the sim
    /// (cursor force, preset switch, slider drag).
    pub fn sim_wake(&mut self) {
        if let Some(l) = self.layout.as_mut() {
            l.wake();
        }
    }

    /// Most recent observed max kinetic-energy proxy (|vel|^2). 0 before
    /// the first readback completes.
    pub fn sim_max_ke(&self) -> f32 {
        self.layout.as_ref().map(|l| l.last_max_ke()).unwrap_or(0.0)
    }

    /// One-shot frame: optionally step the layout, then render.
    pub fn step(&mut self) -> Result<(), wgpu::SurfaceError> {
        // Update camera uniform.
        let cam_uniform = CameraUniform {
            view_proj: self.camera.view_proj(),
            cam_pos: self.camera.position.to_array(),
            _pad0: 0.0,
            screen: [
                self.surface_config.width as f32,
                self.surface_config.height as f32,
            ],
            _pad1: [0.0, 0.0],
        };
        self.queue.write_buffer(
            &self.camera_uniform_buffer,
            0,
            bytemuck::bytes_of(&cam_uniform),
        );

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        // Compute step (in the same encoder, before rendering — wgpu
        // serialises commands within an encoder so the render pass sees the
        // post-compute state).
        if let Some(l) = self.layout.as_mut() {
            l.step_with_encoder(&self.device, &self.queue, &mut encoder, &self.positions_storage);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.06,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Edges first (no depth write), nodes on top.
            if self.n_edges > 0 {
                pass.set_pipeline(&self.edge_pipeline);
                pass.set_bind_group(0, &self.edge_bind_group, &[]);
                pass.draw(0..(self.n_edges * 2), 0..1);
            }

            if self.n_nodes > 0 {
                pass.set_pipeline(&self.node_pipeline);
                pass.set_bind_group(0, &self.node_bind_group, &[]);
                pass.draw(0..6, 0..self.n_nodes);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Backward-compatible alias for `step()` so the native binary's
    /// `r.render()` call site keeps working.
    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.step()
    }

    /// Naive sphere raycast on CPU. Returns nearest hit node index. Note:
    /// with the live force sim running, the CPU position cache is only a
    /// snapshot from the most recent `update_positions` call; for accurate
    /// picking under live sim, call `read_back_positions` first.
    pub fn raycast(&self, ndc_x: f32, ndc_y: f32) -> Option<u32> {
        let (origin, dir) = self.camera.raycast(ndc_x, ndc_y);
        let mut best: Option<(f32, u32)> = None;
        // Convert pixel sizes to world-space radius at each node's depth.
        // px_world = (2 * tan(fov/2) * dist_to_camera) / screen_h * px_size
        let tan_half = (self.camera.fov_y * 0.5).tan();
        let screen_h = self.surface_config.height.max(1) as f32;
        let two_tan_over_h = 2.0 * tan_half / screen_h;
        for i in 0..self.n_nodes as usize {
            if i * 3 + 2 >= self.node_positions_cpu.len() {
                break;
            }
            let center = Vec3::new(
                self.node_positions_cpu[i * 3],
                self.node_positions_cpu[i * 3 + 1],
                self.node_positions_cpu[i * 3 + 2],
            );
            let px = self.node_sizes_cpu.get(i).copied().unwrap_or(4.0);
            let dist = (center - origin).length().max(1.0);
            let r = (two_tan_over_h * dist * px).max(1.0);
            let oc = origin - center;
            let b = oc.dot(dir);
            let c = oc.dot(oc) - r * r;
            let disc = b * b - c;
            if disc < 0.0 {
                continue;
            }
            let t = -b - disc.sqrt();
            if t > 0.0 && best.map(|(bt, _)| t < bt).unwrap_or(true) {
                best = Some((t, i as u32));
            }
        }
        best.map(|(_, i)| i)
    }

    pub fn n_nodes(&self) -> u32 {
        self.n_nodes
    }
    pub fn n_edges(&self) -> u32 {
        self.n_edges
    }
    pub fn edges_cpu(&self) -> &[u32] {
        &self.edges_cpu
    }
    pub fn positions_cpu(&self) -> &[f32] {
        &self.node_positions_cpu
    }
}

fn create_depth_view(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
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

