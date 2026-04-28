use bevy::prelude::*;
use crate::graph::interaction::SelectionState;
use crate::state::ui::UiState;
use crate::state::persistence::{load_view_state, save_view_state, ViewState};

/// Restore saved view state into Bevy resources on startup (after vault loads).
pub fn restore_view_state(
    mut ui_state: ResMut<UiState>,
    mut selection: ResMut<SelectionState>,
) {
    let saved = load_view_state();
    ui_state.search_query = saved.search_query;
    ui_state.focus_mode = saved.focus_mode;
    ui_state.sidebar_open = saved.sidebar_open;
    ui_state.sidebar_width = saved.sidebar_width;
    selection.selected_node = saved.selected_node;
}

/// Save view state on app exit.
pub fn save_view_on_exit(
    mut exit_events: EventReader<AppExit>,
    ui_state: Res<UiState>,
    selection: Res<SelectionState>,
    camera_q: Query<&Transform, With<Camera2d>>,
) {
    for _ in exit_events.read() {
        let (cam_x, cam_y, cam_zoom) = camera_q.single()
            .map(|t| (t.translation.x, t.translation.y, t.scale.x))
            .unwrap_or((0.0, 0.0, 1.0));

        let state = ViewState {
            camera_x: cam_x,
            camera_y: cam_y,
            camera_zoom: cam_zoom,
            selected_node: selection.selected_node.clone(),
            search_query: ui_state.search_query.clone(),
            focus_mode: ui_state.focus_mode,
            sidebar_open: ui_state.sidebar_open,
            sidebar_width: ui_state.sidebar_width,
        };
        save_view_state(&state);
    }
}
