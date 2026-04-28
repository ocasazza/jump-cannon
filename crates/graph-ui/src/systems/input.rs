use bevy::prelude::*;
use crate::state::ui::UiState;

pub fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<UiState>,
) {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if ctrl && keys.just_pressed(KeyCode::KeyP) {
        state.palette_open = !state.palette_open;
    }
}
