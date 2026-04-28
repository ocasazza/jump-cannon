use bevy::prelude::*;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use crate::components::MainCamera;

/// System to handle camera movement with WASD keys
pub fn keyboard_input_system(
    time: Res<Time>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<(&mut Transform, &MainCamera)>,
) {
    if let Ok((mut transform, camera)) = query.single_mut() {
        let mut direction = Vec3::ZERO;
        
        if keyboard_input.pressed(KeyCode::KeyW) {
            direction.y += 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyS) {
            direction.y -= 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyA) {
            direction.x -= 1.0;
        }
        if keyboard_input.pressed(KeyCode::KeyD) {
            direction.x += 1.0;
        }
        // Zoom controls with Q and E
        if keyboard_input.pressed(KeyCode::KeyQ) {
            transform.scale += Vec3::splat(0.1);
        }
        if keyboard_input.pressed(KeyCode::KeyE) {
            transform.scale -= Vec3::splat(0.1);
            // Prevent zooming too far in
            transform.scale = transform.scale.max(Vec3::splat(0.1));
        }
        if direction != Vec3::ZERO {
            transform.translation += direction.normalize() * camera.speed * time.delta_secs();
        }
    }
}

/// System to handle camera panning with mouse drag
pub fn mouse_pan_system(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion_events: EventReader<MouseMotion>,
    mut query: Query<&mut Transform, With<MainCamera>>,
) {
    // Check if we should pan:
    // 1. Middle mouse button is pressed, OR
    // 2. Left mouse button is pressed AND a modifier key is pressed (Ctrl, Shift, or Alt)
    let should_pan = mouse_button.pressed(MouseButton::Middle) || 
                    (mouse_button.pressed(MouseButton::Left) && 
                     (keyboard_input.pressed(KeyCode::ControlLeft) || 
                      keyboard_input.pressed(KeyCode::ControlRight) ||
                      keyboard_input.pressed(KeyCode::ShiftLeft) ||
                      keyboard_input.pressed(KeyCode::ShiftRight) ||
                      keyboard_input.pressed(KeyCode::AltLeft) ||
                      keyboard_input.pressed(KeyCode::AltRight)));
    
    if should_pan {
        let mut pan = Vec2::ZERO;
        
        // Sum all mouse motion events
        for event in mouse_motion_events.read() {
            pan -= event.delta;
        }
        
        if pan != Vec2::ZERO {
            if let Ok(mut transform) = query.single_mut() {
                // Apply the pan (adjusted for scale)
                let scale_factor = 1.0 / transform.scale.x;
                transform.translation.x += pan.x * scale_factor;
                transform.translation.y += pan.y * scale_factor;
            }
        }
    } else {
        // Clear the event buffer when not panning
        mouse_motion_events.clear();
    }
}

/// System to handle camera panning with touch/trackpad
pub fn touch_pan_system(
    mouse_button: Res<ButtonInput<MouseButton>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion_events: EventReader<MouseMotion>,
    mut query: Query<&mut Transform, With<MainCamera>>,
) {
    // For trackpad users: pan with left mouse button only (no modifier keys)
    // This is separate from mouse_pan_system to avoid conflicts
    let no_modifiers = !keyboard_input.pressed(KeyCode::ControlLeft) && 
                      !keyboard_input.pressed(KeyCode::ControlRight) &&
                      !keyboard_input.pressed(KeyCode::ShiftLeft) &&
                      !keyboard_input.pressed(KeyCode::ShiftRight) &&
                      !keyboard_input.pressed(KeyCode::AltLeft) &&
                      !keyboard_input.pressed(KeyCode::AltRight);
                      
    let should_pan = mouse_button.pressed(MouseButton::Left) && no_modifiers;
    
    if should_pan {
        let mut pan = Vec2::ZERO;
        
        // Sum all mouse motion events
        for event in mouse_motion_events.read() {
            // Invert the Y direction for more natural trackpad panning
            // Keep X direction the same
            pan.x -= event.delta.x;
            pan.y += event.delta.y; // Note the + sign here instead of -
        }
        
        if pan != Vec2::ZERO {
            if let Ok(mut transform) = query.single_mut() {
                // Apply the pan (adjusted for scale)
                let scale_factor = 1.0 / transform.scale.x;
                transform.translation.x += pan.x * scale_factor;
                transform.translation.y += pan.y * scale_factor;
            }
        }
    }
}

/// System to handle camera zooming with mouse wheel
pub fn mouse_zoom_system(
    mut mouse_wheel_events: EventReader<MouseWheel>,
    mut query: Query<&mut Transform, With<MainCamera>>,
) {
    let mut scroll = 0.0;
    
    // Sum all mouse wheel events
    for event in mouse_wheel_events.read() {
        scroll += event.y;
    }
    
    if scroll != 0.0 {
        if let Ok(mut transform) = query.single_mut() {
            // Zoom in or out based on scroll direction
            let zoom_factor = 1.0 - scroll * 0.1;
            transform.scale *= Vec3::splat(zoom_factor);
            
            // Clamp the scale to prevent zooming too far in or out
            transform.scale = transform.scale.clamp(
                Vec3::splat(0.1),
                Vec3::splat(5.0)
            );
        }
    }
}
