use bevy::prelude::*;
use crate::graph::simulation::GraphNode;
use crate::vault::VaultGraphResource;

#[derive(Resource, Default)]
pub struct HoverState {
    pub hovered_node: Option<String>,
}

#[derive(Resource, Default)]
pub struct SelectionState {
    pub selected_node: Option<String>,
}

/// Detects which node (if any) the mouse cursor is over.
pub fn hover_system(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    nodes: Query<(&GraphNode, &Transform, &Sprite)>,
    mut hover: ResMut<HoverState>,
) {
    let Ok(window) = windows.single() else { return };
    let Ok((camera, cam_transform)) = camera_q.single() else { return };

    let Some(cursor_pos) = window.cursor_position() else {
        hover.hovered_node = None;
        return;
    };

    let Ok(world_pos) = camera.viewport_to_world_2d(cam_transform, cursor_pos) else {
        hover.hovered_node = None;
        return;
    };

    let mut closest: Option<(String, f32)> = None;
    for (node, transform, sprite) in nodes.iter() {
        let node_pos = transform.translation.truncate();
        let half = sprite.custom_size.unwrap_or(Vec2::splat(8.0)) / 2.0;
        let dist = (world_pos - node_pos).length();
        // Hit test: within the sprite's radius
        if dist < half.x.max(half.y) + 4.0 {
            if closest.as_ref().map_or(true, |(_, d)| dist < *d) {
                closest = Some((node.id.clone(), dist));
            }
        }
    }

    hover.hovered_node = closest.map(|(id, _)| id);
}

/// Left-click selects a node; clicking empty space deselects.
pub fn click_select_system(
    mouse: Res<ButtonInput<MouseButton>>,
    hover: Res<HoverState>,
    mut selection: ResMut<SelectionState>,
    mut ui: ResMut<crate::state::ui::UiState>,
) {
    if mouse.just_pressed(MouseButton::Left) {
        selection.selected_node = hover.hovered_node.clone();
        if selection.selected_node.is_some() {
            ui.modal_open = true;
        } else {
            ui.modal_open = false;
        }
    }
}

/// Hide non-matching nodes when focus mode is active.
pub fn focus_mode_system(
    ui_state: Res<crate::state::ui::UiState>,
    mut nodes: Query<(&GraphNode, &mut Visibility)>,
) {
    let should_filter = ui_state.focus_mode && !ui_state.search_query.is_empty();
    for (node, mut vis) in nodes.iter_mut() {
        *vis = if should_filter {
            if ui_state.search_results.contains(&node.id) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            }
        } else {
            Visibility::Inherited
        };
    }
}

/// Tint hovered nodes slightly brighter.
pub fn highlight_hover_system(
    hover: Res<HoverState>,
    selection: Res<SelectionState>,
    vault: Res<VaultGraphResource>,
    mut nodes: Query<(&GraphNode, &mut Sprite)>,
) {
    for (node, mut sprite) in nodes.iter_mut() {
        let community = vault.graph.nodes.get(&node.id)
            .map(|n| n.metrics.community)
            .unwrap_or(0);
        let [r, g, b] = vault_data::color::community_color(community);

        sprite.color = if selection.selected_node.as_deref() == Some(&node.id) {
            Color::srgb(1.0, 0.9, 0.2) // gold for selected
        } else if hover.hovered_node.as_deref() == Some(&node.id) {
            Color::srgb((r + 0.3).min(1.0), (g + 0.3).min(1.0), (b + 0.3).min(1.0))
        } else {
            Color::srgb(r, g, b)
        };
    }
}
