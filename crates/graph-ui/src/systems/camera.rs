use bevy::prelude::*;
use bevy::input::mouse::{MouseMotion, MouseWheel};

/// Spawn the 2D camera.
pub fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

/// WASD pan (500 units/sec) + Q/E zoom.
pub fn keyboard_camera_system(
    time: Res<Time>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<&mut Transform, With<Camera2d>>,
) {
    let Ok(mut transform) = query.single_mut() else { return };

    let speed = 500.0_f32;
    let mut direction = Vec3::ZERO;

    if keyboard_input.pressed(KeyCode::KeyW) { direction.y += 1.0; }
    if keyboard_input.pressed(KeyCode::KeyS) { direction.y -= 1.0; }
    if keyboard_input.pressed(KeyCode::KeyA) { direction.x -= 1.0; }
    if keyboard_input.pressed(KeyCode::KeyD) { direction.x += 1.0; }

    if keyboard_input.pressed(KeyCode::KeyQ) {
        transform.scale += Vec3::splat(0.1);
    }
    if keyboard_input.pressed(KeyCode::KeyE) {
        transform.scale -= Vec3::splat(0.1);
        transform.scale = transform.scale.max(Vec3::splat(0.1));
    }

    if direction != Vec3::ZERO {
        transform.translation += direction.normalize() * speed * time.delta_secs();
    }
}

/// Middle-mouse / modifier+left-click drag pan.
pub fn mouse_pan_system(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion_events: EventReader<MouseMotion>,
    mut query: Query<&mut Transform, With<Camera2d>>,
) {
    let modifier_held = keyboard_input.pressed(KeyCode::ControlLeft)
        || keyboard_input.pressed(KeyCode::ControlRight)
        || keyboard_input.pressed(KeyCode::ShiftLeft)
        || keyboard_input.pressed(KeyCode::ShiftRight)
        || keyboard_input.pressed(KeyCode::AltLeft)
        || keyboard_input.pressed(KeyCode::AltRight);

    let should_pan = mouse_button.pressed(MouseButton::Middle)
        || (mouse_button.pressed(MouseButton::Left) && modifier_held);

    if should_pan {
        let mut pan = Vec2::ZERO;
        for event in mouse_motion_events.read() {
            pan -= event.delta;
        }
        if pan != Vec2::ZERO {
            if let Ok(mut transform) = query.single_mut() {
                let scale_factor = 1.0 / transform.scale.x;
                transform.translation.x += pan.x * scale_factor;
                transform.translation.y += pan.y * scale_factor;
            }
        }
    } else {
        mouse_motion_events.clear();
    }
}

/// Scroll-wheel zoom, clamped to [0.1, 5.0].
pub fn mouse_zoom_system(
    mut mouse_wheel_events: EventReader<MouseWheel>,
    mut query: Query<&mut Transform, With<Camera2d>>,
) {
    let mut scroll = 0.0_f32;
    for event in mouse_wheel_events.read() {
        scroll += event.y;
    }

    if scroll != 0.0 {
        if let Ok(mut transform) = query.single_mut() {
            let zoom_factor = 1.0 - scroll * 0.1;
            transform.scale *= Vec3::splat(zoom_factor);
            transform.scale = transform.scale.clamp(Vec3::splat(0.1), Vec3::splat(5.0));
        }
    }
}
