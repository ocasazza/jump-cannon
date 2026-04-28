use bevy::prelude::*;
// Intentionally not importing Mesh2d or MeshMaterial2d directly
// Sprite is still brought in by prelude

use crate::components::{Node, Edge};

// A marker component for our edge entities, to help with cleanup if needed
#[derive(Component)]
pub struct RenderedEdge;

// A marker component for rendered node visuals
#[derive(Component)]
pub struct RenderedNode;

pub fn graph_rendering_system(
    mut commands: Commands,
    query_nodes: Query<(Entity, &Node, Option<&Children>)>,
    query_edges: Query<&Edge>, // Querying for Edge components
    all_nodes: Query<&Node>,  // To get positions of nodes for edges
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    // Query to find previously rendered edges for potential cleanup/update
    // For simplicity, we'll despawn and respawn edges each frame.
    // A more optimized approach would update existing edge entities.
    rendered_edge_query: Query<Entity, With<RenderedEdge>>,
) {
    // Despawn old edges first to prevent accumulation
    for entity in rendered_edge_query.iter() {
        commands.entity(entity).despawn();
    }

    // Remove the unused rendered_nodes variable since we're always updating node visuals
    
    // Render nodes - always ensure they have the correct visual components
    for (entity, node, _children) in query_nodes.iter() {
        commands.entity(entity).insert((
            bevy::sprite::Sprite {
                color: Color::srgb(0.8, 0.8, 0.8),
                custom_size: Some(Vec2::new(20.0, 20.0)),
                ..default()
            },
            Transform::from_translation(Vec3::new(node.x, node.y, 0.0)),
            Visibility::Visible,
            RenderedNode, // Add marker component
        ));
    }

    // Render edges as thin rectangles
    let edge_color = Color::srgb(0.5, 0.5, 0.5);
    let edge_thickness = 2.0;

    for edge_component in query_edges.iter() {
        if let (Ok(from_node), Ok(to_node)) = (
            all_nodes.get(edge_component.from),
            all_nodes.get(edge_component.to),
        ) {
            let start_point = Vec2::new(from_node.x, from_node.y);
            let end_point = Vec2::new(to_node.x, to_node.y);

            let midpoint = (start_point + end_point) / 2.0;
            let diff = end_point - start_point;
            let length = diff.length();
            let angle = diff.y.atan2(diff.x);

            let mesh_handle = meshes.add(Rectangle::new(length, edge_thickness));
            commands.spawn((
                bevy::prelude::Mesh2d(mesh_handle), // Fully qualified path
                bevy::sprite::MeshMaterial2d(materials.add(edge_color)), // Fully qualified path
                Transform::from_translation(midpoint.extend(0.0))
                    .with_rotation(Quat::from_rotation_z(angle)),
                RenderedEdge, // Add marker component
            ));
        }
    }
}
