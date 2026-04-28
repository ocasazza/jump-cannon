use bevy::prelude::*;
use crate::components::{Node, GraphTitleText, MainCamera}; // Ensure MainCamera is imported
use crate::config::{GraphConfig, RegenerateGraphEvent};
use crate::setup::graph::setup as regenerate_graph_setup; // Alias for clarity
use crate::systems::graph_rendering::{RenderedEdge, RenderedNode};
#[cfg(target_arch = "wasm32")]
use web_sys::console; // For logging

pub fn handle_regeneration_event_system(
    mut commands: Commands,
    mut event_reader: EventReader<RegenerateGraphEvent>,
    node_query: Query<Entity, With<Node>>,
    edge_query: Query<Entity, With<RenderedEdge>>,
    rendered_node_query: Query<Entity, With<RenderedNode>>,
    title_query: Query<Entity, With<GraphTitleText>>,
    asset_server: Res<AssetServer>,
    graph_config: Res<GraphConfig>,
    camera_query: Query<Entity, With<MainCamera>>, // Parameter using MainCamera
) {
    if event_reader.read().next().is_some() {
        #[cfg(target_arch = "wasm32")]
        console::log_1(&"[BEVY] handle_regeneration_event_system: Event received. Despawning and regenerating graph.".into());

        // Despawn existing graph elements
        for entity in node_query.iter() {
            commands.entity(entity).despawn();
        }
        for entity in edge_query.iter() {
            commands.entity(entity).despawn();
        }
        for entity in rendered_node_query.iter() {
            commands.entity(entity).despawn();
        }
        for entity in title_query.iter() {
            commands.entity(entity).despawn();
        }

        regenerate_graph_setup(commands, asset_server, graph_config, camera_query);
    }
}
