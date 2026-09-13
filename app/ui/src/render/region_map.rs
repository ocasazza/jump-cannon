//! GMap-style cluster region underlay, computed entirely on the GPU.
//!
//! The graph plane is tiled into per-node Voronoi cells over a fixed
//! [`GRID_SIZE`]x[`GRID_SIZE`] screen-space grid via jump-flooding, cells
//! sharing a cluster id merge into a region, and cells farther than
//! `radius_cells` from every node become ocean (transparent). This keeps
//! the graph readable at node counts too large to draw: the per-frame
//! cost is one O(n) seed pass, a fixed number of jump-flood passes over
//! [`GRID_SIZE`]^2 cells, and one fullscreen draw — independent of n
//! except for the seed pass.
//!
//! See `shaders/region_map.wgsl` for the kernels and the packed-seed
//! encoding. The grid is stored as two separate `array<atomic<u32>>`
//! buffers per ping-pong slot (seed + cluster) because WGSL forbids
//! `vec<atomic<u32>>`; this lets the seed pass resolve same-cell races
//! deterministically with `atomicMin`.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// Region grid resolution. Fixed so the jump-flood cost is constant.
pub const GRID_SIZE: u32 = 512;

/// Jump-flood step schedule: halving from GRID_SIZE/2 down to 1, plus one
/// extra pass at k=1 (Rong & Tan's "1+JFA" refinement that cleans up the
/// small errors a plain halving schedule leaves behind).
const JFA_STEPS: [u32; 10] = [256, 128, 64, 32, 16, 8, 4, 2, 1, 1];

/// Compute-kernel width; matches the shader's `@workgroup_size(64)`.
const WORKGROUP_SIZE: u32 = 64;

/// WebGPU's per-dimension workgroup cap (see gpu_force.rs). Dispatches
/// wider than this spill into the Y dimension; the shader recovers its
/// lane index through `linear_index`.
const MAX_WORKGROUPS_PER_DIM: u32 = 65535;

/// How regions are drawn relative to the node/edge layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RegionMode {
    /// No region map: draw nothing and record no compute.
    #[default]
    Off,
    /// Regions drawn beneath edges and nodes.
    Underlay,
    /// Regions only; edges and nodes are skipped.
    Only,
}

impl RegionMode {
    fn as_u32(self) -> u32 {
        match self {
            RegionMode::Off => 0,
            RegionMode::Underlay => 1,
            RegionMode::Only => 2,
        }
    }
}

/// Which dendrogram level of the uploaded clustering the region underlay
/// renders. Level 0 is the coarsest and `n_levels - 1` the finest. `Auto`
/// picks a level from the camera framing (see the auto rule in
/// [`auto_level`]); `Fixed` pins one (clamped to `n_levels - 1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RegionLevel {
    #[default]
    Auto,
    Fixed(u32),
}

/// Tunable appearance + behaviour of the region underlay.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionMapConfig {
    pub mode: RegionMode,
    /// Dendrogram level selector for the region ids (see [`RegionLevel`]).
    pub level: RegionLevel,
    /// Max distance (region-grid cells; the grid is [`GRID_SIZE`] square)
    /// from a node for a cell to belong to a region; beyond it the cell
    /// is "ocean" (transparent).
    pub radius_cells: f32,
    /// Fill alpha of region interiors, 0..1.
    pub fill_alpha: f32,
    pub outline: bool,
    /// RGBA swatches cycled by cluster id (id % len). Non-empty.
    pub palette: Vec<[f32; 4]>,
}

impl Default for RegionMapConfig {
    fn default() -> Self {
        Self {
            mode: RegionMode::Off,
            level: RegionLevel::Auto,
            radius_cells: 24.0,
            fill_alpha: 0.35,
            outline: true,
            // One neutral grey swatch: with all-zero cluster ids the whole
            // cloud reads as a single translucent region.
            palette: vec![[0.5, 0.5, 0.5, 1.0]],
        }
    }
}

/// Mirrors `RegionParams` in region_map.wgsl (48 bytes, 16-byte aligned).
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct RegionParams {
    grid_size: u32,
    radius_cells: f32,
    fill_alpha: f32,
    outline: u32,
    n_nodes: u32,
    palette_len: u32,
    mode: u32,
    /// Active dendrogram level; the seed kernel reads
    /// `cluster_ids[level * n_nodes + i]`.
    level: u32,
    /// Number of uploaded levels (>= 1). Level stride for `cluster_ids`.
    n_levels: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// Mirrors `StepParams` in region_map.wgsl. One slot per jump-flood pass,
/// bound with a dynamic uniform offset so all passes share one buffer.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct StepUniform {
    step: u32,
    src_is_a: u32,
    _p0: u32,
    _p1: u32,
}

/// All wgpu state for the region-map underlay. Owns the ping-pong grids,
/// the params/step/palette buffers, and the compute + draw pipelines; the
/// shared positions / camera / cluster-id buffers are referenced through
/// [`RegionMap::bind`].
pub struct RegionMap {
    cfg: RegionMapConfig,

    compute_bgl: wgpu::BindGroupLayout,
    render_bgl: wgpu::BindGroupLayout,

    clear_pipeline: wgpu::ComputePipeline,
    seed_pipeline: wgpu::ComputePipeline,
    jfa_pipeline: wgpu::ComputePipeline,
    prune_pipeline: wgpu::ComputePipeline,
    draw_pipeline: wgpu::RenderPipeline,

    grid_a_seed: wgpu::Buffer,
    grid_a_cluster: wgpu::Buffer,
    grid_b_seed: wgpu::Buffer,
    grid_b_cluster: wgpu::Buffer,

    params_buf: wgpu::Buffer,
    /// Byte stride between step-uniform slots (rounded up to the device's
    /// min dynamic-uniform-offset alignment).
    step_stride: u32,

    palette_buf: wgpu::Buffer,
    /// Number of swatches currently uploaded (>= 1).
    palette_len: u32,

    /// Compute bind group over the shared + owned buffers. `None` until
    /// [`RegionMap::bind`] runs (i.e. before the graph is loaded).
    compute_bg: Option<wgpu::BindGroup>,
    /// Step bind group (dynamic offset). Built at construction.
    step_bg: wgpu::BindGroup,
    /// Draw bind group over params + grid_a + palette. Rebuilt when the
    /// palette buffer is recreated.
    render_bg: wgpu::BindGroup,

    n_nodes: u32,
    /// Number of uploaded dendrogram levels (>= 1). Level stride for the
    /// shared `cluster_ids` buffer.
    n_levels: u32,
    /// Level chosen for the last encoded frame. Returned by
    /// [`RegionMap::region_current_level`] and mirrored into the params
    /// uniform read by the seed kernel.
    current_level: u32,
}

impl RegionMap {
    /// Build pipelines and owned buffers against the render device. The
    /// compute bind group is deferred to [`RegionMap::bind`] (it needs the
    /// shared positions / camera / cluster-id buffers, created in
    /// `GraphPipelines::load`).
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let cfg = RegionMapConfig::default();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("region_map.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/region_map.wgsl").into()),
        });

        let compute_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("region compute bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::COMPUTE, false),
                uniform_entry(1, wgpu::ShaderStages::COMPUTE, false),
                storage_entry(2, wgpu::ShaderStages::COMPUTE, true),
                storage_entry(3, wgpu::ShaderStages::COMPUTE, true),
                storage_entry(4, wgpu::ShaderStages::COMPUTE, false),
                storage_entry(5, wgpu::ShaderStages::COMPUTE, false),
                storage_entry(6, wgpu::ShaderStages::COMPUTE, false),
                storage_entry(7, wgpu::ShaderStages::COMPUTE, false),
            ],
        });
        let step_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("region step bgl"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::COMPUTE, true)],
        });
        // Draw reads the grid (read_write storage, accessed via atomicLoad
        // — legal in the fragment stage) plus params and the palette.
        let render_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("region render bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT, false),
                storage_entry(4, wgpu::ShaderStages::FRAGMENT, false),
                storage_entry(5, wgpu::ShaderStages::FRAGMENT, false),
                storage_entry(8, wgpu::ShaderStages::FRAGMENT, true),
            ],
        });

        let compute_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("region compute pl"),
            bind_group_layouts: &[&compute_bgl],
            push_constant_ranges: &[],
        });
        let jfa_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("region jfa pl"),
            bind_group_layouts: &[&compute_bgl, &step_bgl],
            push_constant_ranges: &[],
        });
        let render_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("region render pl"),
            bind_group_layouts: &[&render_bgl],
            push_constant_ranges: &[],
        });

        let make_compute = |label: &str, layout: &wgpu::PipelineLayout, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let clear_pipeline = make_compute("region_clear", &compute_pl, "region_clear");
        let seed_pipeline = make_compute("region_seed", &compute_pl, "region_seed");
        let jfa_pipeline = make_compute("region_jfa", &jfa_pl, "region_jfa");
        let prune_pipeline = make_compute("region_prune", &compute_pl, "region_prune");

        let draw_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("region draw"),
            layout: Some(&render_pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("region_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("region_fs"),
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

        // Ping-pong grids. Cleared/overwritten every frame, so no init.
        let grid_bytes = (GRID_SIZE as u64) * (GRID_SIZE as u64) * 4;
        let make_grid = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: grid_bytes,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        let grid_a_seed = make_grid("region grid_a_seed");
        let grid_a_cluster = make_grid("region grid_a_cluster");
        let grid_b_seed = make_grid("region grid_b_seed");
        let grid_b_cluster = make_grid("region grid_b_cluster");

        let palette_data = palette_floats(&cfg.palette);
        let palette_len = (palette_data.len() / 4) as u32;
        let palette_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("region palette"),
            contents: bytemuck::cast_slice(&palette_data),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let params = RegionParams {
            grid_size: GRID_SIZE,
            radius_cells: cfg.radius_cells,
            fill_alpha: cfg.fill_alpha,
            outline: cfg.outline as u32,
            n_nodes: 0,
            palette_len,
            mode: cfg.mode.as_u32(),
            level: 0,
            n_levels: 1,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("region params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Dynamic uniform offsets must be a multiple of the device's
        // min alignment; pack one 16-byte StepUniform per aligned slot.
        let align = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(std::mem::size_of::<StepUniform>() as u32);
        let step_stride = align;
        let mut step_data = vec![0u8; step_stride as usize * JFA_STEPS.len()];
        for (i, &step) in JFA_STEPS.iter().enumerate() {
            // Pass 0 reads grid_a, writes grid_b; passes then alternate. An
            // even number of passes (10) lands the final result back in
            // grid_a, which `region_prune` and the draw shader read.
            let src_is_a = if i % 2 == 0 { 1 } else { 0 };
            let u = StepUniform {
                step,
                src_is_a,
                _p0: 0,
                _p1: 0,
            };
            let off = i * step_stride as usize;
            step_data[off..off + std::mem::size_of::<StepUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&u));
        }
        let step_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("region step"),
            contents: &step_data,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let step_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("region step bg"),
            layout: &step_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &step_buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<StepUniform>() as u64),
                }),
            }],
        });

        let render_bg = Self::make_render_bg(
            device,
            &render_bgl,
            &params_buf,
            &grid_a_seed,
            &grid_a_cluster,
            &palette_buf,
        );

        Self {
            cfg,
            compute_bgl,
            render_bgl,
            clear_pipeline,
            seed_pipeline,
            jfa_pipeline,
            prune_pipeline,
            draw_pipeline,
            grid_a_seed,
            grid_a_cluster,
            grid_b_seed,
            grid_b_cluster,
            params_buf,
            step_stride,
            palette_buf,
            palette_len,
            compute_bg: None,
            step_bg,
            render_bg,
            n_nodes: 0,
            n_levels: 1,
            current_level: 0,
        }
    }

    fn make_render_bg(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        params: &wgpu::Buffer,
        grid_a_seed: &wgpu::Buffer,
        grid_a_cluster: &wgpu::Buffer,
        palette: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("region render bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: grid_a_seed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: grid_a_cluster.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: palette.as_entire_binding(),
                },
            ],
        })
    }

    /// Point the compute bind group at the shared positions / camera /
    /// cluster-id buffers and record the node count. Call from
    /// `GraphPipelines::load` once those buffers exist.
    pub fn bind(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        positions: &wgpu::Buffer,
        camera_uniform: &wgpu::Buffer,
        cluster_ids: &wgpu::Buffer,
        n_nodes: u32,
    ) {
        self.n_nodes = n_nodes;
        self.compute_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("region compute bg"),
            layout: &self.compute_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: camera_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: positions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: cluster_ids.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.grid_a_seed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.grid_a_cluster.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.grid_b_seed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: self.grid_b_cluster.as_entire_binding(),
                },
            ],
        }));
        // n_nodes lives in the params uniform (the seed pass bounds-checks
        // against it), so keep it in sync with the loaded graph.
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params()));
    }

    /// Apply a new configuration: re-upload the palette (recreating the
    /// buffer if its length changed) and rewrite the params uniform.
    pub fn set_config(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, cfg: RegionMapConfig) {
        let palette_data = palette_floats(&cfg.palette);
        let new_len = (palette_data.len() / 4) as u32;
        if new_len != self.palette_len {
            self.palette_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("region palette"),
                contents: bytemuck::cast_slice(&palette_data),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });
            self.palette_len = new_len;
            // The draw bind group references the palette buffer; rebuild it.
            self.render_bg = Self::make_render_bg(
                device,
                &self.render_bgl,
                &self.params_buf,
                &self.grid_a_seed,
                &self.grid_a_cluster,
                &self.palette_buf,
            );
        } else {
            queue.write_buffer(&self.palette_buf, 0, bytemuck::cast_slice(&palette_data));
        }
        self.cfg = cfg;
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params()));
    }

    pub fn config(&self) -> &RegionMapConfig {
        &self.cfg
    }

    /// The level chosen for the most recently encoded frame.
    pub fn region_current_level(&self) -> u32 {
        self.current_level
    }

    /// Record the uploaded dendrogram depth (>= 1). Re-clamps the stored
    /// current level into range and rewrites the params uniform so the
    /// seed kernel's level stride matches the freshly uploaded ids.
    pub fn set_levels(&mut self, queue: &wgpu::Queue, n_levels: u32) {
        self.n_levels = n_levels.max(1);
        if self.current_level >= self.n_levels {
            self.current_level = self.n_levels - 1;
        }
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params()));
    }

    /// Resolve the level for this frame from the config's [`RegionLevel`]
    /// and the current camera framing, store it, and rewrite the params
    /// uniform when it changed. `framing` is `Some((d, fit_dist))` — the
    /// camera-to-bounds-centre distance and the distance `fit_to_bounds`
    /// would choose for the bounds radius — or `None` when no bounds exist
    /// (Auto then resolves to level 0). Called once per encoded frame.
    pub fn update_level(&mut self, queue: &wgpu::Queue, framing: Option<(f32, f32)>) {
        let max_level = self.n_levels.saturating_sub(1);
        let level = match self.cfg.level {
            RegionLevel::Fixed(k) => k.min(max_level),
            RegionLevel::Auto => match framing {
                Some((d, fit_dist)) => auto_level(self.n_levels, d, fit_dist),
                None => 0,
            },
        };
        if level != self.current_level {
            self.current_level = level;
            queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params()));
        }
    }

    fn params(&self) -> RegionParams {
        RegionParams {
            grid_size: GRID_SIZE,
            radius_cells: self.cfg.radius_cells,
            fill_alpha: self.cfg.fill_alpha,
            outline: self.cfg.outline as u32,
            n_nodes: self.n_nodes,
            palette_len: self.palette_len.max(1),
            mode: self.cfg.mode.as_u32(),
            level: self.current_level,
            n_levels: self.n_levels,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        }
    }

    /// Record the clear -> seed -> jump-flood -> prune compute passes.
    /// No-op when the mode is `Off` or before [`RegionMap::bind`]. Each
    /// stage is its own compute pass so the implicit inter-pass barrier
    /// orders the reads-after-writes the jump-flood depends on.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.cfg.mode == RegionMode::Off {
            return;
        }
        let Some(compute_bg) = self.compute_bg.as_ref() else {
            return;
        };
        let cells = GRID_SIZE * GRID_SIZE;

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("region clear"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.clear_pipeline);
            pass.set_bind_group(0, compute_bg, &[]);
            dispatch_1d(&mut pass, cells);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("region seed"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.seed_pipeline);
            pass.set_bind_group(0, compute_bg, &[]);
            dispatch_1d(&mut pass, self.n_nodes);
        }
        for i in 0..JFA_STEPS.len() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("region jfa"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.jfa_pipeline);
            pass.set_bind_group(0, compute_bg, &[]);
            pass.set_bind_group(1, &self.step_bg, &[i as u32 * self.step_stride]);
            dispatch_1d(&mut pass, cells);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("region prune"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.prune_pipeline);
            pass.set_bind_group(0, compute_bg, &[]);
            dispatch_1d(&mut pass, cells);
        }
    }

    /// Record the fullscreen region draw. No-op when the mode is `Off`.
    pub fn draw(&self, rpass: &mut wgpu::RenderPass<'_>) {
        if self.cfg.mode == RegionMode::Off {
            return;
        }
        rpass.set_pipeline(&self.draw_pipeline);
        rpass.set_bind_group(0, &self.render_bg, &[]);
        rpass.draw(0..3, 0..1);
    }
}

/// Flatten swatches to a float array, substituting one neutral grey when
/// empty so the palette storage buffer is never zero-length (WGSL indexes
/// it with `id % palette_len`).
fn palette_floats(palette: &[[f32; 4]]) -> Vec<f32> {
    if palette.is_empty() {
        return vec![0.5, 0.5, 0.5, 1.0];
    }
    let mut out = Vec::with_capacity(palette.len() * 4);
    for c in palette {
        out.extend_from_slice(c);
    }
    out
}

/// Shared zoom-to-level rule for [`RegionLevel::Auto`]. `d` is the camera
/// distance to the bounds centre and `fit_dist` the distance
/// [`crate::render::camera::Camera::fit_to_bounds`] would place the camera
/// at for the bounds radius, so `r = d / fit_dist` is 1 at the fitted view
/// and shrinks as the camera dollies in. The dendrogram is ordered with
/// level 0 the COARSEST (byte-identical to `community`) and level
/// `n_levels - 1` the finest. `r >= 1` yields level 0; each halving of `r`
/// steps one level finer: `level = min(floor(log2(1 / r)), n_levels - 1)`.
fn auto_level(n_levels: u32, d: f32, fit_dist: f32) -> u32 {
    let finest = n_levels.saturating_sub(1);
    if !(fit_dist > 0.0) || !d.is_finite() {
        return 0;
    }
    let r = d / fit_dist;
    if !(r > 0.0) || r >= 1.0 {
        return 0;
    }
    let steps = (1.0 / r).log2().floor().max(0.0) as u32;
    steps.min(finest)
}

fn uniform_entry(
    binding: u32,
    vis: wgpu::ShaderStages,
    has_dynamic_offset: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(
    binding: u32,
    vis: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Dispatch `invocations` lanes of a 64-wide kernel, spilling into Y once
/// X would exceed the per-dimension cap. Mirrors `dispatch_1d` in
/// gpu_force.rs; the shader recovers its index through `linear_index`.
fn dispatch_1d(pass: &mut wgpu::ComputePass<'_>, invocations: u32) {
    let (x, y) = dispatch_grid(invocations);
    pass.dispatch_workgroups(x, y, 1);
}

fn dispatch_grid(invocations: u32) -> (u32, u32) {
    let groups = invocations.div_ceil(WORKGROUP_SIZE).max(1);
    let x = groups.min(MAX_WORKGROUPS_PER_DIM);
    let y = groups.div_ceil(x);
    (x, y)
}
