use crate::types::Graph;

/// Common trait for all layout algorithms
pub trait LayoutEngine {
    /// Apply the layout algorithm to a graph
    fn apply_layout(&self, graph: &mut Graph) -> Result<(), String>;
    
    /// Get the name of the layout algorithm
    fn name(&self) -> &'static str;
    
    /// Get a description of the layout algorithm
    fn description(&self) -> &'static str;
}

/// Trait for algorithms that work with layers
pub trait LayeredLayout {
    /// Assign nodes to layers
    fn assign_layers(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String>;
    
    /// Break cycles in the graph by reversing edges
    fn break_cycles(&self, graph: &mut Graph, layers: &mut Vec<Vec<String>>) -> Result<(), String>;
    
    /// Minimize edge crossings between layers
    fn minimize_crossings(&self, layers: &mut Vec<Vec<String>>, graph: &Graph) -> Result<(), String>;
    
    /// Count edge crossings between two layers
    fn count_crossings(&self, layer1: &[String], layer2: &[String], graph: &Graph) -> usize;
}

/// Trait for algorithms that work with force-directed layouts
pub trait ForceDirectedLayout {
    /// Calculate repulsive forces between nodes
    fn calculate_repulsion(&self, graph: &Graph) -> Vec<(f64, f64)>;
    
    /// Calculate attractive forces along edges
    fn calculate_attraction(&self, graph: &Graph) -> Vec<(f64, f64)>;
    
    /// Apply forces to update node positions
    fn apply_forces(&self, graph: &mut Graph, forces: &[(f64, f64)]) -> Result<(), String>;
}

/// Trait for algorithms that work with circular layouts
pub trait CircularLayout {
    /// Arrange nodes in a circle
    fn arrange_circle(&self, graph: &mut Graph, radius: f64) -> Result<(), String>;
    
    /// Optimize node ordering to minimize edge crossings
    fn optimize_ordering(&self, graph: &mut Graph) -> Result<(), String>;
}

/// Trait for algorithms that work with hierarchical layouts
pub trait HierarchicalLayout {
    /// Assign nodes to hierarchy levels
    fn assign_levels(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String>;
    
    /// Position nodes within their levels
    fn position_nodes(&self, graph: &mut Graph, levels: &[Vec<String>]) -> Result<(), String>;
}
