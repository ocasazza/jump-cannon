//! Sphere static layout — Fibonacci spiral on a sphere.
//!
//! One-shot CPU solver. Distributes nodes near-uniformly on a sphere
//! using the golden-angle azimuth + equal-area latitude trick. O(n),
//! single allocation for the output buffer, no per-node heap traffic.

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SphereSettings {
    pub radius: f32,
    /// 0..=1 perturbation along the surface normal (radius scale).
    pub jitter: f32,
    pub seed: u64,
}

impl Default for SphereSettings {
    fn default() -> Self {
        Self {
            radius: 200.0,
            jitter: 0.0,
            seed: 0xC0FFEE,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SphereLayout;

impl StaticLayout for SphereLayout {
    type Settings = SphereSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "sphere",
            kind: LayoutKind::Static,
            display_name: "Sphere (Fibonacci)",
            description:
                "Distribute nodes near-uniformly on a sphere via the Fibonacci spiral (golden-angle azimuth + equal-area latitude).",
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
        let jitter = settings.jitter.clamp(0.0, 1.0);
        let denom = n.max(1) as f32;

        // Golden ratio φ = (1 + √5) / 2; golden angle in radians = TAU / φ.
        const GOLDEN_RATIO: f32 = 1.618_034;
        let golden_step = std::f32::consts::TAU / GOLDEN_RATIO;

        for i in 0..n {
            let fi = i as f32;
            // Equal-area latitude: cos(phi) = 1 - 2*(i + 0.5)/n in [-1, 1].
            let cos_phi = 1.0 - 2.0 * (fi + 0.5) / denom;
            let sin_phi = (1.0 - cos_phi * cos_phi).max(0.0).sqrt();
            let theta = golden_step * fi;
            let (sin_t, cos_t) = theta.sin_cos();

            let mut r = radius;
            if jitter > 0.0 {
                let s = unit_signed(settings.seed, i as u64, 0);
                r += jitter * radius * s;
            }

            out.push(r * sin_phi * cos_t);
            out.push(r * sin_phi * sin_t);
            out.push(r * cos_phi);
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
    let u = ((z >> 40) as f32) / ((1u32 << 24) as f32);
    u * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Graph, Node};

    #[test]
    fn smoke_one_million_nodes() {
        let mut graph = Graph::new();
        let n: usize = 1_000_000;
        for i in 0..n {
            graph.add_node(Node::new(format!("{i:07}")));
        }
        let settings = SphereSettings::default();
        let out = SphereLayout::solve(&settings, &graph).expect("solve");
        assert_eq!(out.len(), 3 * n);
    }

    #[test]
    fn points_lie_on_sphere() {
        let mut graph = Graph::new();
        for i in 0..1024 {
            graph.add_node(Node::new(format!("{i:04}")));
        }
        let settings = SphereSettings {
            radius: 100.0,
            jitter: 0.0,
            seed: 1,
        };
        let out = SphereLayout::solve(&settings, &graph).expect("solve");
        for chunk in out.chunks_exact(3) {
            let r = (chunk[0] * chunk[0] + chunk[1] * chunk[1] + chunk[2] * chunk[2]).sqrt();
            assert!((r - 100.0).abs() < 1e-2, "r={r}");
        }
    }
}
