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
}

impl Default for GpuForceOptions {
    fn default() -> Self {
        Self {
            repulsion: 50.0,
            spring_k: 0.05,
            spring_len: 30.0,
            gravity: 0.001,
            damping: 0.85,
            dt: 0.016,
            cursor_pos: [0.0; 3],
            cursor_radius: 0.0,
            cursor_strength: 0.0,
            steps_per_call: 1,
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
        let mut st = s.serialize_struct("GpuForceOptions", 11)?;
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
        })
    }
}

pub struct GpuForceLayout {
    options: GpuForceOptions,
    state: Option<GpuState>,
}

impl GpuForceLayout {
    pub fn new(options: GpuForceOptions) -> Self {
        Self {
            options,
            state: None,
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
    pub async fn run(&mut self, graph: &mut Graph) -> Result<(), String> {
        // (Re)build GPU state if topology changed or this is the first run.
        let needs_rebuild = match &self.state {
            None => true,
            Some(state) => {
                state.n_nodes as usize != graph.nodes.len()
                    || state.n_edges as usize != graph.edges.len()
            }
        };
        if needs_rebuild {
            self.state = Some(GpuState::new(graph).await?);
        }

        let state = self.state.as_mut().unwrap();
        state.write_params(&self.options);
        for _ in 0..self.options.steps_per_call.max(1) {
            state.dispatch_step();
            state.swap_position_buffers();
        }
        let positions = state.read_positions().await?;
        // Write back into the graph in the same id-order we built the buffer.
        for (id, p) in state.node_order.iter().zip(positions.chunks_exact(4)) {
            if let Some(node) = graph.nodes.get_mut(id) {
                node.position3 = Some([p[0], p[1], p[2]]);
            }
        }
        Ok(())
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
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

// Each vec3<f32> in a storage buffer occupies 16 bytes (vec3 has stride/align
// of 16 in WGSL). We use a 4-component layout on the CPU side to match.
const VEC3_STRIDE: u64 = 16;

struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    pos_a: wgpu::Buffer,
    pos_b: wgpu::Buffer,
    /// True while pos_a is the "in" and pos_b is the "out" buffer.
    a_is_in: bool,
    velocities: wgpu::Buffer,
    edge_offsets: wgpu::Buffer,
    edge_neighbors: wgpu::Buffer,
    params_buf: wgpu::Buffer,
    staging: wgpu::Buffer,

    n_nodes: u32,
    n_edges: u32,
    pos_buf_size: u64,

    /// Stable node-id ordering used to interpret the position buffer.
    node_order: Vec<String>,
}

impl GpuState {
    async fn new(graph: &Graph) -> Result<Self, String> {
        let n_nodes = graph.nodes.len() as u32;
        let n_edges = graph.edges.len() as u32;

        // ---- Stable node ordering + id->idx map ----
        let mut node_order: Vec<String> = graph.nodes.keys().cloned().collect();
        node_order.sort();
        let id_to_idx: std::collections::HashMap<&str, u32> = node_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i as u32))
            .collect();

        // ---- Initial positions (vec3-as-vec4 padded) ----
        // Use position3 if present, else random sphere of radius sqrt(n)*5.
        let radius = ((n_nodes as f32).max(1.0).sqrt()) * 5.0;
        let mut positions: Vec<f32> = Vec::with_capacity(n_nodes as usize * 4);
        // Tiny LCG so we don't have to drag rand traits across cfg(target).
        let mut seed: u32 = 0x9E37_79B1;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        for id in &node_order {
            let n = &graph.nodes[id];
            let p = n.position3.unwrap_or_else(|| {
                [next() * radius, next() * radius, next() * radius]
            });
            positions.extend_from_slice(&[p[0], p[1], p[2], 0.0]);
        }
        let velocities: Vec<f32> = vec![0.0; n_nodes as usize * 4];

        // ---- CSR adjacency (undirected) ----
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
        // Storage buffers must be non-empty.
        if edge_neighbors.is_empty() {
            edge_neighbors.push(0);
        }

        // ---- Adapter / device ----
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| {
                "no GPU adapter (try a fallback adapter or skip)".to_string()
            })?;

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

        // ---- Buffers ----
        let pos_buf_size = (n_nodes as u64).max(1) * VEC3_STRIDE;
        let pos_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_a"),
            contents: bytemuck::cast_slice(&positions),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let pos_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("positions_b"),
            contents: bytemuck::cast_slice(&positions),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let vel = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("velocities"),
            contents: bytemuck::cast_slice(&velocities),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let off = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge_offsets"),
            contents: bytemuck::cast_slice(&edge_offsets),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let neigh = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge_neighbors"),
            contents: bytemuck::cast_slice(&edge_neighbors),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sim_params"),
            size: std::mem::size_of::<SimParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("positions_staging"),
            size: pos_buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // ---- Pipeline ----
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("force.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "shaders/force.wgsl"
            ))),
        });

        let bgl_entries = [
            // 0: positions_in (read-only storage)
            storage_entry(0, true),
            // 1: positions_out
            storage_entry(1, false),
            // 2: velocities
            storage_entry(2, false),
            // 3: edge_offsets (read-only)
            storage_entry(3, true),
            // 4: edge_neighbors (read-only)
            storage_entry(4, true),
            // 5: params (uniform)
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

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            pos_a,
            pos_b,
            a_is_in: true,
            velocities: vel,
            edge_offsets: off,
            edge_neighbors: neigh,
            params_buf,
            staging,
            n_nodes,
            n_edges,
            pos_buf_size,
            node_order,
        })
    }

    fn write_params(&self, opts: &GpuForceOptions) {
        let raw = SimParamsRaw {
            repulsion: opts.repulsion,
            spring_k: opts.spring_k,
            spring_len: opts.spring_len,
            gravity: opts.gravity,
            damping: opts.damping,
            dt: opts.dt,
            cursor_radius: opts.cursor_radius,
            cursor_strength: opts.cursor_strength,
            cursor_pos: opts.cursor_pos,
            n_nodes: self.n_nodes,
            n_edges: self.n_edges,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&raw));
    }

    fn current_in_out(&self) -> (&wgpu::Buffer, &wgpu::Buffer) {
        if self.a_is_in {
            (&self.pos_a, &self.pos_b)
        } else {
            (&self.pos_b, &self.pos_a)
        }
    }

    fn dispatch_step(&self) {
        let (pos_in, pos_out) = self.current_in_out();
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_force_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                buf_entry(0, pos_in),
                buf_entry(1, pos_out),
                buf_entry(2, &self.velocities),
                buf_entry(3, &self.edge_offsets),
                buf_entry(4, &self.edge_neighbors),
                buf_entry(5, &self.params_buf),
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu_force_cmd"),
            });
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("force_step_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.pipeline);
            cpass.set_bind_group(0, &bind_group, &[]);
            let groups = (self.n_nodes + 63) / 64;
            cpass.dispatch_workgroups(groups.max(1), 1, 1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    fn swap_position_buffers(&mut self) {
        self.a_is_in = !self.a_is_in;
    }

    /// Copy the current "in" position buffer (= last-written "out" before the
    /// swap, or the initial buffer if zero steps ran) back to the CPU.
    async fn read_positions(&self) -> Result<Vec<f32>, String> {
        let (pos_in, _) = self.current_in_out();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu_force_readback"),
            });
        encoder.copy_buffer_to_buffer(pos_in, 0, &self.staging, 0, self.pos_buf_size);
        self.queue.submit(Some(encoder.finish()));

        let slice = self.staging.slice(..);
        let (tx, rx) = futures_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        // Drive the device until the map completes. On wasm32 the browser
        // event loop drives this; on native we poll.
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.device.poll(wgpu::Maintain::Wait);
        }
        let res = rx.recv().await;
        res.map_err(|_| "map channel dropped".to_string())?
            .map_err(|e| format!("buffer map failed: {e:?}"))?;

        let data = slice.get_mapped_range();
        // Layout: [x,y,z,_pad] per node.
        let floats: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
        drop(data);
        self.staging.unmap();
        Ok(floats)
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
}
