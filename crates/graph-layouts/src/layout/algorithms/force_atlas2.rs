//! ForceAtlas2 — physics layout skeleton (CPU stub).
//!
//! This file registers the layout shell so the renderer can select it from
//! the sidebar. `step_with_encoder` is a no-op pending the GPU compute
//! shader. Settings are wired through to the UI so future work plugs in
//! the force model (LinLog, strong gravity, edge-weight, Barnes-Hut)
//! without touching registration plumbing.

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, PhysicsLayout,
};
use crate::types::Graph;

/// Tunables for the ForceAtlas2 physics layout. All fields are wired
/// through to the sidebar UI; the actual force model is unimplemented
/// (see crate-level docs).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForceAtlas2Settings {
    pub gravity: f32,
    pub strong_gravity: bool,
    pub scaling_ratio: f32,
    pub edge_weight_influence: f32,
    pub jitter_tolerance: f32,
    pub lin_log_mode: bool,
    pub prevent_overlap: bool,
    pub barnes_hut: bool,
    pub barnes_hut_theta: f32,
    pub time_step: f32,
    pub steps_per_frame: u32,
}

impl Default for ForceAtlas2Settings {
    fn default() -> Self {
        Self {
            gravity: 1.0,
            strong_gravity: false,
            scaling_ratio: 2.0,
            edge_weight_influence: 1.0,
            jitter_tolerance: 1.0,
            lin_log_mode: false,
            prevent_overlap: false,
            barnes_hut: true,
            barnes_hut_theta: 0.5,
            time_step: 1.0,
            steps_per_frame: 1,
        }
    }
}

/// ForceAtlas2 layout — skeleton; `step_with_encoder` is a no-op.
pub struct ForceAtlas2Layout {
    settings: ForceAtlas2Settings,
}

impl PhysicsLayout for ForceAtlas2Layout {
    type Settings = ForceAtlas2Settings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "force_atlas2",
            kind: LayoutKind::Physics,
            display_name: "ForceAtlas2 (skeleton)",
            description: "ForceAtlas2 physics layout — registration shell; force model TBD.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn new(settings: Self::Settings) -> Self {
        Self { settings }
    }

    fn init_with_device(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _graph: &Graph,
        _positions_buf: &wgpu::Buffer,
    ) -> Result<(), String> {
        Ok(())
    }

    fn step_with_encoder(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        _positions_buf: &wgpu::Buffer,
    ) {
        // Stub: no force model wired yet.
    }

    fn set_settings(&mut self, settings: Self::Settings) {
        self.settings = settings;
    }

    fn settings(&self) -> &Self::Settings {
        &self.settings
    }
}
