#![allow(dead_code)]
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

/// Trait for force-directed layout algorithms
pub trait ForceDirectedLayout: LayoutEngine {
    /// Calculate repulsive forces between all pairs of nodes
    fn calculate_repulsion(&self, graph: &Graph) -> Vec<(f64, f64)>;

    /// Calculate attractive forces along edges
    fn calculate_attraction(&self, graph: &Graph) -> Vec<(f64, f64)>;

    /// Apply forces to update node positions
    fn apply_forces(&self, graph: &mut Graph, forces: &[(f64, f64)]) -> Result<(), String>;
}

/// Trait for circular layout algorithms
pub trait CircularLayout: LayoutEngine {
    /// Arrange nodes in a circle with the given radius
    fn arrange_circle(&self, graph: &mut Graph, radius: f64) -> Result<(), String>;

    /// Optimize node ordering to minimize edge crossings
    fn optimize_ordering(&self, graph: &mut Graph) -> Result<(), String>;
}

/// Trait for hierarchical/concentric layout algorithms
pub trait HierarchicalLayout: LayoutEngine {
    /// Assign nodes to levels/concentric rings
    fn assign_levels(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String>;

    /// Position nodes based on the level assignment
    fn position_nodes(&self, graph: &mut Graph, levels: &[Vec<String>]) -> Result<(), String>;
}

/// Trait for layered/DAG layout algorithms
pub trait LayeredLayout: LayoutEngine {
    /// Assign nodes to layers
    fn assign_layers(&self, graph: &Graph) -> Result<Vec<Vec<String>>, String>;

    /// Break cycles in the graph to enable layered layout
    fn break_cycles(&self, graph: &mut Graph, layers: &mut Vec<Vec<String>>) -> Result<(), String>;

    /// Minimize edge crossings between adjacent layers
    fn minimize_crossings(&self, layers: &mut Vec<Vec<String>>, graph: &Graph) -> Result<(), String>;

    /// Count edge crossings between two adjacent layers
    fn count_crossings(&self, layer1: &[String], layer2: &[String], graph: &Graph) -> usize;
}