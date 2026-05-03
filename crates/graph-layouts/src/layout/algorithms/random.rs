//! Random static layout — uniform sample inside a ball.
//!
//! One-shot CPU solver. Useful as a fresh seed before resuming a physics
//! sim, or as a deterministic fixture for tests via the `seed` knob.

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RandomSettings {
    pub seed: u64,
    pub radius: f32,
}

impl Default for RandomSettings {
    fn default() -> Self {
        Self {
            seed: 0xC0FFEE,
            radius: 200.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RandomLayout;

impl StaticLayout for RandomLayout {
    type Settings = RandomSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "random",
            kind: LayoutKind::Static,
            display_name: "Random (uniform ball)",
            description:
                "Uniform sample inside a ball of given radius — useful as a fresh seed before a physics layout.",
            requirements: LayoutRequirements {
                needs_edges: false,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
        // Match the engine's id-sorted node order so the returned packed
        // positions line up with the GPU positions buffer index-for-index.
        let mut node_order: Vec<&String> = graph.nodes.keys().collect();
        node_order.sort();

        let n = node_order.len();
        let mut out: Vec<f32> = Vec::with_capacity(n * 3);
        let radius = settings.radius.max(0.0);

        for (i, _id) in node_order.iter().enumerate() {
            // Stateless splitmix64 keyed on (seed, node_index, axis_attempt).
            // Use rejection sampling: draw points in [-1,1]^3 until we land
            // inside the unit ball, then scale by radius. Bound the loop so
            // a degenerate seed can't spin forever.
            let mut attempt: u64 = 0;
            let (x, y, z) = loop {
                let r1 = unit_signed(settings.seed, i as u64, attempt * 3);
                let r2 = unit_signed(settings.seed, i as u64, attempt * 3 + 1);
                let r3 = unit_signed(settings.seed, i as u64, attempt * 3 + 2);
                if r1 * r1 + r2 * r2 + r3 * r3 <= 1.0 {
                    break (r1, r2, r3);
                }
                attempt += 1;
                if attempt > 32 {
                    // Fallback: project onto the unit sphere via normalisation
                    // so we always terminate. Vanishingly rare with splitmix.
                    let len = (r1 * r1 + r2 * r2 + r3 * r3).sqrt().max(1e-6);
                    break (r1 / len, r2 / len, r3 / len);
                }
            };
            out.push(x * radius);
            out.push(y * radius);
            out.push(z * radius);
        }

        Ok(out)
    }
}

/// Stateless hash → f32 in [-1, 1]. Splitmix64 over the (seed, idx, k)
/// tuple, then mapped to a signed unit interval.
fn unit_signed(seed: u64, idx: u64, k: u64) -> f32 {
    let mut z = seed
        .wrapping_add(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(k.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Top 24 bits → unit float in [0,1), then map to [-1, 1).
    let u = ((z >> 40) as f32) / ((1u32 << 24) as f32);
    u * 2.0 - 1.0
}
