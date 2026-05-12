use crate::types::{Graph, LayoutAlgorithm};

pub mod traits;
pub mod algorithms;
pub mod coarsen;
pub mod layout_trait;
pub mod topo_fisheye;

#[allow(unused_imports)]
pub use traits::*;
#[allow(unused_imports)]
pub use algorithms::*;

/// Apply a layout algorithm to a graph
#[allow(dead_code)]
pub fn apply_layout(graph: &mut Graph, layout: &LayoutAlgorithm) -> Result<(), String> {
    match layout {
        LayoutAlgorithm::Fcose(options) => algorithms::fcose::apply_layout(graph, options),
        LayoutAlgorithm::CoseBilkent(options) => algorithms::cose_bilkent::apply_layout(graph, options),
        LayoutAlgorithm::Cise(options) => algorithms::cise::apply_layout(graph, options),
        LayoutAlgorithm::Concentric(options) => algorithms::concentric::apply_layout(graph, options),
        LayoutAlgorithm::KlayLayered(options) => algorithms::klay::apply_layout(graph, options),
        LayoutAlgorithm::Dagre(options) => algorithms::dagre::apply_layout(graph, options),
    }
}
