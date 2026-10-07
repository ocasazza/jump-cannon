//! 6DoF camera with selectable projection. Position + forward + up basis.
//! WASD pans, mouse-drag rotates pitch+yaw, scroll zooms (perspective:
//! dolly along forward; orthographic: shrink the view volume), QE
//! ascends/descends.

use glam::{Mat4, Vec3};

/// Camera projection model. `Orthographic::half_height` is the world-space
/// half-extent of the view volume's vertical axis — the ortho analog of
/// dolly distance (smaller = more zoomed in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    Perspective { fov_y: f32 },
    Orthographic { half_height: f32 },
}
impl Projection {
    pub const DEFAULT_FOV_Y: f32 = std::f32::consts::FRAC_PI_3; // 60°
}

#[derive(Clone)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,   // radians, around world up (Y)
    pub pitch: f32, // radians, around right axis
    pub projection: Projection,
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
            projection: Projection::Perspective {
                fov_y: Projection::DEFAULT_FOV_Y,
            },
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

    /// Projection matrix for the given aspect ratio. Split out of
    /// `view_proj` so CPU-side picking (pipelines raycast / edge pick)
    /// builds the *same* projection the GPU used, for the active model.
    pub fn proj_matrix(&self, aspect: f32) -> Mat4 {
        let aspect = aspect.max(0.0001);
        match self.projection {
            Projection::Perspective { fov_y } => {
                Mat4::perspective_rh(fov_y, aspect, self.znear, self.zfar)
            }
            Projection::Orthographic { half_height } => {
                let hh = half_height.max(1.0);
                let hw = hh * aspect;
                Mat4::orthographic_rh(-hw, hw, -hh, hh, self.znear, self.zfar)
            }
        }
    }

    /// NDC-per-world-unit vertical scale of the active projection
    /// (perspective: `1/tan(fov/2)`; ortho: `1/half_height`). Staged into
    /// `CameraUniform.proj_scale`; shaders multiply world-space lengths by
    /// `proj_scale * screen.y * 0.5` (perspective also divides by view
    /// depth) to get pixels.
    pub fn proj_scale(&self) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => 1.0 / (fov_y * 0.5).tan(),
            Projection::Orthographic { half_height } => 1.0 / half_height.max(1.0),
        }
    }

    /// True when the orthographic model is active — shaders branch on
    /// this (perspective divides world lengths by view depth, ortho does
    /// not).
    pub fn is_ortho(&self) -> bool {
        matches!(self.projection, Projection::Orthographic { .. })
    }

    /// Current vertical field of view in radians, or the ortho-equivalent
    /// value derived from `half_height` at the camera's distance to the
    /// origin. Panel slider support.
    pub fn fov_y(&self) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => fov_y,
            Projection::Orthographic { half_height } => {
                let d = self.position.length().max(1.0);
                2.0 * (half_height.max(1.0) / d).atan()
            }
        }
    }

    pub fn view_proj(&self) -> [[f32; 4]; 4] {
        let view = Mat4::look_to_rh(self.position, self.forward(), Vec3::Y);
        (self.proj_matrix(self.aspect) * view).to_cols_array_2d()
    }

    /// Camera view matrix only (no projection). Used by shaders that need
    /// view-space depth (camera-orthogonal focal plane).
    pub fn view(&self) -> [[f32; 4]; 4] {
        Mat4::look_to_rh(self.position, self.forward(), Vec3::Y).to_cols_array_2d()
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
        self.pitch = (self.pitch + d).clamp(
            -std::f32::consts::FRAC_PI_2 + 0.01,
            std::f32::consts::FRAC_PI_2 - 0.01,
        );
    }

    /// Zoom by a signed world-unit delta (positive = in). Perspective
    /// dollies along forward; orthographic shrinks the view volume (a
    /// dolly would change nothing but clipping).
    pub fn zoom(&mut self, factor: f32) {
        match &mut self.projection {
            Projection::Perspective { .. } => {
                let f = self.forward();
                self.position += f * factor;
            }
            Projection::Orthographic { half_height } => {
                *half_height = (*half_height - factor).clamp(1.0, 100_000.0);
            }
        }
    }

    /// Re-aim the camera at `point` (no orientation change) while pulling
    /// the position to `distance` along the current forward. Used by the
    /// badge → focus-node flow so clicking a chip slides the viewport over
    /// the corresponding node without rotating the user's chosen angle.
    ///
    /// `distance < znear` is clamped up; `distance.is_finite()` is required.
    pub fn look_at_point(&mut self, point: Vec3, distance: f32) {
        if !point.is_finite() || !distance.is_finite() {
            return;
        }
        let d = distance.max(self.znear * 2.0);
        let dir = self.forward();
        self.position = point - dir * d;
        // Snap yaw/pitch so forward exactly hits `point` even if `dir`
        // came back not-quite-unit-length (precision creep over long
        // sessions). One call into the same formula `forward()` uses.
        let to = (point - self.position).normalize_or_zero();
        if to != Vec3::ZERO {
            self.pitch = to.y.asin();
            self.yaw = to.z.atan2(to.x);
        }
    }

    /// Distance `fit_to_bounds` places the camera at to frame a sphere of
    /// the given `radius`. Shares the framing formula with `fit_to_bounds`;
    /// the region-map auto-level rule asks "what distance would fit this
    /// radius?" without moving the camera. Orthographic has no dolly
    /// distance, so it returns the same near/far-satisfying standoff
    /// `fit_to_bounds` uses for the ortho position.
    pub fn fit_distance(&self, radius: f32) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => radius * 1.7 / (fov_y * 0.5).sin(),
            Projection::Orthographic { .. } => (radius * 4.0).max(self.znear * 10.0),
        }
    }

    pub fn fit_to_bounds(&mut self, min: Vec3, max: Vec3) {
        let center = (min + max) * 0.5;
        let radius = ((max - min) * 0.5).length().max(1.0);
        // 1.7× padding (was 1.4× — felt too cramped). With fov_y=60°
        // this lands at ≈ 3.4 × radius, giving the cluster ~25%
        // breathing room on every edge of the viewport.
        match &mut self.projection {
            Projection::Perspective { fov_y } => {
                let dist = radius * 1.7 / (*fov_y * 0.5).sin();
                // back off along world +Z, look toward center.
                self.position = center + Vec3::Z * dist;
            }
            Projection::Orthographic { half_height } => {
                *half_height = radius * 1.7;
                // Position only needs to satisfy near/far; scale comes
                // from half_height.
                self.position = center + Vec3::Z * (radius * 4.0).max(self.znear * 10.0);
            }
        }
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
    /// Ortho rays originate on the view plane and all run along forward.
    pub fn raycast(&self, ndc_x: f32, ndc_y: f32) -> (Vec3, Vec3) {
        let f = self.forward();
        match self.projection {
            Projection::Perspective { fov_y } => {
                let r = self.right();
                let u = self.up();
                let tan_half = (fov_y * 0.5).tan();
                let dir =
                    (f + r * ndc_x * tan_half * self.aspect + u * ndc_y * tan_half).normalize();
                (self.position, dir)
            }
            Projection::Orthographic { half_height } => {
                let hh = half_height.max(1.0);
                let origin = self.position
                    + self.right() * ndc_x * hh * self.aspect
                    + self.up() * ndc_y * hh;
                (origin, f)
            }
        }
    }
}
