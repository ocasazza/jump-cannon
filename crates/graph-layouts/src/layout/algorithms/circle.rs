//! Circle static layout — evenly-spaced nodes around a great circle.
//!
//! One-shot CPU solver. Picks an axis to define which plane the circle
//! lies in (default Z = circle in the XY plane).

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CircleAxis {
    X,
    Y,
    #[default]
    Z,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CircleSettings {
    pub radius: f32,
    pub axis: CircleAxis,
}

impl Default for CircleSettings {
    fn default() -> Self {
        Self {
            radius: 200.0,
            axis: CircleAxis::Z,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CircleLayout;

impl StaticLayout for CircleLayout {
    type Settings = CircleSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "circle",
            kind: LayoutKind::Static,
            display_name: "Circle",
            description: "Lay nodes evenly around a great circle on the chosen axis.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
        let mut node_order: Vec<&String> = graph.nodes.keys().collect();
        node_order.sort();

        let n = node_order.len();
        let mut out: Vec<f32> = Vec::with_capacity(n * 3);
        let radius = settings.radius.max(0.0);
        let denom = n.max(1) as f32;

        for i in 0..n {
            let theta = std::f32::consts::TAU * (i as f32) / denom;
            let c = theta.cos() * radius;
            let s = theta.sin() * radius;
            // Place the 2D circle (c, s) in the plane perpendicular to the
            // chosen axis. Z = circle in xy plane (default).
            let (x, y, z) = match settings.axis {
                CircleAxis::Z => (c, s, 0.0),
                CircleAxis::X => (0.0, c, s),
                CircleAxis::Y => (c, 0.0, s),
            };
            out.push(x);
            out.push(y);
            out.push(z);
        }

        Ok(out)
    }
}
