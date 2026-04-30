//! 6DoF perspective camera. Position + forward + up basis. WASD pans,
//! mouse-drag rotates pitch+yaw, scroll zooms (move along forward),
//! QE ascends/descends.

use glam::{Mat4, Vec3};

pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,   // radians, around world up (Y)
    pub pitch: f32, // radians, around right axis
    pub fov_y: f32,
    pub aspect: f32,
    pub znear: f32,
    pub zfar: f32,

    initial_position: Vec3,
    initial_yaw: f32,
    initial_pitch: f32,
}

impl Camera {
    pub fn new(aspect: f32) -> Self {
        let position = Vec3::new(0.0, 0.0, 1500.0);
        Self {
            position,
            yaw: -std::f32::consts::FRAC_PI_2, // looking down -Z
            pitch: 0.0,
            fov_y: 60f32.to_radians(),
            aspect,
            znear: 0.1,
            zfar: 200_000.0,
            initial_position: position,
            initial_yaw: -std::f32::consts::FRAC_PI_2,
            initial_pitch: 0.0,
        }
    }

    pub fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.sin() * self.pitch.cos(),
        )
        .normalize()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize()
    }

    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward()).normalize()
    }

    pub fn view_proj(&self) -> [[f32; 4]; 4] {
        let view = Mat4::look_to_rh(self.position, self.forward(), Vec3::Y);
        let proj = Mat4::perspective_rh(self.fov_y, self.aspect.max(0.0001), self.znear, self.zfar);
        (proj * view).to_cols_array_2d()
    }

    /// Pan along (right, up, forward) by raw deltas.
    pub fn pan(&mut self, dx: f32, dy: f32, dz: f32) {
        let r = self.right();
        let u = self.up();
        let f = self.forward();
        self.position += r * dx + u * dy + f * dz;
    }

    pub fn rotate_yaw(&mut self, d: f32) {
        self.yaw += d;
    }

    pub fn rotate_pitch(&mut self, d: f32) {
        self.pitch =
            (self.pitch + d).clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);
    }

    /// Move along forward by a multiplicative factor (>1 zoom in, <1 out).
    pub fn zoom(&mut self, factor: f32) {
        let f = self.forward();
        self.position += f * factor;
    }

    pub fn fit_to_bounds(&mut self, min: Vec3, max: Vec3) {
        let center = (min + max) * 0.5;
        let radius = ((max - min) * 0.5).length().max(1.0);
        let dist = radius * 1.4 / (self.fov_y * 0.5).sin();
        // back off along world +Z, look toward center.
        self.position = center + Vec3::Z * dist;
        // recompute yaw/pitch to look at center
        let dir = (center - self.position).normalize();
        self.pitch = dir.y.asin();
        self.yaw = dir.z.atan2(dir.x);
    }

    pub fn reset(&mut self) {
        self.position = self.initial_position;
        self.yaw = self.initial_yaw;
        self.pitch = self.initial_pitch;
    }

    /// Build a ray from NDC (x in [-1,1], y in [-1,1]) into the scene.
    pub fn raycast(&self, ndc_x: f32, ndc_y: f32) -> (Vec3, Vec3) {
        let f = self.forward();
        let r = self.right();
        let u = self.up();
        let tan_half = (self.fov_y * 0.5).tan();
        let dir = (f + r * ndc_x * tan_half * self.aspect + u * ndc_y * tan_half).normalize();
        (self.position, dir)
    }
}
