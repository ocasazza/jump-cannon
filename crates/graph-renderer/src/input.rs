//! Cross-platform input → camera deltas. Same struct used native and WASM.

use crate::camera::Camera;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    W,
    A,
    S,
    D,
    Q,
    E,
    R,
    F,
    Shift,
}

#[derive(Default)]
pub struct InputState {
    pub keys: HashSet<Key>,
    pub mouse_dragging: bool,
    pub mouse_delta: (f32, f32),
    pub wheel_delta: f32,
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&mut self, k: Key) {
        self.keys.insert(k);
    }

    pub fn release(&mut self, k: Key) {
        self.keys.remove(&k);
    }

    pub fn apply_to_camera(&mut self, cam: &mut Camera, dt: f32) {
        let speed = if self.keys.contains(&Key::Shift) { 5.0 } else { 1.0 } * 400.0 * dt;

        let mut dx = 0.0;
        let mut dy = 0.0;
        let mut dz = 0.0;

        if self.keys.contains(&Key::W) {
            dz += speed;
        }
        if self.keys.contains(&Key::S) {
            dz -= speed;
        }
        if self.keys.contains(&Key::D) {
            dx += speed;
        }
        if self.keys.contains(&Key::A) {
            dx -= speed;
        }
        if self.keys.contains(&Key::Q) {
            dy += speed;
        }
        if self.keys.contains(&Key::E) {
            dy -= speed;
        }
        if self.keys.contains(&Key::R) {
            dz += speed * 2.0;
        }
        if self.keys.contains(&Key::F) {
            dz -= speed * 2.0;
        }

        if dx != 0.0 || dy != 0.0 || dz != 0.0 {
            cam.pan(dx, dy, dz);
        }

        if self.mouse_dragging && (self.mouse_delta.0 != 0.0 || self.mouse_delta.1 != 0.0) {
            // Mouse movement → yaw/pitch.
            let sensitivity = 0.005;
            cam.rotate_yaw(self.mouse_delta.0 * sensitivity);
            cam.rotate_pitch(-self.mouse_delta.1 * sensitivity);
        }
        self.mouse_delta = (0.0, 0.0);

        if self.wheel_delta != 0.0 {
            cam.zoom(self.wheel_delta * 50.0);
            self.wheel_delta = 0.0;
        }
    }
}
