//! Target-agnostic wgpu state. Constructor takes an already-created
//! `wgpu::Surface`, so the native (winit window) and web (canvas) paths can
//! both feed it.

use crate::camera::Camera;
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
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
    _pad: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct SphereVertex {
    position: [f32; 3],
    normal: [f32; 3],
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,

    node_pipeline: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,

    node_vertex_buffer: wgpu::Buffer,
    node_index_buffer: wgpu::Buffer,
    node_index_count: u32,

    instance_position_buffer: wgpu::Buffer,
    instance_color_buffer: wgpu::Buffer,
    instance_size_buffer: wgpu::Buffer,
    n_nodes: u32,

    edge_position_buffer: wgpu::Buffer,
    n_edges: u32,

    camera_uniform_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    // Cache of node positions + sizes for raycasting on CPU.
    node_positions_cpu: Vec<f32>,
    node_sizes_cpu: Vec<f32>,

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
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults()
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

        // --- Camera uniform + bind group ---
        let camera = Camera::new(surface_config.width as f32 / surface_config.height as f32);
        let cam_uniform = CameraUniform {
            view_proj: camera.view_proj(),
            cam_pos: camera.position.to_array(),
            _pad: 0.0,
        };
        let camera_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera uniform"),
            contents: bytemuck::bytes_of(&cam_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_uniform_buffer.as_entire_binding(),
            }],
        });

        // --- Sphere geometry (icosphere subdivided once-ish; for size, just an icosahedron) ---
        let (sphere_vertices, sphere_indices) = build_icosphere();
        let node_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sphere vertices"),
            contents: bytemuck::cast_slice(&sphere_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let node_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sphere indices"),
            contents: bytemuck::cast_slice(&sphere_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let node_index_count = sphere_indices.len() as u32;

        // --- Per-instance buffers ---
        let n_nodes = (graph.positions.len() / 3) as u32;
        let instance_position_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("instance positions"),
                contents: bytemuck::cast_slice(&graph.positions),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
        let instance_color_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("instance colors"),
            contents: bytemuck::cast_slice(&graph.colors),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let instance_size_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("instance sizes"),
            contents: bytemuck::cast_slice(&graph.sizes),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        // --- Edge positions (unrolled to vertex pairs) ---
        let n_edges = (graph.edges.len() / 2) as u32;
        let mut edge_positions = Vec::with_capacity((n_edges * 2 * 3) as usize);
        for i in 0..n_edges as usize {
            let a = graph.edges[i * 2] as usize;
            let b = graph.edges[i * 2 + 1] as usize;
            edge_positions.extend_from_slice(&[
                graph.positions[a * 3],
                graph.positions[a * 3 + 1],
                graph.positions[a * 3 + 2],
                graph.positions[b * 3],
                graph.positions[b * 3 + 1],
                graph.positions[b * 3 + 2],
            ]);
        }
        let edge_position_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge positions"),
            contents: bytemuck::cast_slice(&edge_positions),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[&camera_bgl],
            push_constant_ranges: &[],
        });

        let node_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("node pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &node_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    // Sphere vertex (pos, normal)
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SphereVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                    },
                    // Instance position (vec3)
                    wgpu::VertexBufferLayout {
                        array_stride: 12,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![2 => Float32x3],
                    },
                    // Instance color (vec4)
                    wgpu::VertexBufferLayout {
                        array_stride: 16,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![3 => Float32x4],
                    },
                    // Instance size (f32)
                    wgpu::VertexBufferLayout {
                        array_stride: 4,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![4 => Float32],
                    },
                ],
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
                depth_write_enabled: true,
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
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &edge_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                }],
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
            node_vertex_buffer,
            node_index_buffer,
            node_index_count,
            instance_position_buffer,
            instance_color_buffer,
            instance_size_buffer,
            n_nodes,
            edge_position_buffer,
            n_edges,
            camera_uniform_buffer,
            camera_bind_group,
            node_positions_cpu: graph.positions,
            node_sizes_cpu: graph.sizes,
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

    pub fn update_positions(&mut self, positions: &[f32]) {
        self.queue
            .write_buffer(&self.instance_position_buffer, 0, bytemuck::cast_slice(positions));
        self.node_positions_cpu = positions.to_vec();
        // Edge buffer would also need updating from edges list — left for Wave 2.
    }

    pub fn update_colors(&mut self, colors: &[f32]) {
        self.queue
            .write_buffer(&self.instance_color_buffer, 0, bytemuck::cast_slice(colors));
    }

    pub fn update_sizes(&mut self, sizes: &[f32]) {
        self.queue
            .write_buffer(&self.instance_size_buffer, 0, bytemuck::cast_slice(sizes));
        self.node_sizes_cpu = sizes.to_vec();
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        // Update camera uniform.
        let cam_uniform = CameraUniform {
            view_proj: self.camera.view_proj(),
            cam_pos: self.camera.position.to_array(),
            _pad: 0.0,
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
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, self.edge_position_buffer.slice(..));
                pass.draw(0..(self.n_edges * 2), 0..1);
            }

            if self.n_nodes > 0 {
                pass.set_pipeline(&self.node_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, self.node_vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_position_buffer.slice(..));
                pass.set_vertex_buffer(2, self.instance_color_buffer.slice(..));
                pass.set_vertex_buffer(3, self.instance_size_buffer.slice(..));
                pass.set_index_buffer(self.node_index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.node_index_count, 0, 0..self.n_nodes);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Naive sphere raycast on CPU. Returns nearest hit node index.
    pub fn raycast(&self, ndc_x: f32, ndc_y: f32) -> Option<u32> {
        let (origin, dir) = self.camera.raycast(ndc_x, ndc_y);
        let mut best: Option<(f32, u32)> = None;
        for i in 0..self.n_nodes as usize {
            let center = Vec3::new(
                self.node_positions_cpu[i * 3],
                self.node_positions_cpu[i * 3 + 1],
                self.node_positions_cpu[i * 3 + 2],
            );
            let r = self.node_sizes_cpu.get(i).copied().unwrap_or(1.0);
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

/// 12-vertex / 20-tri icosahedron — small enough for instanced spheres
/// without subdivision, big enough not to look like a triangle.
fn build_icosphere() -> (Vec<SphereVertex>, Vec<u32>) {
    let t = (1.0_f32 + 5.0_f32.sqrt()) * 0.5;
    let raw: [[f32; 3]; 12] = [
        [-1.0, t, 0.0], [1.0, t, 0.0], [-1.0, -t, 0.0], [1.0, -t, 0.0],
        [0.0, -1.0, t], [0.0, 1.0, t], [0.0, -1.0, -t], [0.0, 1.0, -t],
        [t, 0.0, -1.0], [t, 0.0, 1.0], [-t, 0.0, -1.0], [-t, 0.0, 1.0],
    ];
    let verts: Vec<SphereVertex> = raw
        .iter()
        .map(|p| {
            let v = Vec3::from_array(*p).normalize();
            SphereVertex {
                position: v.to_array(),
                normal: v.to_array(),
            }
        })
        .collect();
    let indices: Vec<u32> = vec![
        0, 11, 5, 0, 5, 1, 0, 1, 7, 0, 7, 10, 0, 10, 11,
        1, 5, 9, 5, 11, 4, 11, 10, 2, 10, 7, 6, 7, 1, 8,
        3, 9, 4, 3, 4, 2, 3, 2, 6, 3, 6, 8, 3, 8, 9,
        4, 9, 5, 2, 4, 11, 6, 2, 10, 8, 6, 7, 9, 8, 1,
    ];
    (verts, indices)
}
