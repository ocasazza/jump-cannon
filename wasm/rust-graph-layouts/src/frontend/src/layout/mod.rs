use shared::types::{Graph, LayoutAlgorithm};
use shared::layout::{apply_layout as shared_apply_layout};

#[cfg(test)]
mod tests;

/// Layout module for the graph visualization application
/// This module is responsible for applying layout algorithms to graphs
/// 
/// This is a thin wrapper around the shared library's layout functionality
/// All actual layout algorithms are implemented in the shared library

/// Apply a layout algorithm to a graph
pub fn apply_layout(graph: &mut Graph, layout: &LayoutAlgorithm) -> Result<(), String> {
    // Delegate to the shared library implementation
    shared_apply_layout(graph, layout)
}

/// Trait for layout engines
pub trait LayoutEngine {
    /// Get the name of the layout algorithm
    fn name(&self) -> &'static str;
    
    /// Get a description of the layout algorithm
    fn description(&self) -> &'static str;
    
    /// Apply the layout algorithm to a graph
    fn apply(&self, graph: &mut Graph) -> Result<(), String>;
    
    /// Get the default options for the layout algorithm
    fn default_options(&self) -> LayoutAlgorithm;
}

/// fCoSE layout engine
pub struct FcoseLayout;

impl LayoutEngine for FcoseLayout {
    fn name(&self) -> &'static str {
        "Force-Directed (fCoSE)"
    }
    
    fn description(&self) -> &'static str {
        "Force-directed layout algorithm optimized for compound graphs"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::Fcose(shared::types::FcoseLayoutOptions::default())
    }
}

/// CoSE Bilkent layout engine
pub struct CoseBilkentLayout;

impl LayoutEngine for CoseBilkentLayout {
    fn name(&self) -> &'static str {
        "CoSE Bilkent"
    }
    
    fn description(&self) -> &'static str {
        "Compound Spring Embedder layout algorithm from Bilkent University"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::CoseBilkent(shared::types::CoseBilkentLayoutOptions::default())
    }
}

/// CiSE layout engine
pub struct CiseLayout;

impl LayoutEngine for CiseLayout {
    fn name(&self) -> &'static str {
        "CiSE"
    }
    
    fn description(&self) -> &'static str {
        "Circular Spring Embedder layout algorithm"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::Cise(shared::types::CiseLayoutOptions::default())
    }
}

/// Concentric layout engine
pub struct ConcentricLayout;

impl LayoutEngine for ConcentricLayout {
    fn name(&self) -> &'static str {
        "Concentric"
    }
    
    fn description(&self) -> &'static str {
        "Concentric layout algorithm that positions nodes in concentric circles"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::Concentric(shared::types::ConcentricLayoutOptions::default())
    }
}

/// KLay Layered layout engine
pub struct KlayLayeredLayout;

impl LayoutEngine for KlayLayeredLayout {
    fn name(&self) -> &'static str {
        "KLay Layered"
    }
    
    fn description(&self) -> &'static str {
        "Layer-based layout algorithm optimized for directed graphs"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::KlayLayered(shared::types::KlayLayeredLayoutOptions::default())
    }
}

/// Dagre layout engine
pub struct DagreLayout;

impl LayoutEngine for DagreLayout {
    fn name(&self) -> &'static str {
        "Dagre"
    }
    
    fn description(&self) -> &'static str {
        "Directed graph layout algorithm optimized for hierarchical visualizations"
    }
    
    fn apply(&self, graph: &mut Graph) -> Result<(), String> {
        apply_layout(graph, &self.default_options())
    }
    
    fn default_options(&self) -> LayoutAlgorithm {
        LayoutAlgorithm::Dagre(shared::types::DagreLayoutOptions::default())
    }
}

/// Get all available layout engines
pub fn get_layout_engines() -> Vec<Box<dyn LayoutEngine>> {
    vec![
        Box::new(FcoseLayout),
        Box::new(CoseBilkentLayout),
        Box::new(CiseLayout),
        Box::new(ConcentricLayout),
        Box::new(KlayLayeredLayout),
        Box::new(DagreLayout)
    ]
}
