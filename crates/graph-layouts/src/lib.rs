use wasm_bindgen::prelude::*;

mod types;
mod layout;
mod benchmark;
mod file_parsers;
mod utils;

pub use types::{Graph, Node, Edge, Id, MetadataValue, LayoutOptions};
pub use layout::algorithms::fcose::{FcoseLayout, FcoseQuality, FcoseSettings};
pub use layout::algorithms::cose_bilkent::{CoseBilkentLayout, CoseBilkentSettings};
pub use layout::algorithms::cise::{CiseLayout, CiseSettings};
pub use layout::algorithms::dagre::{DagreLayout, DagreRanker, DagreSettings, RankDirection};
pub use layout::algorithms::klay::{KlayLayout, KlaySettings};
pub use layout::algorithms::gpu_force::{GpuForceLayout, GpuForceOptions, RepulsionMode, SeedMode};
pub use layout::algorithms::random::{RandomLayout, RandomSettings};
pub use layout::algorithms::circle::{CircleAxis, CircleLayout, CircleSettings};
pub use layout::algorithms::grid::{GridLayout, GridSettings};
pub use layout::algorithms::sphere::{SphereLayout, SphereSettings};
pub use layout::algorithms::concentric_static::{ConcentricLayout, ConcentricMetric, ConcentricSettings};
pub use layout::algorithms::hilbert::{HilbertLayout, HilbertSettings};
pub use layout::algorithms::spectral::{SpectralLayout, SpectralSettings};
pub use layout::layout_trait::{
    BoxedPhysics, BoxedStatic, DynPhysicsLayout, DynStaticLayout, LayoutDescriptor, LayoutId,
    LayoutKind, LayoutRequirements, LayoutSettings, PhysicsLayout, StaticLayout,
};
pub use layout::coarsen::{coarsen, prolong, warmup_positions, CoarseLevel, Coarsening};

// Topological-fisheye coarsening (Gansner-Koren-North §4) — canonical home
// for the algorithm. Used both as the `SeedMode::TopoFisheye` seeder for
// the GPU force sim and as the §4 input to graph-compute's interactive
// hybrid-graph + distortion RPC (paper §5–§6).
pub mod topo_fisheye {
    pub use crate::layout::topo_fisheye::*;
}

pub use benchmark::{run_benchmark, run_all_benchmarks};
use file_parsers::parse_graph_file;

// When the `wee_alloc` feature is enabled, use `wee_alloc` as the global allocator.
#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen]
pub fn set_panic_hook() {
    // When the `console_error_panic_hook` feature is enabled, we can call the
    // `set_panic_hook` function to get better error messages if our code ever panics.
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct LayoutManager {
    graph: Graph,
    /// Lazily-initialised GPU force engine. Holds wgpu device/queue/buffers
    /// once `init_gpu_force` runs.
    gpu_force: Option<GpuForceLayout>,
}

#[wasm_bindgen]
impl LayoutManager {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        set_panic_hook();
        Self {
            graph: Graph::new(),
            gpu_force: None,
        }
    }

    /// Add a node to the graph
    pub fn add_node(&mut self, id: String, x: Option<f64>, y: Option<f64>) {
        let mut node = Node::new(id);
        if let (Some(x_val), Some(y_val)) = (x, y) {
            node = node.with_position(x_val, y_val);
        }
        self.graph.add_node(node);
    }

    /// Add an edge to the graph
    pub fn add_edge(&mut self, id: String, source: String, target: String) {
        let edge = Edge::new(id, source, target);
        self.graph.add_edge(edge);
    }

    /// Remove a node from the graph
    pub fn remove_node(&mut self, id: String) {
        self.graph.remove_node(&id);
    }

    /// Remove an edge from the graph
    pub fn remove_edge(&mut self, id: String) {
        self.graph.remove_edge(&id);
    }

    /// Get the current graph state as JSON
    pub fn get_graph_json(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.graph)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize graph: {}", e)))
    }

    /// Load a graph from JSON
    pub fn load_graph_json(&mut self, json: String) -> Result<(), JsValue> {
        self.graph = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse graph: {}", e)))?;
        Ok(())
    }

    /// Parse and load a graph from various file formats
    pub fn parse_and_load_graph(&mut self, content: String, file_type: String) -> Result<(), JsValue> {
        self.graph = parse_graph_file(&content, &file_type)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse file: {}", e)))?;
        Ok(())
    }
}

/// Convenience native helper: spin up a one-shot GPU force layout, run
/// `options.steps_per_call` steps, and write positions into the graph.
/// For long-running per-frame use, hold a [`GpuForceLayout`] yourself.
pub async fn run_gpu_force_native(
    graph: &mut Graph,
    options: &GpuForceOptions,
) -> Result<(), String> {
    let mut layout = GpuForceLayout::new(options.clone());
    layout.run(graph).await
}

// ---------- WASM bindings for the GPU force backend -------------------------

#[wasm_bindgen]
impl LayoutManager {
    /// Initialise (or reset) the GPU force engine for the current graph
    /// topology. Subsequent `step_gpu_force` calls reuse the GPU resources.
    /// `options_json` is a JSON object mirroring `GpuForceOptions`.
    pub async fn init_gpu_force(&mut self, options_json: String) -> Result<(), JsValue> {
        let opts: GpuForceOptions = serde_json::from_str(&options_json)
            .map_err(|e| JsValue::from_str(&format!("parse options: {e}")))?;
        // Stash the layout; GPU resources are built lazily on first run().
        self.gpu_force = Some(GpuForceLayout::new(opts));
        // Run a single step to materialise GPU state + write back positions.
        if let Some(l) = self.gpu_force.as_mut() {
            l.run(&mut self.graph)
                .await
                .map_err(|e| JsValue::from_str(&format!("init: {e}")))?;
        }
        Ok(())
    }

    /// Run `steps_per_call` simulation steps. Returns positions packed as
    /// `[x0,y0,z0, x1,y1,z1, ...]` in the manager's stable id-sorted order.
    pub async fn step_gpu_force(&mut self) -> Result<js_sys::Float32Array, JsValue> {
        let Some(layout) = self.gpu_force.as_mut() else {
            return Err(JsValue::from_str(
                "gpu_force not initialised; call init_gpu_force first",
            ));
        };
        layout
            .run(&mut self.graph)
            .await
            .map_err(|e| JsValue::from_str(&format!("step: {e}")))?;

        // Pack [x,y,z] in id-sorted order for stable downstream indexing.
        let mut ids: Vec<&String> = self.graph.nodes.keys().collect();
        ids.sort();
        let mut out: Vec<f32> = Vec::with_capacity(ids.len() * 3);
        for id in ids {
            let p = self.graph.nodes[id].position3.unwrap_or([0.0, 0.0, 0.0]);
            out.extend_from_slice(&p);
        }
        let arr = js_sys::Float32Array::new_with_length(out.len() as u32);
        arr.copy_from(&out);
        Ok(arr)
    }

    /// Update mutable options without rebuilding GPU state (cursor force,
    /// slider tweaks, etc.).
    pub fn update_gpu_force_options(&mut self, options_json: String) -> Result<(), JsValue> {
        let opts: GpuForceOptions = serde_json::from_str(&options_json)
            .map_err(|e| JsValue::from_str(&format!("parse options: {e}")))?;
        if let Some(l) = self.gpu_force.as_mut() {
            l.set_options(opts);
            Ok(())
        } else {
            Err(JsValue::from_str("gpu_force not initialised"))
        }
    }
}

// CLI interface for running benchmarks
#[cfg(all(feature = "cli", not(target_arch = "wasm32")))]
pub fn main() {
    use std::env;

    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} benchmark <output_csv_path>", args[0]);
        std::process::exit(1);
    }

    let command = &args[1];
    let output_path = &args[2];

    match command.as_str() {
        "benchmark" => {
            match run_all_benchmarks(output_path) {
                Ok(_) => println!("Benchmarks completed successfully"),
                Err(e) => {
                    eprintln!("Error running benchmarks: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            std::process::exit(1);
        }
    }
}
pub mod geometric;
