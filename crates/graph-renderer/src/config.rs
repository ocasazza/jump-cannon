use bevy::prelude::*;

#[derive(Resource, Debug, Clone, Copy)]
pub struct GraphConfig {
    pub num_nodes: usize,
    pub num_edges: usize,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            num_nodes: 10, // Default number of nodes
            num_edges: 15, // Default number of edges
        }
    }
}

#[derive(Event, Debug)]
pub struct RegenerateGraphEvent;
