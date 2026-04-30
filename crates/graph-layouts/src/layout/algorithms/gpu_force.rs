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
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

/// State of the asynchronous energy_buf -> energy_staging readback. Shared
/// between the main thread and the wgpu map_async callback via Arc<Mutex<>>.
#[derive(Debug)]
enum EnergyReadback {
    /// No copy in flight; staging buffer is unmapped and idle.
    Idle,
    /// `copy_buffer_to_buffer` recorded + `map_async` issued; waiting for
    /// the GPU + driver to call our callback.
    Mapping,
    /// Callback fired. Ok = mapped successfully (caller must
    /// `get_mapped_range` + `unmap`); Err = map failed (no unmap needed).
    Mapped(Result<(), String>),
}

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
    /// Once max-KE has stayed below `energy_threshold` for `HALT_FRAMES`
    /// consecutive observed readbacks, the sim is considered settled and
    /// `step_with_encoder` becomes a no-op until something calls `wake()` or
    /// updates options in a way that perturbs the system.
    halted: bool,
    halt_streak: u32,
    /// Step count since last wake. Halt is suppressed until this exceeds
    /// `HALT_GRACE_STEPS` so the sim can break out of degenerate initial
    /// conditions (e.g., uniform sphere, ring) before being declared settled.
    steps_since_wake: u32,
    /// Most recent max-KE reduction value (for diagnostics / stats UI).
    last_max_ke: f32,
}

/// How many consecutive low-KE readbacks we require before halting. With
/// `steps_per_call = 8` and ~60fps this is half a second of "settled" before
/// we flip to halt.
const HALT_FRAMES: u32 = 30;

/// Minimum number of compute dispatches before halting becomes possible.
/// Prevents premature halt in the early "everything is at uniform low velocity"
/// phase that happens with random sphere seeding.
const HALT_GRACE_STEPS: u32 = 600;

impl GpuForceLayout {
    pub fn new(options: GpuForceOptions) -> Self {
        Self {
            options,
            state: None,
            owned_device: None,
            halted: false,
            halt_streak: 0,
            steps_since_wake: 0,
            last_max_ke: 0.0,
        }
    }

    pub fn set_options(&mut self, options: GpuForceOptions) {
        // Any param change wakes the sim — sliders/cursor force/preset switch
        // are the user telling us "do work again". The cheap correctness-first
        // policy is to always wake; the energy-halt logic will re-settle on
        // its own once the new params produce a steady state.
        self.options = options;
        self.wake();
    }

    /// Re-activate a halted sim. Call this from JS / cursor tool / preset
    /// switch / anywhere that perturbs the layout from the outside.
    pub fn wake(&mut self) {
        self.halted = false;
        self.halt_streak = 0;
        self.steps_since_wake = 0;
    }

    /// True once the sim has been observed below `energy_threshold` for
    /// [`HALT_FRAMES`] consecutive readbacks. While halted, `step_with_encoder`
    /// is a no-op.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Most recent max-per-node kinetic-energy proxy from the readback path.
    /// Returns 0.0 before the first readback completes.
    pub fn last_max_ke(&self) -> f32 {
        self.last_max_ke
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
        let total_steps = self.options.steps_per_call.max(1);
        for s in 0..total_steps {
            // Build the grid only on the first step (matches borrowed path's
            // "build once per call" cadence) and only if grid is enabled.
            let build_grid = s == 0 && self.options.grid_enabled;
            state.dispatch_step_direct(&od.device, &od.queue, build_grid);
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

        // Drive any pending native callbacks. On WASM the browser drives
        // map_async via the event loop; on native we have to poll. `Poll`
        // is non-blocking — if no GPU work has finished yet this just
        // returns immediately and the callback fires on a later frame.
        #[cfg(not(target_arch = "wasm32"))]
        {
            device.poll(wgpu::Maintain::Poll);
        }

        // Drain a previously-scheduled readback (if any) and update halt
        // bookkeeping. We do this BEFORE the early-return so that even after
        // halting we still unmap a stragglar staging buffer cleanly.
        if let Some(max_ke) = state.drain_energy_readback() {
            self.last_max_ke = max_ke;
            // Suppress halt during the grace period — even truly low velocities
            // early on are usually because the random initial layout hasn't
            // had time to gain energy yet, not because it's converged.
            if self.steps_since_wake >= HALT_GRACE_STEPS
                && self.options.energy_threshold > 0.0
                && max_ke < self.options.energy_threshold
            {
                self.halt_streak = self.halt_streak.saturating_add(1);
                if self.halt_streak >= HALT_FRAMES {
                    if !self.halted {
                        #[cfg(not(target_arch = "wasm32"))]
                        eprintln!(
                            "gpu_force: halted (max_ke={:.4} < threshold={:.4} after {} steps)",
                            max_ke, self.options.energy_threshold, self.steps_since_wake
                        );
                    }
                    self.halted = true;
                }
            } else {
                self.halt_streak = 0;
            }
        }

        if self.halted {
            // Sim is at rest. No dispatch, no readback. The renderer will
            // still draw the last positions (they live in the shared buffer).
            return;
        }

        // First-call init for cooling.
        if state.effective_damping <= 0.0 || state.effective_damping > 1.0 {
            state.effective_damping = self.options.damping;
        }
        let alpha = self.options.cooling_alpha.clamp(0.5, 1.0);
        let floor = self.options.cooling_floor.clamp(0.0, 1.0);
        state.effective_damping = (state.effective_damping * alpha).max(floor.min(self.options.damping));

        state.rebuild_and_upload_grid(device, queue, &self.options);
        state.write_params(queue, &self.options);
        // GPU-side bucket sort of positions into spatial-hash cells. Done
        // once per call (not per step), reading from whichever buffer is
        // currently the "in" side of the ping-pong.
        if self.options.grid_enabled {
            state.encode_grid_build_borrowed(device, encoder, shared_buffer);
        }
        let steps = self.options.steps_per_call.max(1);
        for _ in 0..steps {
            state.dispatch_borrowed_step(device, encoder, shared_buffer);
            state.swap_position_buffers();
        }
        self.steps_since_wake = self.steps_since_wake.saturating_add(steps);
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

        // Schedule an async energy readback for THIS frame's results. We
        // only kick off a new copy when the previous one has been drained
        // (state == Idle). If the GPU+CPU pipelines are running ahead of
        // the readback we'll just skip a frame's worth of measurements;
        // the halt detector tolerates that — its streak only counts
        // observed-low samples.
        let readback_idle = state
            .energy_readback
            .lock()
            .map(|g| matches!(*g, EnergyReadback::Idle))
            .unwrap_or(false);
        if readback_idle {
            state.schedule_energy_copy(encoder);
            // map_async itself is queued by wgpu; the actual callback won't
            // fire until the encoder is submitted by the caller and the GPU
            // finishes the copy. That's fine — we drain on the next frame.
            state.issue_energy_map();
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

    /// Pipelines for the GPU-side spatial-grid bucket sort. All four share
    /// `gb_bind_group_layout`.
    gb_clear_pipeline: wgpu::ComputePipeline,
    gb_count_pipeline: wgpu::ComputePipeline,
    gb_scan_pipeline: wgpu::ComputePipeline,
    gb_scatter_pipeline: wgpu::ComputePipeline,
    gb_bind_group_layout: wgpu::BindGroupLayout,

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
    /// Per-cell atomic counts (filled by count_cells, scanned into offsets).
    cell_counts_buf: wgpu::Buffer,
    cell_counts_capacity: u64,
    /// Per-cell atomic write cursor used by scatter_cells.
    cell_write_cursor_buf: wgpu::Buffer,
    cell_write_cursor_capacity: u64,
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

    /// Async energy-readback state. Shared with the wgpu map_async callback.
    /// On native, drained inside `step_with_encoder` after `device.poll(Poll)`;
    /// on WASM, the browser drives the callback between rAF ticks.
    energy_readback: Arc<Mutex<EnergyReadback>>,
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

struct ForcePipelines {
    force_step: wgpu::ComputePipeline,
    force_bgl: wgpu::BindGroupLayout,
    gb_clear: wgpu::ComputePipeline,
    gb_count: wgpu::ComputePipeline,
    gb_scan: wgpu::ComputePipeline,
    gb_scatter: wgpu::ComputePipeline,
    gb_bgl: wgpu::BindGroupLayout,
}

fn build_pipeline(device: &wgpu::Device) -> ForcePipelines {
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

    // Grid-build BGL (group 1 in shader). All four build entry points share
    // it; bindings an entry point doesn't reference are simply unused.
    let gb_bgl_entries = [
        storage_entry(0, true), // gb_positions_in
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        storage_entry(2, false), // gb_cell_counts (atomic rw)
        storage_entry(3, false), // gb_cell_cursor (atomic rw)
        storage_entry(4, false), // gb_cell_offsets (rw u32)
        storage_entry(5, false), // gb_cell_nodes   (rw u32)
    ];
    let gb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_grid_build_bgl"),
        entries: &gb_bgl_entries,
    });
    // Empty BGL placeholder at group(0) for build pipelines (the build
    // entry points only reference @group(1) bindings).
    let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu_force_grid_build_empty_bgl"),
        entries: &[],
    });

    let pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        },
    );
    let force_step = device.create_compute_pipeline(
        &wgpu::ComputePipelineDescriptor {
            label: Some("force_step"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("force_step"),
            compilation_options: Default::default(),
            cache: None,
        },
    );

    let gb_pipeline_layout = device.create_pipeline_layout(
        &wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_force_grid_build_pl"),
            bind_group_layouts: &[&empty_bgl, &gb_bgl],
            push_constant_ranges: &[],
        },
    );
    let mk = |name: &'static str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(name),
            layout: Some(&gb_pipeline_layout),
            module: &shader,
            entry_point: Some(name),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let gb_clear = mk("clear_cell_counts");
    let gb_count = mk("count_cells");
    let gb_scan = mk("scan_cell_offsets");
    let gb_scatter = mk("scatter_cells");

    ForcePipelines {
        force_step,
        force_bgl: bind_group_layout,
        gb_clear,
        gb_count,
        gb_scan,
        gb_scatter,
        gb_bgl,
    }
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
        let pipelines = build_pipeline(device);
        let cpu_positions = pc.initial_positions.clone();

        Ok(Self {
            pipeline: pipelines.force_step,
            bind_group_layout: pipelines.force_bgl,
            gb_clear_pipeline: pipelines.gb_clear,
            gb_count_pipeline: pipelines.gb_count,
            gb_scan_pipeline: pipelines.gb_scan,
            gb_scatter_pipeline: pipelines.gb_scatter,
            gb_bind_group_layout: pipelines.gb_bgl,
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
            cell_counts_buf: aux.cell_counts,
            cell_counts_capacity: aux.cell_counts_capacity,
            cell_write_cursor_buf: aux.cell_write_cursor,
            cell_write_cursor_capacity: aux.cell_write_cursor_capacity,
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
            energy_readback: Arc::new(Mutex::new(EnergyReadback::Idle)),
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
        let pipelines = build_pipeline(device);

        let _ = positions_buffer; // sized check happens via caller usage
        let cpu_positions = pc.initial_positions.clone();

        Ok(Self {
            pipeline: pipelines.force_step,
            bind_group_layout: pipelines.force_bgl,
            gb_clear_pipeline: pipelines.gb_clear,
            gb_count_pipeline: pipelines.gb_count,
            gb_scan_pipeline: pipelines.gb_scan,
            gb_scatter_pipeline: pipelines.gb_scatter,
            gb_bind_group_layout: pipelines.gb_bgl,
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
            cell_counts_buf: aux.cell_counts,
            cell_counts_capacity: aux.cell_counts_capacity,
            cell_write_cursor_buf: aux.cell_write_cursor,
            cell_write_cursor_capacity: aux.cell_write_cursor_capacity,
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
            energy_readback: Arc::new(Mutex::new(EnergyReadback::Idle)),
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
        let _ = queue;
        let cell_size_target = if opts.repulsion_radius > 0.0 {
            opts.repulsion_radius
        } else {
            (opts.spring_len * 4.0).max(1.0)
        };
        // Bbox + dims still computed CPU-side from the (possibly stale)
        // cpu_positions mirror — same as before this refactor. The
        // count+scatter use the *fresh* GPU positions buffer, so a slightly
        // stale bbox just means a slightly wider grid, which is harmless
        // (the in-shader clamp keeps every node in a valid cell).
        let (origin, cell_size, dim, n_cells, _co, _cn) =
            build_grid(&self.cpu_positions, self.n_nodes, cell_size_target);
        self.grid_origin = origin;
        self.grid_cell_size = cell_size;
        self.grid_dim = dim;
        self.n_cells = n_cells;

        // (Re)allocate cell-* buffers if the grid grew. We size everything
        // to (n_cells + 1) * 4 — cell_offsets needs the +1 sentinel; the
        // count/cursor buffers don't but it's fine to oversize.
        let needed_off_bytes = ((n_cells as u64 + 1) * 4).max(64);
        if needed_off_bytes > self.cell_offsets_capacity {
            let cap = needed_off_bytes * 2;
            self.cell_offsets_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_offsets"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_offsets_capacity = cap;
            // counts/cursor track cell_offsets capacity so all three resize
            // together — the atomic buffers can't be smaller than n_cells*4.
            self.cell_counts_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_counts"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.cell_counts_capacity = cap;
            self.cell_write_cursor_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_write_cursor"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            self.cell_write_cursor_capacity = cap;
        }
        let needed_nodes_bytes = (self.n_nodes.max(1) as u64 * 4).max(64);
        if needed_nodes_bytes > self.cell_nodes_capacity {
            let cap = needed_nodes_bytes * 2;
            self.cell_nodes_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell_nodes"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cell_nodes_capacity = cap;
        }
    }

    /// Build the gb_* bind group used by the four grid-build entry points.
    /// `pos_in` is the same positions buffer the upcoming `force_step` will
    /// read — that way the grid is built from the same positions force_step
    /// sees (no one-frame stale grid).
    fn make_grid_build_bg(
        &self,
        device: &wgpu::Device,
        pos_in: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_grid_build_bg"),
            layout: &self.gb_bind_group_layout,
            entries: &[
                buf_entry(0, pos_in),
                buf_entry(1, &self.params_buf),
                buf_entry(2, &self.cell_counts_buf),
                buf_entry(3, &self.cell_write_cursor_buf),
                buf_entry(4, &self.cell_offsets_buf),
                buf_entry(5, &self.cell_nodes_buf),
            ],
        })
    }

    /// Record the four grid-build dispatches (clear → count → scan → scatter)
    /// into `encoder`. After this returns, cell_offsets + cell_nodes hold a
    /// fresh bucket sort of `pos_in`. Each pass is its own compute pass so
    /// wgpu inserts the necessary storage-buffer barriers between them.
    fn encode_grid_build(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bg: &wgpu::BindGroup,
    ) {
        let cells_groups = (self.n_cells + 63) / 64;
        let nodes_groups = (self.n_nodes + 63) / 64;
        // 1. clear counts + cursor + offsets
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_clear_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_clear_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(cells_groups.max(1), 1, 1);
        }
        // 2. count per cell (atomic add)
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_count_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_count_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(nodes_groups.max(1), 1, 1);
        }
        // 3. exclusive prefix sum, single workgroup, single thread
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_scan_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_scan_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(1, 1, 1);
        }
        // 4. scatter node indices into cell_nodes via cursor atomicAdd
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("grid_scatter_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.gb_scatter_pipeline);
            cpass.set_bind_group(1, bg, &[]);
            cpass.dispatch_workgroups(nodes_groups.max(1), 1, 1);
        }
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
    /// Used by the legacy `run()` path (owned mode only). `build_grid`
    /// controls whether the grid bucket sort runs in the same submit
    /// (called once for the first step per call; subsequent steps within
    /// the same `run()` reuse the grid for symmetry with the borrowed
    /// path's "build once per call").
    fn dispatch_step_direct(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        build_grid: bool,
    ) {
        let (pos_in, pos_out) = self.owned_in_out();
        let bind_group = self.make_bind_group(device, pos_in, pos_out);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_force_cmd"),
        });
        if build_grid {
            let gb_bg = self.make_grid_build_bg(device, pos_in);
            self.encode_grid_build(&mut encoder, &gb_bg);
        }
        self.encode_compute(&mut encoder, &bind_group);
        queue.submit(Some(encoder.finish()));
    }

    /// Record dispatch into a caller-supplied encoder, reading/writing the
    /// borrowed shared buffer + internal pos_b. Caller is responsible for
    /// invoking `encode_grid_build_borrowed` before this if grid is enabled
    /// — `step_with_encoder` does that once per call (not per step).
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

    /// Borrowed-mode wrapper around `encode_grid_build` — builds the bind
    /// group bound to whichever position buffer is currently the "in" side.
    fn encode_grid_build_borrowed(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shared: &wgpu::Buffer,
    ) {
        let (pos_in, _pos_out) = self.borrowed_in_out(shared);
        let bg = self.make_grid_build_bg(device, pos_in);
        self.encode_grid_build(encoder, &bg);
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

    /// If the previous frame's `energy_staging` map_async has completed, read
    /// out the per-node KE values, take their max, unmap the staging buffer,
    /// and reset the readback state to Idle. Returns Some(max_ke) if a value
    /// was consumed this call, None if no completed map was waiting.
    ///
    /// This is the "drain on next frame" half of the deferred-readback pattern
    /// — we never block. If the GPU/driver hasn't finished the map yet we
    /// just report None and try again next frame.
    fn drain_energy_readback(&self) -> Option<f32> {
        // Take the lock briefly to inspect+transition state. We can't read
        // the mapped range while holding the mutex (and we don't need to —
        // the staging buffer's mapped slice is independent of this state).
        let take_action = {
            let mut guard = self.energy_readback.lock().ok()?;
            match &*guard {
                EnergyReadback::Mapped(Ok(())) => {
                    *guard = EnergyReadback::Idle;
                    Some(true)
                }
                EnergyReadback::Mapped(Err(_e)) => {
                    // Map failures are rare and self-recovering — silently
                    // reset to Idle and try again next frame.
                    *guard = EnergyReadback::Idle;
                    Some(false)
                }
                _ => None, // Idle or Mapping: nothing to drain.
            }
        };
        let ok = take_action?;
        if !ok {
            return None;
        }
        // Map succeeded: read, reduce, unmap.
        let slice = self.energy_staging.slice(..);
        let view = slice.get_mapped_range();
        let floats: &[f32] = bytemuck::cast_slice(&view);
        let n = (self.n_nodes as usize).min(floats.len());
        let mut max = 0.0f32;
        for &v in &floats[..n] {
            if v.is_finite() && v > max {
                max = v;
            }
        }
        drop(view);
        self.energy_staging.unmap();
        Some(max)
    }

    /// Record `energy_buf -> energy_staging` copy and schedule the
    /// non-blocking map_async. Safe to call only when state is Idle —
    /// remapping a buffer that's still mapped panics in wgpu. Caller is
    /// responsible for that check.
    fn schedule_energy_copy(
        &self,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let n_bytes = (self.n_nodes as u64) * 4;
        if n_bytes == 0 {
            return;
        }
        encoder.copy_buffer_to_buffer(
            &self.energy_buf,
            0,
            &self.energy_staging,
            0,
            n_bytes,
        );
        // Mark as Mapping *before* the async map fires so a slow callback
        // can't race a future drain.
        if let Ok(mut g) = self.energy_readback.lock() {
            *g = EnergyReadback::Mapping;
        }
    }

    /// Issue the actual `map_async` request on the energy_staging buffer.
    /// Must be called AFTER the encoder is submitted (the copy needs to be
    /// in flight). The callback signals completion via the shared Arc<Mutex<>>.
    fn issue_energy_map(&self) {
        let shared = self.energy_readback.clone();
        let slice = self.energy_staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |res| {
            if let Ok(mut g) = shared.lock() {
                *g = EnergyReadback::Mapped(res.map_err(|e| format!("{e:?}")));
            }
        });
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
    cell_counts: wgpu::Buffer,
    cell_counts_capacity: u64,
    cell_write_cursor: wgpu::Buffer,
    cell_write_cursor_capacity: u64,
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
    // GPU-side bucket-sort scratch: per-cell atomic counts + write cursor.
    // Sized to match cell_offsets at construction; both grow alongside it.
    let cell_counts_capacity = cell_offsets_capacity;
    let cell_counts = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_counts"),
        size: cell_counts_capacity,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let cell_write_cursor_capacity = cell_offsets_capacity;
    let cell_write_cursor = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cell_write_cursor"),
        size: cell_write_cursor_capacity,
        usage: wgpu::BufferUsages::STORAGE,
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
        cell_counts,
        cell_counts_capacity,
        cell_write_cursor,
        cell_write_cursor_capacity,
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
