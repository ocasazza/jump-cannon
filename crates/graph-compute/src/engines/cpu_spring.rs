//! CPU spring-only fallback engine (`"cpu-spring"`).
//!
//! Wraps the existing `sim::cpu_step` reference integrator in the
//! [`LayoutEngine`] trait so a host without a usable wgpu adapter still has a
//! registered, selectable engine — and so the registry, not the sim loop, owns
//! the GPU-vs-CPU choice. Spring-only (no repulsion); NOT meant to scale. It is
//! the documented fallback referenced in `docs/compute-architecture.md` §1.
//!
//! `init` accepts a `None` GPU context (it never touches the GPU), so the sim
//! loop can fall back to this engine when `Fa2BruteEngine::init` fails for lack
//! of an adapter.

use graph_layouts::{LayoutDescriptor, LayoutKind, LayoutRequirements};
use serde::{Deserialize, Serialize};

use super::{CsrShard, EngineCtx, LayoutEngine, StepOutput};
use crate::sim::{cpu_step, CsrGraph};

/// Stable registry key for this engine.
pub const LAYOUT_ID: &str = "cpu-spring";

/// Tunables for the CPU spring integrator. Serde-roundtrippable for the wire
/// (ADR-002).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuSpringSettings {
    /// Integration time step.
    pub time_step: f32,
}

impl Default for CpuSpringSettings {
    fn default() -> Self {
        // Matches the old sim-loop dt for a 30 Hz tick — preserves prior
        // behavior when no params are supplied.
        Self {
            time_step: 1.0 / 30.0,
        }
    }
}

/// CPU spring-only engine. Holds its own working copy of positions + graph so
/// `step` is self-contained (the trait's `step` takes no graph argument).
pub struct CpuSpringEngine {
    descriptor: LayoutDescriptor,
    settings: CpuSpringSettings,
    graph: Option<CsrGraph>,
    positions: Vec<f32>,
}

impl CpuSpringEngine {
    pub const ID: &'static str = LAYOUT_ID;

    pub fn new() -> Self {
        Self {
            descriptor: Self::descriptor_static(),
            settings: CpuSpringSettings::default(),
            graph: None,
            positions: Vec::new(),
        }
    }

    fn descriptor_static() -> LayoutDescriptor {
        LayoutDescriptor {
            id: LAYOUT_ID,
            kind: LayoutKind::Physics,
            display_name: "CPU spring (fallback)",
            description: "Serial spring-only integrator. Runs anywhere (no GPU); \
                          used as the fallback on hosts without a wgpu adapter.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: true,
                needs_gpu_positions_buffer: false,
            },
        }
    }
}

impl Default for CpuSpringEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine for CpuSpringEngine {
    fn descriptor(&self) -> &LayoutDescriptor {
        &self.descriptor
    }

    fn set_params(&mut self, params: &serde_json::Value) -> Result<(), String> {
        if params.is_null() {
            return Ok(());
        }
        let typed: CpuSpringSettings = serde_json::from_value(params.clone())
            .map_err(|e| format!("decode cpu-spring settings: {e}"))?;
        self.settings = typed;
        Ok(())
    }

    fn init(
        &mut self,
        _ctx: &mut EngineCtx,
        graph: &CsrShard,
        positions: &[f32],
    ) -> Result<(), String> {
        let g = graph.graph;
        let n = g.n_nodes as usize;
        if positions.len() != 3 * n {
            return Err(format!(
                "initial positions length {} != 3 * n_nodes {}",
                positions.len(),
                3 * n
            ));
        }
        self.graph = Some(g.clone());
        self.positions = positions.to_vec();
        Ok(())
    }

    fn step(&mut self, _ctx: &mut EngineCtx) -> StepOutput {
        let graph = self
            .graph
            .as_ref()
            .expect("cpu-spring step called before successful init");
        let next = cpu_step(graph, &self.positions, self.settings.time_step);
        self.positions = next.clone();
        StepOutput::positions_only(next)
    }
}
