//! Grid static layout — packs nodes into a regular 3D grid.
//!
//! One-shot CPU solver. O(n), single linear pass, no per-node allocation
//! beyond the id-sort + the output `Vec<f32>`. Designed to handle
//! ~1_000_000 nodes without blocking the UI for long.

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{
    LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout,
};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GridSettings {
    /// Distance between adjacent grid cells.
    pub spacing: f32,
    /// `x / y` aspect ratio of the 2D footprint per layer.
    pub aspect: f32,
    /// Number of z-layers; 1 = flat 2D grid.
    pub layers: u32,
    /// If true, recenter the grid around the origin.
    pub center: bool,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            spacing: 50.0,
            aspect: 1.0,
            layers: 1,
            center: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GridLayout;

impl StaticLayout for GridLayout {
    type Settings = GridSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "grid",
            kind: LayoutKind::Static,
            display_name: "Grid",
            description:
                "Pack nodes into a regular 3D grid. O(n), allocation-light — safe for very large graphs.",
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

        if n == 0 {
            return Ok(out);
        }

        let spacing = settings.spacing.max(0.0);
        let aspect = settings.aspect.max(0.0001);
        let layers = settings.layers.max(1);
        let per_layer = ((n as u32 + layers - 1) / layers).max(1) as f32;

        // cols ≈ sqrt(per_layer * aspect), at least 1.
        let cols = (per_layer * aspect).sqrt().ceil().max(1.0) as u32;
        let rows_per_layer = ((per_layer as u32 + cols - 1) / cols).max(1);
        let cells_per_layer = cols * rows_per_layer;

        // Centering offsets (subtract half-extent so footprint is centered).
        let (ox, oy, oz) = if settings.center {
            (
                (cols.saturating_sub(1) as f32) * 0.5 * spacing,
                (rows_per_layer.saturating_sub(1) as f32) * 0.5 * spacing,
                (layers.saturating_sub(1) as f32) * 0.5 * spacing,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

        for i in 0..n {
            let idx = i as u32;
            let layer = idx / cells_per_layer;
            let in_layer = idx % cells_per_layer;
            let row = in_layer / cols;
            let col = in_layer % cols;

            let x = (col as f32) * spacing - ox;
            let y = (row as f32) * spacing - oy;
            let z = (layer as f32) * spacing - oz;

            out.push(x);
            out.push(y);
            out.push(z);
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Graph, Node};
    use std::time::Instant;

    #[test]
    fn solve_handles_one_million_nodes() {
        let mut graph = Graph::new();
        let n = 1_000_000usize;
        for i in 0..n {
            graph.add_node(Node::new(format!("n{:08}", i)));
        }

        let settings = GridSettings::default();
        let start = Instant::now();
        let out = GridLayout::solve(&settings, &graph).expect("solve");
        let elapsed = start.elapsed();

        assert_eq!(out.len(), n * 3, "packed positions length");
        // Smoke check: the first triple should be finite numbers.
        assert!(out[0].is_finite() && out[1].is_finite() && out[2].is_finite());
        eprintln!("grid solve(1M) took {:?}", elapsed);
    }

    #[test]
    fn solve_centers_when_requested() {
        let mut graph = Graph::new();
        for i in 0..4 {
            graph.add_node(Node::new(format!("n{}", i)));
        }
        let s = GridSettings {
            spacing: 10.0,
            aspect: 1.0,
            layers: 1,
            center: true,
        };
        let out = GridLayout::solve(&s, &graph).unwrap();
        // 4 nodes → 2x2 grid, centered → range [-5, 5] on x and y.
        let xs: Vec<f32> = (0..4).map(|i| out[i * 3]).collect();
        let ys: Vec<f32> = (0..4).map(|i| out[i * 3 + 1]).collect();
        let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((max_x + min_x).abs() < 1e-4, "x not centered");
        assert!((max_y + min_y).abs() < 1e-4, "y not centered");
    }

    #[test]
    fn solve_layers_use_z_axis() {
        let mut graph = Graph::new();
        for i in 0..8 {
            graph.add_node(Node::new(format!("n{}", i)));
        }
        let s = GridSettings {
            spacing: 10.0,
            aspect: 1.0,
            layers: 2,
            center: false,
        };
        let out = GridLayout::solve(&s, &graph).unwrap();
        // first 4 nodes on z=0, next 4 on z=10
        assert_eq!(out[2], 0.0);
        assert_eq!(out[3 * 4 + 2], 10.0);
    }
}
