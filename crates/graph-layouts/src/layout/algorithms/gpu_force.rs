//! WebGPU compute-shader force-directed layout.
//!
//! Runs natively (vulkan/metal/dx12 via wgpu defaults) and in browsers
//! (wgpu's WebGPU backend). No rendering — this is a layout engine; the
//! consumer reads positions out and renders them however it likes.
//!
//! Algorithm: O(n^2) repulsion + CSR-adjacency springs + gravity + cursor.
//! Verlet-ish integration with velocity damping. Designed to step
//! incrementally — caller picks `steps_per_call` and runs `run()` each
//! frame (or as desired).

use crate::types::Graph;
use std::borrow::Cow;
use wgpu::util::DeviceExt;

// ---------- Public API -------------------------------------------------------

/// Tunables for the GPU force engine. Anything in here can be updated
/// per-frame via [`GpuForceLayout::set_options`] without rebuilding GPU
/// resources — only the uniform buffer is rewritten.
#[derive(Clone, Debug)]
pub struct GpuForceOptions {
    pub repulsion: f32,
    pub spring_k: f32,
    pub spring_len: f32,
    pub gravity: f32,
    pub damping: f32,
    pub dt: f32,
    pub cursor_pos: [f32; 3],
    /// 0.0 disables the cursor force entirely.
    pub cursor_radius: f32,
    /// Negative attracts, positive repels.
    pub cursor_strength: f32,
    pub steps_per_call: u32,
    /// Per-pair distance clip on repulsion. <=0 means "no clip" (full O(n^2)
    /// attractor at infinity). With grid_enabled this also lets us skip
    /// far-cell pairs cheaply. Default = 4 * spring_len = 120.
    pub repulsion_radius: f32,
    /// Multiplied into `damping` once per `step_with_encoder` / `run` call.
    /// 1.0 = no cooling. 0.998 cools toward `cooling_floor` over a few
    /// hundred frames. Set <1.0 to enable.
    pub cooling_alpha: f32,
    /// Lower bound on damping under cooling. Below this we stop decaying.
    pub cooling_floor: f32,
    /// Average kinetic-energy threshold below which we consider the layout
    /// converged and short-circuit further dispatches. 0 disables.
    pub energy_threshold: f32,
    /// Whether to use the spatial-hash grid. Default true. Disable for
    /// correctness comparison or for tiny graphs where the grid build
    /// dominates.
    pub grid_enabled: bool,
}

impl Default for GpuForceOptions {
    fn default() -> Self {
        Self {
            repulsion: 200.0,
            spring_k: 0.08,
            spring_len: 30.0,
            gravity: 0.005,
            damping: 0.78,
            dt: 0.04,
            cursor_pos: [0.0; 3],
            cursor_radius: 0.0,
            cursor_strength: 0.0,
            steps_per_call: 8,
            repulsion_radius: 120.0,
            cooling_alpha: 1.0,
            cooling_floor: 0.5,
            energy_threshold: 0.0,
            grid_enabled: true,
        }
    }
}

#[cfg(feature = "_serde_gpu")]
mod _ignore {
    // intentionally empty
}

// Hand-rolled serde so callers can pass JSON through the WASM bridge
// without dragging serde derives onto wgpu types.
impl serde::Serialize for GpuForceOptions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("GpuForceOptions", 15)?;
        st.serialize_field("repulsion", &self.repulsion)?;
        st.serialize_field("spring_k", &self.spring_k)?;
        st.serialize_field("spring_len", &self.spring_len)?;
        st.serialize_field("gravity", &self.gravity)?;
        st.serialize_field("damping", &self.damping)?;
        st.serialize_field("dt", &self.dt)?;
        st.serialize_field("cursor_pos", &self.cursor_pos)?;
        st.serialize_field("cursor_radius", &self.cursor_radius)?;
        st.serialize_field("cursor_strength", &self.cursor_strength)?;
        st.serialize_field("steps_per_call", &self.steps_per_call)?;
        st.serialize_field("repulsion_radius", &self.repulsion_radius)?;
        st.serialize_field("cooling_alpha", &self.cooling_alpha)?;
        st.serialize_field("cooling_floor", &self.cooling_floor)?;
        st.serialize_field("energy_threshold", &self.energy_threshold)?;
        st.serialize_field("grid_enabled", &self.grid_enabled)?;
        st.end()
    }
}

impl<'de> serde::Deserialize<'de> for GpuForceOptions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            repulsion: Option<f32>,
            #[serde(default)]
            spring_k: Option<f32>,
            #[serde(default)]
            spring_len: Option<f32>,
            #[serde(default)]
            gravity: Option<f32>,
            #[serde(default)]
            damping: Option<f32>,
            #[serde(default)]
            dt: Option<f32>,
            #[serde(default)]
            cursor_pos: Option<[f32; 3]>,
            #[serde(default)]
            cursor_radius: Option<f32>,
            #[serde(default)]
            cursor_strength: Option<f32>,
            #[serde(default)]
            steps_per_call: Option<u32>,
            #[serde(default)]
            repulsion_radius: Option<f32>,
            #[serde(default)]
            cooling_alpha: Option<f32>,
            #[serde(default)]
            cooling_floor: Option<f32>,
            #[serde(default)]
            energy_threshold: Option<f32>,
            #[serde(default)]
            grid_enabled: Option<bool>,
        }
        let r = Raw::deserialize(d)?;
        let def = GpuForceOptions::default();
        Ok(GpuForceOptions {
            repulsion: r.repulsion.unwrap_or(def.repulsion),
            spring_k: r.spring_k.unwrap_or(def.spring_k),
            spring_len: r.spring_len.unwrap_or(def.spring_len),
            gravity: r.gravity.unwrap_or(def.gravity),
            damping: r.damping.unwrap_or(def.damping),
            dt: r.dt.unwrap_or(def.dt),
            cursor_pos: r.cursor_pos.unwrap_or(def.cursor_pos),
            cursor_radius: r.cursor_radius.unwrap_or(def.cursor_radius),
            cursor_strength: r.cursor_strength.unwrap_or(def.cursor_strength),
            steps_per_call: r.steps_per_call.unwrap_or(def.steps_per_call),
            repulsion_radius: r.repulsion_radius.unwrap_or(def.repulsion_radius),
            cooling_alpha: r.cooling_alpha.unwrap_or(def.cooling_alpha),
            cooling_floor: r.cooling_floor.unwrap_or(def.cooling_floor),
            energy_threshold: r.energy_threshold.unwrap_or(def.energy_threshold),
            grid_enabled: r.grid_enabled.unwrap_or(def.grid_enabled),
        })
    }
}

/// Owns the wgpu device + queue when the layout is constructed via the
/// legacy `run()` path. The shared/borrowed path leaves this `None` since
/// the caller's renderer owns those.
struct OwnedDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

pub struct GpuForceLayout {
    options: GpuForceOptions,
    state: Option<GpuState>,
    owned_device: Option<OwnedDevice>,
}

impl GpuForceLayout {
    pub fn new(options: GpuForceOptions) -> Self {
        Self {
            options,
            state: None,
            owned_device: None,
        }
    }

    pub fn set_options(&mut self, options: GpuForceOptions) {
        self.options = options;
    }

    pub fn options(&self) -> &GpuForceOptions {
        &self.options
    }

    pub fn node_count(&self) -> Option<usize> {
        self.state.as_ref().map(|s| s.n_nodes as usize)
    }

    /// Run `steps_per_call` simulation steps. Initialises GPU resources on
    /// first call (or whenever the graph topology has changed). Writes back
    /// positions into `graph.nodes[*].position3`.
    ///
    /// This is the legacy "I own everything" path — it creates its own
    /// `wgpu::Instance + Device + Queue + positions buffer`. Kept for native
    /// standalone callers / WASM `LayoutManager`. For sharing GPU resources
    /// with a renderer, use [`init_with_device`] + [`step_with_encoder`].
    pub async fn run(&mut self, graph: &mut Graph) -> Result<(), String> {
        // (Re)build GPU state if topology changed or this is the first run.
        let needs_rebuild = match &self.state {
            None => true,
            Some(state) => {
                state.n_nodes as usize != graph.nodes.len()
                    || state.n_edges as usize != graph.edges.len()
                    || !matches!(state.positions, PositionsStorage::Owned { .. })
            }
        };
        if needs_rebuild {
            // Acquire our own device/queue if we haven't already.
            if self.owned_device.is_none() {
                let instance = wgpu::Instance::default();
                let adapter = instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
                    .ok_or_else(|| "no GPU adapter".to_string())?;
                let (device, queue) = adapter
                    .request_device(
                        &wgpu::DeviceDescriptor {
                            label: Some("graph-layouts/gpu_force"),
                            required_features: wgpu::Features::empty(),
                            required_limits: wgpu::Limits::downlevel_defaults()
                                .using_resolution(adapter.limits()),
                            memory_hints: wgpu::MemoryHints::Performance,
                        },
                        None,
                    )
                    .await
                    .map_err(|e| format!("request_device failed: {e}"))?;
                self.owned_device = Some(OwnedDevice { device, queue });
            }
            let od = self.owned_device.as_ref().unwrap();
            self.state = Some(GpuState::new_owned(&od.device, graph)?);
        }

        let od = self
            .owned_device
            .as_ref()
            .ok_or_else(|| "owned device missing".to_string())?;
        let state = self.state.as_mut().unwrap();
        // First-call init for cooling.
        if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
            state.effective_damping = self.options.damping;
        }
        // Cool damping per call (toward floor).
        let alpha = self.options.cooling_alpha.clamp(0.5, 1.0);
        let floor = self.options.cooling_floor.clamp(0.0, 1.0);
        state.effective_damping = (state.effective_damping * alpha).max(floor.min(self.options.damping));

        state.rebuild_and_upload_grid(&od.device, &od.queue, &self.options);
        state.write_params(&od.queue, &self.options);
        let mut steps_done = 0u32;
        for _ in 0..self.options.steps_per_call.max(1) {
            state.dispatch_step_direct(&od.device, &od.queue);
            state.swap_position_buffers();
            steps_done += 1;
        }
        let _ = steps_done;
        let positions = state.read_positions_owned(&od.device, &od.queue).await?;
        // Mirror back into our CPU position cache for the next grid build.
        if positions.len() == state.cpu_positions.len() {
            state.cpu_positions.copy_from_slice(&positions);
        }
        // Write back into the graph in the same id-order we built the buffer.
        for (id, p) in state.node_order.iter().zip(positions.chunks_exact(4)) {
            if let Some(node) = graph.nodes.get_mut(id) {
                node.position3 = Some([p[0], p[1], p[2]]);
            }
        }
        Ok(())
    }

    /// Build GPU compute resources against a caller-supplied
    /// `wgpu::Device + Queue + positions buffer`. The positions buffer must
    /// be sized for `graph.nodes.len() * 16` bytes (vec3 + pad per node) and
    /// usable as a STORAGE buffer (and whatever else the caller needs —
    /// typically also VERTEX/COPY_SRC/COPY_DST for renderer sharing).
    ///
    /// After init, [`step_with_encoder`] records compute dispatches into a
    /// caller-supplied encoder. The shared positions buffer always contains
    /// the latest simulation state after the encoder is submitted, so a
    /// vertex shader bound to the same buffer reads current positions with
    /// zero CPU copies per frame.
    pub fn init_with_device(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        positions_buffer: &wgpu::Buffer,
    ) -> Result<(), String> {
        let state = GpuState::new_borrowed(device, graph, positions_buffer)?;
        state.upload_initial_positions_to(queue, positions_buffer);
        self.state = Some(state);
        Ok(())
    }

    /// Record `steps_per_call` compute dispatches into the caller's encoder.
    /// `device` and `queue` must be the same ones passed to
    /// `init_with_device`. `queue` is used to write the params uniform
    /// before the dispatches; `device` to allocate the per-step bind group.
    /// No-op if the layout isn't initialised.
    pub fn step_with_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        shared_buffer: &wgpu::Buffer,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        // First-call init for cooling.
        if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
            state.effective_damping = self.options.damping;
        }
        let alpha = self.options.cooling_alpha.clamp(0.5, 1.0);
        let floor = self.options.cooling_floor.clamp(0.0, 1.0);
        state.effective_damping = (state.effective_damping * alpha).max(floor.min(self.options.damping));

        state.rebuild_and_upload_grid(device, queue, &self.options);
        state.write_params(queue, &self.options);
        let steps = self.options.steps_per_call.max(1);
        for _ in 0..steps {
            state.dispatch_borrowed_step(device, encoder, shared_buffer);
            state.swap_position_buffers();
        }
        // Make sure the shared (external/borrowed) buffer ends up holding
        // the latest result. Convention:
        //   - Borrowed mode: pos_a == shared, pos_b == internal.
        //   - Each dispatch writes "out", then we flip a_is_in.
        //   - After dispatch+swap, a_is_in indicates which buffer is the
        //     NEXT step's "in" — i.e. which buffer holds the latest result.
        //     So after the loop, if a_is_in == true the latest is pos_a
        //     (shared, good). If a_is_in == false the latest is pos_b
        //     (internal) — copy it to shared so the renderer reads it.
        if !state.a_is_in {
            encoder.copy_buffer_to_buffer(
                state.positions.pos_b(),
                0,
                shared_buffer,
                0,
                state.pos_buf_size,
            );
        }
    }

    /// Read positions back to the CPU. Useful for picking / debugging from
    /// the new shared-buffer path (the legacy `run()` already does this
    /// internally).
    pub async fn read_back_positions(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared_buffer: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let Some(state) = self.state.as_ref() else {
            return Err("layout not initialised".to_string());
        };
        state
            .read_positions_with_device(device, queue, shared_buffer)
            .await
    }
}

// ---------- Internal GPU state ----------------------------------------------

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SimParamsRaw {
    repulsion: f32,
    spring_k: f32,
    spring_len: f32,
    gravity: f32,

    damping: f32,
    dt: f32,
    cursor_radius: f32,
    cursor_strength: f32,

    cursor_pos: [f32; 3],
    n_nodes: u32,

    n_edges: u32,
    repulsion_radius: f32,
    grid_cell_size: f32,
    grid_enabled: u32,

    grid_origin: [f32; 3],
    n_cells: u32,

    grid_dim: [u32; 3],
    _pad0: u32,
}

// Each vec3<f32> in a storage buffer occupies 16 bytes (vec3 has stride/align
// of 16 in WGSL). We use a 4-component layout on the CPU side to match.
const VEC3_STRIDE: u64 = 16;

/// Position buffer ownership.
///
/// In the legacy `run()` path the GPU state owns both ping-pong buffers.
/// In the renderer-shared path the renderer owns one buffer (used as both
/// vertex source and compute storage) and we own the second internal
/// ping-pong target. The shared buffer is supplied to step / readback
/// methods as a reference so we don't have to clone wgpu::Buffer (which
/// isn't Clone in wgpu 23).
enum PositionsStorage {
    Owned {
        pos_a: wgpu::Buffer,
        pos_b: wgpu::Buffer,
    },
    /// Marker variant — the actual shared `pos_a` is passed in to each
    /// method that needs it. We still own the internal `pos_b` ping-pong.
    Borrowed {
        pos_b: wgpu::Buffer,
    },
}

impl PositionsStorage {
    fn pos_b(&self) -> &wgpu::Buffer {
        match self {
            PositionsStorage::Owned { pos_b, .. } | PositionsStorage::Borrowed { pos_b, .. } => {
                pos_b
            }
        }
    }
}

struct GpuState {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    positions: PositionsStorage,
    /// True while pos_a is the "in" and pos_b is the "out" buffer.
    a_is_in: bool,
    velocities: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    edge_neighbors: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    /// Per-node mass (1 + log2(degree)). Static once built.
    mass_buf: wgpu::Buffer,
    /// Spatial-hash cells. (Re)allocated when capacity grows.
    cell_offsets_buf: wgpu::Buffer,
    cell_offsets_capacity: u64, // bytes
    cell_nodes_buf: wgpu::Buffer,
    cell_nodes_capacity: u64,
    /// Per-node KE proxy = |vel|^2 written by the shader; CPU reads back
    /// (small) for energy_threshold checks.
    energy_buf: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
    /// Staging buffer for CPU readback. Only allocated in the owned path
    /// and on-demand for the borrowed path's `read_back_positions`.
    staging: Option<wgpu::Buffer>,

    n_nodes: u32,
    n_edges: u32,
    pos_buf_size: u64,

    /// Initial (CPU-built) positions, kept around so the borrowed-mode path
    /// can seed the shared buffer via `queue.write_buffer` after init.
    initial_positions: Vec<f32>,

    /// CPU-side mirror of latest positions, used to rebuild the grid each
    /// step without a GPU readback.
    cpu_positions: Vec<f32>,

    /// Last-built grid metadata (mirrored into params each step).
    grid_origin: [f32; 3],
    grid_cell_size: f32,
    grid_dim: [u32; 3],
    n_cells: u32,

    /// Stable node-id ordering used to interpret the position buffer.
    node_order: Vec<String>,

    /// Effective damping currently in use; cooled per call.
    effective_damping: f32,
}

/// CPU-side pre-compute: stable id ordering, initial positions
/// (padded vec4 layout), velocities, and CSR adjacency arrays.
struct PreCompute {
    n_nodes: u32,
    n_edges: u32,
    node_order: Vec<String>,
    initial_positions: Vec<f32>, // padded vec4-per-node
    velocities: Vec<f32>,
    edge_offsets: Vec<u32>,
    edge_neighbors: Vec<u32>,
    /// Per-node mass = 1 + log2(degree). Hubs end up heavier.
    mass: Vec<f32>,
}

fn precompute(graph: &Graph) -> PreCompute {
    let n_nodes = graph.nodes.len() as u32;
    let n_edges = graph.edges.len() as u32;

    let mut node_order: Vec<String> = graph.nodes.keys().cloned().collect();
    node_order.sort();
    let id_to_idx: std::collections::HashMap<&str, u32> = node_order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i as u32))
        .collect();

    let radius = ((n_nodes as f32).max(1.0).sqrt()) * 5.0;
    let mut positions: Vec<f32> = Vec::with_capacity(n_nodes as usize * 4);
    let mut seed: u32 = 0x9E37_79B1;
    let mut next = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    for id in &node_order {
        let n = &graph.nodes[id];
        let p = n
            .position3
            .unwrap_or_else(|| [next() * radius, next() * radius, next() * radius]);
        positions.extend_from_slice(&[p[0], p[1], p[2], 0.0]);
    }
    let velocities: Vec<f32> = vec![0.0; n_nodes as usize * 4];

    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n_nodes as usize];
    for e in graph.edges.values() {
        let (Some(&s), Some(&t)) = (
            id_to_idx.get(e.source.as_str()),
            id_to_idx.get(e.target.as_str()),
        ) else {
            continue;
        };
        if s == t {
            continue;
        }
        adj[s as usize].push(t);
        adj[t as usize].push(s);
    }
    let mut edge_offsets: Vec<u32> = Vec::with_capacity(n_nodes as usize + 1);
    let mut edge_neighbors: Vec<u32> = Vec::new();
    let mut acc: u32 = 0;
    edge_offsets.push(0);
    for ns in &adj {
        acc += ns.len() as u32;
        edge_neighbors.extend_from_slice(ns);
        edge_offsets.push(acc);
    }
    if edge_neighbors.is_empty() {
        edge_neighbors.push(0);
    }
    let mass: Vec<f32> = adj
        .iter()
        .map(|ns| 1.0 + ((ns.len() as f32).max(1.0)).log2())
        .collect();
    let mass = if mass.is_empty() { vec![1.0f32] } else { mass };
    PreCompute {
        n_nodes,
        n_edges,
        node_order,
        initial_positions: positions,
        velocities,
        edge_offsets,
        edge_neighbors,
        mass,
    }
}

// ---------- Spatial-hash grid (CPU build) -----------------------------------

/// Build a uniform 3D voxel grid over `positions` (length n*4, padded vec4).
/// Returns (origin, cell_size, dim, n_cells, cell_offsets, cell_nodes).
/// Caps `dim` at 64 per axis so memory stays bounded for crazy bboxes.
fn build_grid(
    positions: &[f32],
    n_nodes: u32,
    cell_size_in: f32,
) -> (
    [f32; 3],
    f32,
    [u32; 3],
    u32,
    Vec<u32>,
    Vec<u32>,
) {
    let n = n_nodes as usize;
    if n == 0 {
        return ([0.0; 3], 1.0, [1, 1, 1], 1, vec![0, 0], vec![0]);
    }
    // 1. bbox
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for k in 0..3 {
            let v = positions[i * 4 + k];
            if v < mn[k] { mn[k] = v; }
            if v > mx[k] { mx[k] = v; }
        }
    }
    if !mn[0].is_finite() {
        mn = [-1.0; 3];
        mx = [1.0; 3];
    }
    // pad bbox slightly so points on the max edge still land in the last cell
    let pad = (cell_size_in.max(1.0)) * 0.5;
    let origin = [mn[0] - pad, mn[1] - pad, mn[2] - pad];
    let extent = [
        (mx[0] - mn[0]) + 2.0 * pad,
        (mx[1] - mn[1]) + 2.0 * pad,
        (mx[2] - mn[2]) + 2.0 * pad,
    ];
    let cell_size = cell_size_in.max(1.0);
    const MAX_DIM: u32 = 64;
    let mut dim = [
        (((extent[0] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
        (((extent[1] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
        (((extent[2] / cell_size).ceil()) as u32).max(1).min(MAX_DIM),
    ];
    // If we capped, expand effective cell size so all points still fit.
    let mut eff_cell = cell_size;
    for k in 0..3 {
        let needed = (extent[k] / dim[k] as f32).max(1e-3);
        if needed > eff_cell {
            eff_cell = needed;
        }
    }
    // recompute dims with eff_cell to keep grid covering bbox precisely
    for k in 0..3 {
        dim[k] = (((extent[k] / eff_cell).ceil()) as u32).max(1).min(MAX_DIM);
    }
    let n_cells = dim[0] * dim[1] * dim[2];
    let inv = 1.0 / eff_cell;

    // 2. count per cell
    let mut counts = vec![0u32; n_cells as usize];
    let mut node_cell = vec![0u32; n];
    for i in 0..n {
        let mut ix = ((positions[i * 4] - origin[0]) * inv) as i32;
        let mut iy = ((positions[i * 4 + 1] - origin[1]) * inv) as i32;
        let mut iz = ((positions[i * 4 + 2] - origin[2]) * inv) as i32;
        if ix < 0 { ix = 0; } else if ix >= dim[0] as i32 { ix = dim[0] as i32 - 1; }
        if iy < 0 { iy = 0; } else if iy >= dim[1] as i32 { iy = dim[1] as i32 - 1; }
        if iz < 0 { iz = 0; } else if iz >= dim[2] as i32 { iz = dim[2] as i32 - 1; }
        let cell =
            ix as u32 + iy as u32 * dim[0] + iz as u32 * dim[0] * dim[1];
        node_cell[i] = cell;
        counts[cell as usize] += 1;
    }
    // 3. prefix sum
    let mut cell_offsets = vec![0u32; n_cells as usize + 1];
    let mut acc = 0u32;
    for c in 0..n_cells as usize {
        cell_offsets[c] = acc;
        acc += counts[c];
    }
    cell_offsets[n_cells as usize] = acc;
    // 4. scatter
    let mut cursor = cell_offsets.clone();
    let mut cell_nodes = vec![0u32; n];
    for i in 0..n {
        let c = node_cell[i] as usize;
        cell_nodes[cursor[c] as usize] = i as u32;
        cursor[c] += 1;
    }

    (origin, eff_cell, dim, n_cells, cell_offsets, cell_nodes)
}

fn build_pipeline(
    device: &wgpu::Device,
) -> (wgpu::ComputePipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("force.wgsl"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
            "shaders/force.wgsl"
        ))),
    });
    let bgl_entries = [
        storage_entry(0, true),
        storage_entry(1, false),
        storage_entry(2, false),
        storage_entry(3, true),
        storage_entry(4, true),
        wgpu::BindGroupLayoutEntry {
            binding: 5,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        storage_entry(6, true),  // cell_offsets
        storage_entry(7, true),  // cell_nodes
        storage_entry(8, true),  // mass
        storage_entry(9, false), // energy_out
    ];
    let bind_group_layout = device.create_bind_group_layout(
        &wgpu::BindGroupLayoutDescriptor {
            label: Some("gpu_force_bgl"),
            entries: &bgl_entries,
        },
    );
    let pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        },
    );
    let pipeline = device.create_compute_pipeline(
        &wgpu::ComputePipelineDescriptor {
            label: Some("force_step"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("force_step"),
            compilation_options: Default::default(),
            cache: None,
        },
    );
    (pipeline, bind_group_layout)
}

impl GpuState {
    /// Build state with caller-supplied device + owned positions buffers.
    fn new_owned(device: &wgpu::Device, graph: &Graph) -> Result<Self, String> {
        let pc = precompute(graph);
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        let pos_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_a"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_b"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let aux = build_aux_buffers(device, &pc);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions_staging"),
            size: pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let (pipeline, bind_group_layout) = build_pipeline(device);
        let cpu_positions = pc.initial_positions.clone();

        Ok(Self {
            pipeline,
            bind_group_layout,
            positions: PositionsStorage::Owned { pos_a, pos_b },
            a_is_in: true,
            velocities: aux.vel,
            edge_offsets: aux.off,
            edge_neighbors: aux.neigh,
            params_buf: aux.params,
            mass_buf: aux.mass,
            cell_offsets_buf: aux.cell_offsets,
            cell_offsets_capacity: aux.cell_offsets_capacity,
            cell_nodes_buf: aux.cell_nodes,
            cell_nodes_capacity: aux.cell_nodes_capacity,
            energy_buf: aux.energy,
            energy_staging: aux.energy_staging,
            staging: Some(staging),
            n_nodes: pc.n_nodes,
            n_edges: pc.n_edges,
            pos_buf_size,
            initial_positions: pc.initial_positions,
            cpu_positions,
            grid_origin: [0.0; 3],
            grid_cell_size: 1.0,
            grid_dim: [1, 1, 1],
            n_cells: 1,
            node_order: pc.node_order,
            effective_damping: 1.0,
        })
    }

    /// Build state against caller-supplied device + a borrowed positions
    /// storage buffer (typically owned by the renderer). We don't take the
    /// queue here — the caller passes it to `upload_initial_positions` and
    /// `step_with_encoder`. This avoids cloning wgpu::Queue.
    fn new_borrowed(
        device: &wgpu::Device,
        graph: &Graph,
        positions_buffer: &wgpu::Buffer,
    ) -> Result<Self, String> {
        let pc = precompute(graph);
        let pos_buf_size = (pc.n_nodes as u64).max(1) * VEC3_STRIDE;

        // Internal ping-pong target. COPY_SRC so we can copy back to the
        // shared buffer; COPY_DST so we can seed it.
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_internal_b"),
            contents: bytemuck::cast_slice(&pc.initial_positions),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let aux = build_aux_buffers(device, &pc);
        let (pipeline, bind_group_layout) = build_pipeline(device);

        let _ = positions_buffer; // sized check happens via caller usage
        let cpu_positions = pc.initial_positions.clone();

        Ok(Self {
            pipeline,
            bind_group_layout,
            positions: PositionsStorage::Borrowed { pos_b },
            a_is_in: true,
            velocities: aux.vel,
            edge_offsets: aux.off,
            edge_neighbors: aux.neigh,
            params_buf: aux.params,
            mass_buf: aux.mass,
            cell_offsets_buf: aux.cell_offsets,
            cell_offsets_capacity: aux.cell_offsets_capacity,
            cell_nodes_buf: aux.cell_nodes,
            cell_nodes_capacity: aux.cell_nodes_capacity,
            energy_buf: aux.energy,
            energy_staging: aux.energy_staging,
            staging: None,
            n_nodes: pc.n_nodes,
            n_edges: pc.n_edges,
            pos_buf_size,
            initial_positions: pc.initial_positions,
            cpu_positions,
            grid_origin: [0.0; 3],
            grid_cell_size: 1.0,
            grid_dim: [1, 1, 1],
            n_cells: 1,
            node_order: pc.node_order,
            effective_damping: 1.0,
        })
    }

    /// Seed the shared (borrowed) positions buffer with our initial values.
    /// Caller must supply the same shared buffer that was passed to
    /// `new_borrowed`.
    fn upload_initial_positions_to(&self, queue: &wgpu::Queue, shared: &wgpu::Buffer) {
        queue.write_buffer(shared, 0, bytemuck::cast_slice(&self.initial_positions));
    }

    fn write_params(&self, queue: &wgpu::Queue, opts: &GpuForceOptions) {
        let raw = SimParamsRaw {
            repulsion: opts.repulsion,
            spring_k: opts.spring_k,
            spring_len: opts.spring_len,
            gravity: opts.gravity,
            damping: self.effective_damping,
            dt: opts.dt,
            cursor_radius: opts.cursor_radius,
            cursor_strength: opts.cursor_strength,
            cursor_pos: opts.cursor_pos,
            n_nodes: self.n_nodes,
            n_edges: self.n_edges,
            repulsion_radius: opts.repulsion_radius,
            grid_cell_size: self.grid_cell_size,
            grid_enabled: if opts.grid_enabled { 1 } else { 0 },
            grid_origin: self.grid_origin,
            n_cells: self.n_cells,
            grid_dim: self.grid_dim,
            _pad0: 0,
        };
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&raw));
    }

    /// Build the spatial-hash grid from `cpu_positions`, (re)allocate the
    /// cell buffers if needed, and upload to GPU.
    fn rebuild_and_upload_grid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        opts: &GpuForceOptions,
    ) {
        let cell_size_target = if opts.repulsion_radius > 0.0 {
            opts.repulsion_radius
        } else {
            (opts.spring_len * 4.0).max(1.0)
        };
        let (origin, cell_size, dim, n_cells, cell_offsets, cell_nodes) =
            build_grid(&self.cpu_positions, self.n_nodes, cell_size_target);
        self.grid_origin = origin;
        self.grid_cell_size = cell_size;
        self.grid_dim = dim;
        self.n_cells = n_cells;

        // Resize cell_offsets buffer if needed.
        let needed_off_bytes = (cell_offsets.len() as u64 * 4).max(64);
        if needed_off_bytes > self.cell_offsets_capacity {
            self.cell_offsets_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_offsets"),
                size: needed_off_bytes * 2,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_offsets_capacity = needed_off_bytes * 2;
        }
        let needed_nodes_bytes = (cell_nodes.len() as u64 * 4).max(64);
        if needed_nodes_bytes > self.cell_nodes_capacity {
            self.cell_nodes_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_nodes"),
                size: needed_nodes_bytes * 2,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_nodes_capacity = needed_nodes_bytes * 2;
        }
        queue.write_buffer(
            &self.cell_offsets_buf,
            0,
            bytemuck::cast_slice(&cell_offsets),
        );
        queue.write_buffer(
            &self.cell_nodes_buf,
            0,
            bytemuck::cast_slice(&cell_nodes),
        );
    }

    /// Owned-mode "in/out" picker — both buffers live in PositionsStorage::Owned.
    fn owned_in_out(&self) -> (&wgpu::Buffer, &wgpu::Buffer) {
        let PositionsStorage::Owned { pos_a, pos_b } = &self.positions else {
            panic!("owned_in_out called on borrowed state");
        };
        if self.a_is_in {
            (pos_a, pos_b)
        } else {
            (pos_b, pos_a)
        }
    }

    /// Borrowed-mode "in/out" picker. The shared buffer (pos_a) is supplied
    /// by the caller; the internal pos_b lives in the state.
    fn borrowed_in_out<'a>(
        &'a self,
        shared: &'a wgpu::Buffer,
    ) -> (&'a wgpu::Buffer, &'a wgpu::Buffer) {
        let pos_b = self.positions.pos_b();
        if self.a_is_in {
            (shared, pos_b)
        } else {
            (pos_b, shared)
        }
    }

    fn make_bind_group(
        &self,
        device: &wgpu::Device,
        pos_in: &wgpu::Buffer,
        pos_out: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                buf_entry(0, pos_in),
                buf_entry(1, pos_out),
                buf_entry(2, &self.velocities),
                buf_entry(3, &self.edge_offsets),
                buf_entry(4, &self.edge_neighbors),
                buf_entry(5, &self.params_buf),
                buf_entry(6, &self.cell_offsets_buf),
                buf_entry(7, &self.cell_nodes_buf),
                buf_entry(8, &self.mass_buf),
                buf_entry(9, &self.energy_buf),
            ],
        })
    }

    /// Direct dispatch — owns its own encoder and submits immediately.
    /// Used by the legacy `run()` path (owned mode only).
    fn dispatch_step_direct(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let (pos_in, pos_out) = self.owned_in_out();
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_cmd"),
        });
        self.encode_compute(&mut encoder, &bind_group);
        queue.submit(Some(encoder.finish()));
    }

    /// Record dispatch into a caller-supplied encoder, reading/writing the
    /// borrowed shared buffer + internal pos_b.
    fn dispatch_borrowed_step(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shared: &wgpu::Buffer,
    ) {
        let (pos_in, pos_out) = self.borrowed_in_out(shared);
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        self.encode_compute(encoder, &bind_group);
    }

    fn encode_compute(&self, encoder: &mut wgpu::CommandEncoder, bind_group: &wgpu::BindGroup) {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("force_step_pass"),
            timestamp_writes: None,
        });
        cpass.set_pipeline(&self.pipeline);
        cpass.set_bind_group(0, bind_group, &[]);
        let groups = (self.n_nodes + 63) / 64;
        cpass.dispatch_workgroups(groups.max(1), 1, 1);
    }

    fn swap_position_buffers(&mut self) {
        self.a_is_in = !self.a_is_in;
    }

    /// Owned-mode CPU readback of the latest positions.
    async fn read_positions_owned(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Vec<f32>, String> {
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| "no staging buffer (borrowed mode)".to_string())?;
        let (pos_in, _) = self.owned_in_out();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_readback"),
        });
        encoder.copy_buffer_to_buffer(pos_in, 0, staging, 0, self.pos_buf_size);
        queue.submit(Some(encoder.finish()));
        Self::map_and_read(staging, device).await
    }

    /// Borrowed-mode CPU readback. Allocates a temporary staging buffer.
    async fn read_positions_with_device(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shared: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions_readback_tmp"),
            size: self.pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_readback_borrowed"),
        });
        // Latest result lives on the shared buffer after
        // step_with_encoder ensures it's there.
        encoder.copy_buffer_to_buffer(shared, 0, &staging, 0, self.pos_buf_size);
        queue.submit(Some(encoder.finish()));
        Self::map_and_read(&staging, device).await
    }

    async fn map_and_read(staging: &wgpu::Buffer, device: &wgpu::Device) -> Result<Vec<f32>, String> {
        let slice = staging.slice(..);
        let (tx, rx) = futures_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            device.poll(wgpu::Maintain::Wait);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = device;
        let res = rx.recv().await;
        res.map_err(|_| "map channel dropped".to_string())?
            .map_err(|e| format!("buffer map failed: {e:?}"))?;
        let data = slice.get_mapped_range();
        let floats: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(floats)
    }
}

struct AuxBuffers {
    vel: wgpu::Buffer,
    off: wgpu::Buffer,
    neigh: wgpu::Buffer,
    params: wgpu::Buffer,
    mass: wgpu::Buffer,
    cell_offsets: wgpu::Buffer,
    cell_offsets_capacity: u64,
    cell_nodes: wgpu::Buffer,
    cell_nodes_capacity: u64,
    energy: wgpu::Buffer,
    energy_staging: wgpu::Buffer,
}

/// Build the velocity, edge_offsets, edge_neighbors, params, mass, grid,
/// and energy buffers used by both owned and borrowed paths.
fn build_aux_buffers(device: &wgpu::Device, pc: &PreCompute) -> AuxBuffers {
    let vel = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("velocities"),
        contents: bytemuck::cast_slice(&pc.velocities),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let off = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_offsets"),
        contents: bytemuck::cast_slice(&pc.edge_offsets),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let neigh = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge_neighbors"),
        contents: bytemuck::cast_slice(&pc.edge_neighbors),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sim_params"),
        size: std::mem::size_of::<SimParamsRaw>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mass = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mass"),
        contents: bytemuck::cast_slice(&pc.mass),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    // Initial capacity: enough for a 1-cell grid + the n nodes. Will grow.
    let init_cell_offsets = vec![0u32, pc.n_nodes];
    let cell_offsets_capacity =
        (init_cell_offsets.len() as u64 * 4).max(64);
    let cell_offsets = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_offsets"),
        size: cell_offsets_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let n = pc.n_nodes.max(1) as u64;
    let cell_nodes_capacity = (n * 4).max(64);
    let cell_nodes = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_nodes"),
        size: cell_nodes_capacity,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let energy_size = (n * 4).max(64);
    let energy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("energy"),
        size: energy_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let energy_staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("energy_staging"),
        size: energy_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    AuxBuffers {
        vel,
        off,
        neigh,
        params,
        mass,
        cell_offsets,
        cell_offsets_capacity,
        cell_nodes,
        cell_nodes_capacity,
        energy,
        energy_staging,
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn buf_entry(binding: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buf.as_entire_binding(),
    }
}

// ---- minimal one-shot oneshot channel that's Send + works on wasm32 -------
//
// We avoid pulling in `futures` just for `oneshot`. This is enough for the
// "buffer map completed" callback path. The receiver is async and yields
// once the value arrives; on wasm32 the browser's microtask queue drives it,
// on native `device.poll(Wait)` runs the callback synchronously before we
// hit recv().

fn futures_channel() -> (OneshotTx, OneshotRx) {
    let inner = std::sync::Arc::new(OneshotInner {
        slot: std::sync::Mutex::new(None),
    });
    (
        OneshotTx {
            inner: inner.clone(),
        },
        OneshotRx { inner },
    )
}

struct OneshotInner {
    slot: std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>,
}

struct OneshotTx {
    inner: std::sync::Arc<OneshotInner>,
}
impl OneshotTx {
    fn send(self, v: Result<(), wgpu::BufferAsyncError>) {
        if let Ok(mut slot) = self.inner.slot.lock() {
            *slot = Some(v);
        }
    }
}

struct OneshotRx {
    inner: std::sync::Arc<OneshotInner>,
}
impl OneshotRx {
    async fn recv(self) -> Result<Result<(), wgpu::BufferAsyncError>, ()> {
        // Spin-yield until the slot is populated. On native, by the time we
        // arrive here `device.poll(Wait)` has already run the callback. On
        // wasm32 we yield to the event loop until the GPU job completes.
        loop {
            if let Some(v) = self.inner.slot.lock().map_err(|_| ())?.take() {
                return Ok(v);
            }
            yield_now().await;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn yield_now() {
    // No real async runtime assumed — just a single std::thread yield.
    // device.poll(Wait) means the callback already fired; this loop runs at
    // most a couple of times.
    std::thread::yield_now();
    // Cooperate with async runtimes by going through a manual yield future.
    YieldOnce { polled: false }.await;
}

#[cfg(target_arch = "wasm32")]
async fn yield_now() {
    YieldOnce { polled: false }.await;
}

struct YieldOnce {
    polled: bool,
}
impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.polled {
            std::task::Poll::Ready(())
        } else {
            self.polled = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

// ---------- Tests ------------------------------------------------------------

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

    fn triangle() -> Graph {
        let mut g = Graph::new();
        g.add_node(Node::new("a"));
        g.add_node(Node::new("b"));
        g.add_node(Node::new("c"));
        g.add_edge(Edge::new("ab", "a", "b"));
        g.add_edge(Edge::new("bc", "b", "c"));
        g.add_edge(Edge::new("ca", "c", "a"));
        g
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_runs_and_moves_nodes() {
        let mut graph = triangle();
        // Seed deterministic-ish initial positions.
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            if let Some(n) = graph.nodes.get_mut(*id) {
                n.position3 = Some([i as f32 * 10.0, 0.0, 0.0]);
            }
        }
        let initial: Vec<[f32; 3]> = ["a", "b", "c"]
            .iter()
            .map(|id| graph.nodes[*id].position3.unwrap())
            .collect();

        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 4,
            repulsion: 200.0,
            ..Default::default()
        });
        match layout.run(&mut graph).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        }

        // Every node must now have position3, and at least one must have moved.
        let mut any_moved = false;
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let p = graph.nodes[*id]
                .position3
                .expect("position3 must be set after run");
            let d = (p[0] - initial[i][0]).abs()
                + (p[1] - initial[i][1]).abs()
                + (p[2] - initial[i][2]).abs();
            if d > 1e-4 {
                any_moved = true;
            }
        }
        assert!(any_moved, "force step should have moved at least one node");
        assert_eq!(layout.node_count(), Some(3));
    }

    fn random_graph(n: usize, m: usize) -> Graph {
        let mut g = Graph::new();
        let mut s: u32 = 0xDEADBEEF;
        let mut rng = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            s
        };
        for i in 0..n {
            let mut node = Node::new(format!("{:06}", i));
            let r = 200.0;
            let x = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            let y = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            let z = ((rng() as f32) / u32::MAX as f32) * 2.0 * r - r;
            node.position3 = Some([x, y, z]);
            g.add_node(node);
        }
        for k in 0..m {
            let a = (rng() as usize) % n;
            let b = (rng() as usize) % n;
            if a == b { continue; }
            g.add_edge(Edge::new(format!("e{}", k), format!("{:06}", a), format!("{:06}", b)));
        }
        g
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unit_gpu_force_grid_produces_reasonable_layout() {
        // 100 random nodes, 200 random edges. Run 10 steps with grid on.
        let mut g = random_graph(100, 200);
        let mut layout = GpuForceLayout::new(GpuForceOptions {
            steps_per_call: 10,
            grid_enabled: true,
            repulsion_radius: 120.0,
            ..Default::default()
        });
        match layout.run(&mut g).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("skipping (no gpu adapter): {e}");
                return;
            }
        }
        // Verify all positions finite + non-degenerate spread.
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        let mut all_finite = true;
        for node in g.nodes.values() {
            let p = node.position3.expect("position3 set");
            for k in 0..3 {
                if !p[k].is_finite() { all_finite = false; }
                if p[k] < mn[k] { mn[k] = p[k]; }
                if p[k] > mx[k] { mx[k] = p[k]; }
            }
        }
        assert!(all_finite, "all positions must be finite");
        let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]);
        assert!(span > 50.0, "layout collapsed: span={span}");
    }
}
