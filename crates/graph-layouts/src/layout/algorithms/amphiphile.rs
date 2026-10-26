//! Amphiphile self-assembly: a coarse-grained lipid model on the GPU.
//!
//! Every particle is a [`Species`] — head, tail or solvent — and the only
//! structure in the law is the species-pair attraction table. Tails attract
//! tails (the hydrophobic effect), heads mildly attract heads and solvent,
//! and the mixed pairs are repulsive-only, so from a random start the tails
//! gather, the heads face the solvent, and micelles, bilayer sheets, tubes
//! and closed vesicles emerge. Bonds are emergent: the CSR edge list is
//! unused unless `bond_k > 0` asks for intra-molecule springs.
//!
//! The pair law (`shaders/amphiphile.wgsl`): a linear soft core below
//! `sigma` and the Cooke–Deserno cos² well between `sigma` and `cutoff`,
//! `F = -ε sin(π (r - σ) / (r_c - σ))`, zero at both ends and bounded by ε.
//! Semi-implicit Euler with damping, a velocity clamp of one `sigma` per
//! step, Brownian noise from a per-particle PCG stream, and a weak pull to
//! the origin that keeps the open box bounded.

use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

use crate::layout::algorithms::gpu_force::{futures_channel, CsrInput};
use crate::layout::layout_trait::{LayoutDescriptor, LayoutKind, LayoutRequirements, PhysicsLayout};
use crate::types::Graph;

/// Particle species of the coarse-grained lipid model.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Species {
    /// Hydrophilic head: faces the solvent.
    Head = 0,
    /// Hydrophobic tail: gathers with other tails.
    Tail = 1,
    /// Solvent: the medium.
    Solvent = 2,
}

/// Tuning of the amphiphile simulation, in world units.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AmphiphileOptions {
    /// Integration step.
    pub dt: f32,
    /// Velocity kept per step.
    pub damping: f32,
    /// Brownian force amplitude; 0 is deterministic.
    pub temperature: f32,
    /// Pair interaction range.
    pub cutoff: f32,
    /// Particle diameter: the soft core's extent and the well's start.
    pub sigma: f32,
    /// Well depth per `(species_i, species_j)`; 0 or negative is repulsive-only.
    pub epsilon: [[f32; 3]; 3],
    /// Spring stiffness on CSR edges; 0 ignores the edges.
    pub bond_k: f32,
    /// Rest length of those springs.
    pub bond_len: f32,
    /// Pull toward the origin per world unit.
    pub gravity: f32,
    /// Simulation steps per `step_with_encoder`.
    pub steps_per_call: u32,
    /// Seed of the Brownian streams.
    pub seed: u64,
}

impl Default for AmphiphileOptions {
    /// A lipid-like table: tails strongly attract tails, heads mildly attract
    /// heads and solvent, mixed head–tail and tail–solvent pairs only repel.
    fn default() -> Self {
        Self {
            dt: 0.05,
            damping: 0.9,
            temperature: 6.0,
            cutoff: 100.0,
            sigma: 40.0,
            epsilon: [
                [40.0, -1.0, 20.0],
                [-1.0, 160.0, -1.0],
                [20.0, -1.0, 8.0],
            ],
            bond_k: 0.0,
            bond_len: 40.0,
            gravity: 0.02,
            steps_per_call: 8,
            seed: 0x5EED_A11F_1DE5,
        }
    }
}

/// Linear soft-core force at zero separation.
const CORE_K: f32 = 400.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    epsilon0: [f32; 4],
    epsilon1: [f32; 4],
    epsilon2: [f32; 4],
    dt: f32,
    damping: f32,
    temperature: f32,
    cutoff: f32,
    sigma: f32,
    core_k: f32,
    bond_k: f32,
    bond_len: f32,
    gravity: f32,
    n_nodes: u32,
    step_index: u32,
    v_max: f32,
}

struct State {
    n_nodes: u32,
    pos_buf_size: u64,
    velocities: wgpu::Buffer,
    forces: wgpu::Buffer,
    species: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    edge_neighbors: wgpu::Buffer,
    rng: wgpu::Buffer,
    energy: wgpu::Buffer,
    params: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    force_pipeline: wgpu::ComputePipeline,
    integrate_pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    step_index: u32,
}

/// The amphiphile simulation over a caller-owned positions buffer.
pub struct AmphiphileLayout {
    options: AmphiphileOptions,
    state: Option<State>,
    halted: bool,
}

impl AmphiphileLayout {
    pub fn new(options: AmphiphileOptions) -> Self {
        Self { options, state: None, halted: false }
    }

    pub fn options(&self) -> &AmphiphileOptions {
        &self.options
    }

    pub fn set_options(&mut self, options: AmphiphileOptions) {
        self.options = options;
        self.halted = false;
    }

    pub fn node_count(&self) -> Option<usize> {
        self.state.as_ref().map(|s| s.n_nodes as usize)
    }

    /// Replace every particle's species in place; the running simulation
    /// picks the new table up on the next dispatch.
    pub fn set_species(&mut self, queue: &wgpu::Queue, species: &[Species]) -> Result<(), String> {
        let Some(state) = self.state.as_ref() else {
            return Err("layout not initialised".to_string());
        };
        if species.len() != state.n_nodes as usize {
            return Err(format!("set_species: got {} entries, expected {}", species.len(), state.n_nodes));
        }
        queue.write_buffer(&state.species, 0, bytemuck::cast_slice(&species_words(species)));
        self.halted = false;
        Ok(())
    }

    /// Build the compute resources over `positions_buf` (vec4 per node,
    /// caller-owned, `.w` left untouched) from an index-based ingest.
    pub fn init_with_device_csr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &CsrInput<'_>,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        let n = input.n_nodes as usize;
        if n == 0 {
            return Err("amphiphile: no nodes".to_string());
        }
        if let Some(p) = input.positions {
            if p.len() != n * 3 {
                return Err(format!("amphiphile: {} positions for {n} nodes", p.len() / 3));
            }
            let mut padded = vec![0.0_f32; n * 4];
            for i in 0..n {
                padded[4 * i..4 * i + 3].copy_from_slice(&p[3 * i..3 * i + 3]);
            }
            queue.write_buffer(positions_buf, 0, bytemuck::cast_slice(&padded));
        }

        let species: Vec<u32> = match input.species {
            Some(s) if s.len() == n => species_words(s),
            Some(s) => return Err(format!("amphiphile: {} species for {n} nodes", s.len())),
            None => vec![Species::Solvent as u32; n],
        };
        let (offsets, neighbors) = csr(n, input.edges);
        let rng: Vec<u32> = (0..n as u64)
            .map(|i| (self.options.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(i.wrapping_mul(0x632B_E59B_D9B4_E019)) >> 11) as u32 | 1)
            .collect();

        let storage = |label: &str, bytes: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            })
        };
        let zero4 = vec![0.0_f32; n * 4];
        let velocities = storage("amphiphile/velocities", bytemuck::cast_slice(&zero4));
        let forces = storage("amphiphile/forces", bytemuck::cast_slice(&zero4));
        let species_buf = storage("amphiphile/species", bytemuck::cast_slice(&species));
        let edge_offsets = storage("amphiphile/edge_offsets", bytemuck::cast_slice(&offsets));
        let edge_neighbors = storage("amphiphile/edge_neighbors", bytemuck::cast_slice(&nonempty(&neighbors)));
        let rng_buf = storage("amphiphile/rng", bytemuck::cast_slice(&rng));
        let energy = storage("amphiphile/energy", bytemuck::cast_slice(&vec![0.0_f32; n]));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("amphiphile/params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let entries: Vec<wgpu::BindGroupLayoutEntry> = (0..8u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: matches!(binding, 3 | 4 | 5) },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .chain(std::iter::once(wgpu::BindGroupLayoutEntry {
                binding: 8,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }))
            .collect();
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("amphiphile_bgl"),
            entries: &entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("amphiphile.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/amphiphile.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("amphiphile_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let force_pipeline = pipeline("force_step");
        let integrate_pipeline = pipeline("integrate");

        let mut state = State {
            n_nodes: input.n_nodes,
            pos_buf_size: (n * 16) as u64,
            velocities,
            forces,
            species: species_buf,
            edge_offsets,
            edge_neighbors,
            rng: rng_buf,
            energy,
            params,
            bind_group_layout,
            force_pipeline,
            integrate_pipeline,
            bind_group: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[] }),
                entries: &[],
            }),
            step_index: 0,
        };
        state.bind_group = bind_group(device, &state, positions_buf);
        self.state = Some(state);
        self.halted = false;
        Ok(())
    }

    /// Record `steps_per_call` simulation steps into `encoder`.
    pub fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        _positions_buf: &wgpu::Buffer,
    ) {
        let Some(state) = self.state.as_mut() else { return };
        if self.halted {
            return;
        }
        let o = &self.options;
        let sigma = o.sigma.max(1e-3);
        let groups = state.n_nodes.div_ceil(64).max(1);
        for _ in 0..o.steps_per_call.max(1) {
            let params = Params {
                epsilon0: [o.epsilon[0][0], o.epsilon[0][1], o.epsilon[0][2], 0.0],
                epsilon1: [o.epsilon[1][0], o.epsilon[1][1], o.epsilon[1][2], 0.0],
                epsilon2: [o.epsilon[2][0], o.epsilon[2][1], o.epsilon[2][2], 0.0],
                dt: o.dt,
                damping: o.damping.clamp(0.0, 1.0),
                temperature: o.temperature.max(0.0),
                cutoff: o.cutoff.max(sigma + 1e-3),
                sigma,
                core_k: CORE_K,
                bond_k: o.bond_k.max(0.0),
                bond_len: o.bond_len.max(0.0),
                gravity: o.gravity.max(0.0),
                n_nodes: state.n_nodes,
                step_index: state.step_index,
                v_max: sigma / o.dt.max(1e-4),
            };
            queue.write_buffer(&state.params, 0, bytemuck::bytes_of(&params));
            for pipeline in [&state.force_pipeline, &state.integrate_pipeline] {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("amphiphile"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &state.bind_group, &[]);
                pass.dispatch_workgroups(groups, 1, 1);
            }
            state.step_index = state.step_index.wrapping_add(1);
        }
    }

    /// Copy the shared positions out, `[x, y, z, w]` per node.
    pub async fn read_back_positions(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        positions_buf: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let Some(state) = self.state.as_ref() else {
            return Err("layout not initialised".to_string());
        };
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("amphiphile/readback"),
            size: state.pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("amphiphile/readback") });
        encoder.copy_buffer_to_buffer(positions_buf, 0, &staging, 0, state.pos_buf_size);
        queue.submit(Some(encoder.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = futures_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| tx.send(res));
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);
        rx.recv().await.map_err(|_| "map channel dropped".to_string())?.map_err(|e| format!("buffer map failed: {e:?}"))?;
        let data = slice.get_mapped_range();
        let floats = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(floats)
    }
}

fn species_words(species: &[Species]) -> Vec<u32> {
    species.iter().map(|s| *s as u32).collect()
}

fn nonempty(words: &[u32]) -> Vec<u32> {
    if words.is_empty() { vec![0] } else { words.to_vec() }
}

/// Undirected CSR from `[s0, t0, s1, t1, ...]`; out-of-range endpoints are dropped.
fn csr(n: usize, edges: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut degree = vec![0u32; n];
    for pair in edges.chunks_exact(2) {
        let (s, t) = (pair[0] as usize, pair[1] as usize);
        if s < n && t < n && s != t {
            degree[s] += 1;
            degree[t] += 1;
        }
    }
    let mut offsets = vec![0u32; n + 1];
    for i in 0..n {
        offsets[i + 1] = offsets[i] + degree[i];
    }
    let mut fill = offsets.clone();
    let mut neighbors = vec![0u32; offsets[n] as usize];
    for pair in edges.chunks_exact(2) {
        let (s, t) = (pair[0] as usize, pair[1] as usize);
        if s < n && t < n && s != t {
            neighbors[fill[s] as usize] = t as u32;
            fill[s] += 1;
            neighbors[fill[t] as usize] = s as u32;
            fill[t] += 1;
        }
    }
    (offsets, neighbors)
}

fn bind_group(device: &wgpu::Device, state: &State, positions: &wgpu::Buffer) -> wgpu::BindGroup {
    fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry { binding, resource: buffer.as_entire_binding() }
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("amphiphile_bg"),
        layout: &state.bind_group_layout,
        entries: &[
            entry(0, positions),
            entry(1, &state.velocities),
            entry(2, &state.forces),
            entry(3, &state.species),
            entry(4, &state.edge_offsets),
            entry(5, &state.edge_neighbors),
            entry(6, &state.rng),
            entry(7, &state.energy),
            entry(8, &state.params),
        ],
    })
}

/// Storage buffers the compute stage binds, within WebGPU's per-stage limit.
#[cfg(test)]
const STORAGE_BUFFERS: u32 = 8;

impl PhysicsLayout for AmphiphileLayout {
    type Settings = AmphiphileOptions;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "amphiphile",
            kind: LayoutKind::Physics,
            display_name: "Amphiphile self-assembly",
            description: "wgpu compute coarse-grained lipid model: species-pair wells, emergent micelles, sheets and vesicles",
            requirements: LayoutRequirements { needs_edges: false, needs_cpu_positions: false, needs_gpu_positions_buffer: true },
        }
    }

    fn new(settings: Self::Settings) -> Self {
        AmphiphileLayout::new(settings)
    }

    fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        let mut ids: Vec<&str> = graph.nodes.keys().map(String::as_str).collect();
        ids.sort_unstable();
        let index = |id: &str| ids.binary_search(&id).ok().map(|i| i as u32);
        let edges: Vec<u32> = graph
            .edges
            .values()
            .filter_map(|e| Some([index(&e.source)?, index(&e.target)?]))
            .flatten()
            .collect();
        let positions: Vec<f32> = ids
            .iter()
            .flat_map(|id| graph.nodes[*id].position3.unwrap_or([0.0, 0.0, 0.0]))
            .collect();
        let input = CsrInput {
            n_nodes: ids.len() as u32,
            edges: &edges,
            positions: Some(&positions),
            node_physics: None,
            edge_physics: None,
            species: None,
        };
        self.init_with_device_csr(device, queue, &input, positions_buf)
    }

    fn init_with_device_csr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &CsrInput<'_>,
        positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        AmphiphileLayout::init_with_device_csr(self, device, queue, input, positions_buf)
    }

    fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        positions_buf: &wgpu::Buffer,
    ) {
        AmphiphileLayout::step_with_encoder(self, device, queue, encoder, positions_buf)
    }

    fn set_settings(&mut self, settings: Self::Settings) {
        self.set_options(settings)
    }

    fn settings(&self) -> &Self::Settings {
        &self.options
    }

    fn set_species(&mut self, queue: &wgpu::Queue, species: &[Species]) -> Result<(), String> {
        AmphiphileLayout::set_species(self, queue, species)
    }

    fn is_halted(&self) -> bool {
        self.halted
    }

    fn wake(&mut self) {
        self.halted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await?;
        adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("test/amphiphile"),
                    required_features: wgpu::Features::empty(),
                    required_limits: crate::layout::algorithms::gpu_force::gpu_force_device_limits(&adapter.limits()),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .ok()
    }

    /// Deterministic pseudo-random box of side `side`.
    fn random_box(n: usize, side: f32, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f32 / (1u64 << 53) as f32
        };
        (0..n * 3).map(|_| (next() - 0.5) * side).collect()
    }

    fn positions_buffer(device: &wgpu::Device, n: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test/positions"),
            size: (n * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    async fn run(
        species: &[Species],
        edges: &[u32],
        options: AmphiphileOptions,
        calls: usize,
        retune: Option<(usize, Vec<Species>)>,
    ) -> Option<Vec<[f32; 3]>> {
        let (device, queue) = device().await?;
        let n = species.len();
        let positions = positions_buffer(&device, n);
        let seed = random_box(n, (n as f32).cbrt() * options.sigma * 1.6, 7);
        let input = CsrInput { n_nodes: n as u32, edges, positions: Some(&seed), node_physics: None, edge_physics: None, species: Some(species) };
        let mut layout = AmphiphileLayout::new(options);
        layout.init_with_device_csr(&device, &queue, &input, &positions).expect("init");
        for call in 0..calls {
            if let Some((at, ref table)) = retune {
                if call == at {
                    layout.set_species(&queue, table).expect("retune");
                }
            }
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("test/step") });
            layout.step_with_encoder(&device, &queue, &mut encoder, &positions);
            queue.submit(Some(encoder.finish()));
        }
        let flat = layout.read_back_positions(&device, &queue, &positions).await.ok()?;
        Some(flat.chunks_exact(4).map(|c| [c[0], c[1], c[2]]).collect())
    }

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn centroid(points: &[[f32; 3]]) -> [f32; 3] {
        let n = points.len() as f32;
        let mut c = [0.0; 3];
        for p in points {
            for k in 0..3 {
                c[k] += p[k] / n;
            }
        }
        c
    }

    fn mean_nn(points: &[[f32; 3]]) -> f32 {
        let mut total = 0.0;
        for (i, a) in points.iter().enumerate() {
            let nn = points.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, b)| dist(*a, *b)).fold(f32::INFINITY, f32::min);
            total += nn;
        }
        total / points.len() as f32
    }

    /// Single-linkage clusters under `link`.
    fn clusters(points: &[[f32; 3]], link: f32) -> Vec<Vec<usize>> {
        let mut parent: Vec<usize> = (0..points.len()).collect();
        fn find(p: &mut Vec<usize>, i: usize) -> usize {
            if p[i] != i {
                let r = find(p, p[i]);
                p[i] = r;
            }
            p[i]
        }
        for i in 0..points.len() {
            for j in i + 1..points.len() {
                if dist(points[i], points[j]) < link {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
            }
        }
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for i in 0..points.len() {
            let r = find(&mut parent, i);
            groups.entry(r).or_default().push(i);
        }
        groups.into_values().collect()
    }

    fn mixture(tails: usize, heads: usize, solvent: usize) -> Vec<Species> {
        let mut v = vec![Species::Tail; tails];
        v.extend(std::iter::repeat(Species::Head).take(heads));
        v.extend(std::iter::repeat(Species::Solvent).take(solvent));
        v
    }

    /// Free particles: tails gather with tails and heads stay out of the tail
    /// aggregates (no head sits within the tail–tail contact distance).
    #[test]
    fn tails_cluster_and_heads_stay_out() {
        let species = mixture(60, 60, 200);
        let Some(points) = pollster::block_on(run(&species, &[], AmphiphileOptions::default(), 400, None)) else { return };
        let sigma = AmphiphileOptions::default().sigma;
        let tails: Vec<_> = points[..60].to_vec();
        let heads: Vec<_> = points[60..120].to_vec();
        let nn = mean_nn(&tails);
        let head_to_tail = heads
            .iter()
            .map(|h| tails.iter().map(|t| dist(*h, *t)).fold(f32::INFINITY, f32::min))
            .sum::<f32>()
            / heads.len() as f32;
        println!("tail nn {nn:.1}, head->nearest tail {head_to_tail:.1} (sigma {sigma})");
        assert!(nn < 1.5 * sigma, "tails gather: nearest-neighbour {nn} vs sigma {sigma}");
        assert!(head_to_tail > nn, "heads keep out of the tail aggregates: {head_to_tail} vs tail spacing {nn}");
    }

    #[test]
    fn a_bilayer_sheet_or_vesicle_emerges() {
        let n = 200;
        let mut species = Vec::with_capacity(2 * n + 150);
        let mut edges = Vec::new();
        for i in 0..n {
            species.push(Species::Head);
            species.push(Species::Tail);
            edges.extend([2 * i as u32, 2 * i as u32 + 1]);
        }
        species.extend(std::iter::repeat(Species::Solvent).take(150));
        let options = AmphiphileOptions { bond_k: 30.0, ..AmphiphileOptions::default() };
        let sigma = options.sigma;
        let Some(points) = pollster::block_on(run(&species, &edges, options, 600, None)) else { return };
        let tails: Vec<[f32; 3]> = (0..n).map(|i| points[2 * i + 1]).collect();
        let heads: Vec<[f32; 3]> = (0..n).map(|i| points[2 * i]).collect();
        let groups = clusters(&tails, 1.3 * sigma);
        let big: Vec<_> = groups.iter().filter(|g| g.len() >= 10).collect();
        for g in &big {
            let members: Vec<_> = g.iter().map(|i| tails[*i]).collect();
            let c = centroid(&members);
            let tail_r = members.iter().map(|p| dist(*p, c)).sum::<f32>() / members.len() as f32;
            let head_r = g.iter().map(|i| dist(heads[*i], c)).sum::<f32>() / g.len() as f32;
            println!("cluster of {} tails: tail radius {tail_r:.1}, head radius {head_r:.1}", g.len());
            assert!(head_r > tail_r, "heads outside their cluster");
        }
        println!("{} clusters, {} with 10+ tails", groups.len(), big.len());
        assert!(!big.is_empty() && big.len() <= 3, "tails coalesce into at most 3 aggregates: {}", big.len());
    }

    #[test]
    fn set_species_retunes_without_reinit() {
        let species = mixture(40, 40, 240);
        let mut flipped = species.clone();
        for s in flipped.iter_mut().skip(80).take(120) {
            *s = Species::Tail;
        }
        let opts = AmphiphileOptions { temperature: 0.0, ..AmphiphileOptions::default() };
        let Some(control) = pollster::block_on(run(&species, &[], opts.clone(), 300, None)) else { return };
        let Some(retuned) = pollster::block_on(run(&species, &[], opts, 300, Some((100, flipped)))) else { return };
        let flipped_control = mean_nn(&control[80..200]);
        let flipped_retuned = mean_nn(&retuned[80..200]);
        println!("former solvent nn: control {flipped_control:.1}, retuned {flipped_retuned:.1}");
        assert!(flipped_retuned < 0.8 * flipped_control, "retuned particles gather like tails");
    }

    #[test]
    fn amphiphile_pipeline_layout_fits_ten_storage_buffers() {
        assert!(STORAGE_BUFFERS <= 10, "{STORAGE_BUFFERS} storage buffers exceed Chrome's per-stage cap");
    }
}
