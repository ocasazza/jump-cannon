use bevy::prelude::*;
use crate::components::{MainCamera, Node, Edge, GraphTitleText}; // Added GraphTitleText
use crate::config::GraphConfig;
use rand::Rng;
#[cfg(target_arch = "wasm32")]
use web_sys::console; // For logging

/// Constant for the width of the display area, also used for coordinate range
pub const X_EXTENT: f32 = 900.0;
/// Constant for the height of the display area, assuming a similar range for Y
pub const Y_EXTENT: f32 = 600.0;

// Removed const NUM_NODES and NUM_EDGES, will use GraphConfig resource

/// Setup function to initialize the scene with a graph (based on GraphConfig) and camera
pub fn setup(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    graph_config: Res<GraphConfig>,
    camera_query: Query<Entity, With<MainCamera>>, // Query for existing main camera
) {
    #[cfg(target_arch = "wasm32")]
    console::log_1(&format!("[BEVY] setup::graph::setup called with config: {:?}", *graph_config).into());

    // Add a 2D camera only if one doesn't already exist
    if camera_query.is_empty() {
        commands.spawn((
            Camera2d,
            MainCamera::default(),
        ));
        #[cfg(target_arch = "wasm32")]
        console::log_1(&"[BEVY] MainCamera spawned in setup.".into());
    } else {
        #[cfg(target_arch = "wasm32")]
        console::log_1(&"[BEVY] MainCamera already exists, not spawning in setup.".into());
    }

    let font = asset_server.load("fonts/FiraSans-Bold.ttf");
    let mut rng = rand::thread_rng();
    let mut node_entities = Vec::with_capacity(graph_config.num_nodes);

    // Create nodes at random positions based on GraphConfig
    for _ in 0..graph_config.num_nodes {
        let x = rng.gen_range(-X_EXTENT / 2.0..X_EXTENT / 2.0);
        let y = rng.gen_range(-Y_EXTENT / 2.0..Y_EXTENT / 2.0);
        let node_entity = commands.spawn(Node { x, y }).id();
        node_entities.push(node_entity);
    }

    // Create edges between random existing nodes based on GraphConfig
    if graph_config.num_nodes > 1 {
        for _ in 0..graph_config.num_edges {
            let from_index = rng.gen_range(0..graph_config.num_nodes); // Removed mut
            let mut to_index = rng.gen_range(0..graph_config.num_nodes);
            while from_index == to_index { // Ensure 'from' and 'to' are different nodes
                to_index = rng.gen_range(0..graph_config.num_nodes);
            }

            let from_entity = node_entities[from_index];
            let to_entity = node_entities[to_index];
            commands.spawn(Edge { from: from_entity, to: to_entity });
        }
    }

    // Add title text displaying current config
    commands.spawn((
        Text2d::new(format!("Graph: {} Nodes, {} Edges", graph_config.num_nodes, graph_config.num_edges)),
        TextFont {
            font,
            font_size: 30.0,
            ..default()
        },
        TextLayout::new_with_justify(JustifyText::Left),
        Transform::from_xyz(-X_EXTENT / 2. + 20.0, Y_EXTENT / 2. - 40.0, 0.0), // Adjusted Y for title
        GraphTitleText, // Added marker
    ));
}
