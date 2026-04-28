use bevy::prelude::*;
use super::simulation::{GraphEdge, GraphNode};

/// Marker component for rendered edge mesh entities so we can despawn them on
/// re-render without touching node sprites.
#[derive(Component)]
pub struct RenderedEdge;

/// Render edges as thin Mesh2d rectangles (midpoint + atan2 rotation).
/// Also ensures node sprites reflect the correct transform.
/// Despawns all old `RenderedEdge` entities first to keep the world clean.
pub fn draw_edges(
    mut commands: Commands,
    edges: Query<&GraphEdge>,
    nodes: Query<(&GraphNode, &Transform)>,
    rendered_edges: Query<Entity, With<RenderedEdge>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    // Despawn previously rendered edges
    for entity in rendered_edges.iter() {
        commands.entity(entity).despawn();
    }

    // Build id → position map
    let positions: std::collections::HashMap<&str, Vec2> = nodes
        .iter()
        .map(|(n, t)| (n.id.as_str(), t.translation.truncate()))
        .collect();

    let edge_color = Color::srgba(0.5, 0.5, 0.5, 0.4);
    let edge_thickness = 1.5_f32;

    for edge in edges.iter() {
        if let (Some(&src_pos), Some(&tgt_pos)) = (
            positions.get(edge.source.as_str()),
            positions.get(edge.target.as_str()),
        ) {
            let diff = tgt_pos - src_pos;
            let length = diff.length();
            if length < 0.5 { continue; } // skip degenerate edges

            let midpoint = (src_pos + tgt_pos) / 2.0;
            let angle = diff.y.atan2(diff.x);

            let mesh_handle = meshes.add(Rectangle::new(length, edge_thickness));
            commands.spawn((
                Mesh2d(mesh_handle),
                MeshMaterial2d(materials.add(edge_color)),
                Transform::from_translation(midpoint.extend(-1.0))
                    .with_rotation(Quat::from_rotation_z(angle)),
                RenderedEdge,
            ));
        }
    }
}
