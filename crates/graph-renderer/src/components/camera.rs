use bevy::prelude::*;

/// Component for our main camera with movement speed
#[derive(Component)]
pub struct MainCamera {
    pub speed: f32,
}

impl Default for MainCamera {
    fn default() -> Self {
        Self {
            speed: 500.0,
        }
    }
}
