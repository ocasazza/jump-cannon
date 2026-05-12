//! Topological-fisheye **view** (Gansner, Koren, North — IEEE InfoVis 2004),
//! the interactive §5–§6 half. The §4 multilevel coarsening + hierarchy
//! builder lives in [`graph_layouts::layout::topo_fisheye`] so the
//! `SeedMode::TopoFisheye` seeder for the force-directed sim and this
//! viewing-technique RPC share a single source of truth.
//!
//! Two stages here:
//!
//!   1. [`hybrid`] — pick a focal node, BFS outward on the finest graph,
//!      and assign each visible node a level in the hierarchy based on
//!      its graph-theoretic distance to the focus. The result is a single
//!      "hybrid graph" that is fine near the focus and coarse at the
//!      periphery (paper §5).
//!
//!   2. [`distort`] — radially re-scale the hybrid layout around the
//!      focal node so the coarse peripheral region gets a fair share of
//!      display space (paper §6).
//!
//! Hierarchy types are re-exported from graph-layouts for convenience.

pub mod distort;
pub mod hybrid;
pub mod types;

pub use distort::{distort_radial, DistortParams};
pub use hybrid::{build_hybrid, HybridParams};
pub use types::HybridGraph;

// Re-export the §4 algorithm types from their canonical home in graph-layouts
// so call sites within graph-compute (and external consumers of this crate)
// don't need a second import path.
pub use graph_layouts::topo_fisheye::{
    build_hierarchy, seed_positions, CoarsenParams, Level, MatchWeights, TopoHierarchy,
};
