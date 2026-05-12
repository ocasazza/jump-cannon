//! Stream-only types for the §5–§6 viewing technique. The algorithm-level
//! types (`MatchWeights`, `CoarsenParams`, `Level`, `TopoHierarchy`) live in
//! [`graph_layouts::layout::topo_fisheye`] and are re-exported by this
//! module's parent so call sites don't need two import paths.

/// A hybrid graph (paper §5): each visible node references a specific
/// `(level, node-index-at-that-level)` in a [`TopoHierarchy`], and edges
/// connect those references.
#[derive(Clone, Debug)]
pub struct HybridGraph {
    /// One entry per visible node, `(level, idx_in_level)`.
    pub nodes: Vec<(u32, u32)>,
    /// Interleaved x,y,z positions for `nodes`, length `3 * nodes.len()`.
    pub positions: Vec<f32>,
    /// Edges between hybrid-node indices (indices into `nodes`).
    pub edges: Vec<u32>,
    /// Per-edge level (max of endpoint levels — handy for the paper's
    /// red-to-green colour gradient).
    pub edge_levels: Vec<u32>,
    /// Per-node display level (mirrors `nodes[i].0`). Broken out so the
    /// renderer can upload it as a colour attribute without re-slicing.
    pub node_levels: Vec<u32>,
}

// `TopoHierarchy` re-export is in `topo_fisheye::mod` so consumers can write
// `use crate::topo_fisheye::{TopoHierarchy, HybridGraph};` and get both.
