//! Per-binding gain + response curve.
//!
//! Curves let trackpad scroll feel different from a mouse wheel
//! without forking the binding — same `Trigger::Wheel`, different
//! `Sensitivity` on the binding for each device class. Default is
//! `Linear { gain: 1.0 }` so an unconfigured binding behaves like
//! the raw delta.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sensitivity {
    /// Multiplicative gain applied AFTER the curve.
    pub gain: f32,
    pub curve: Curve,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self { gain: 1.0, curve: Curve::Linear }
    }
}

impl Sensitivity {
    pub const fn linear(gain: f32) -> Self {
        Self { gain, curve: Curve::Linear }
    }
    pub const fn quadratic(gain: f32) -> Self {
        Self { gain, curve: Curve::Quadratic }
    }
    pub const fn cubic(gain: f32) -> Self {
        Self { gain, curve: Curve::Cubic }
    }
    pub const fn deadzoned(gain: f32, deadzone: f32) -> Self {
        Self {
            gain,
            curve: Curve::Deadzone(deadzone),
        }
    }

    /// Apply the curve + gain to a raw input value, preserving sign.
    pub fn apply(&self, raw: f32) -> f32 {
        let sign = raw.signum();
        let abs = raw.abs();
        let shaped = match self.curve {
            Curve::Linear => abs,
            Curve::Quadratic => abs * abs,
            Curve::Cubic => abs * abs * abs,
            Curve::Deadzone(t) => {
                if abs < t {
                    0.0
                } else {
                    (abs - t) / (1.0 - t).max(1e-6)
                }
            }
        };
        sign * shaped * self.gain
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    Linear,
    /// Squared — small motions stay small, big motions amplify.
    /// Good for camera rotation when the user wants precision near
    /// zero and speed at the extremes.
    Quadratic,
    /// Cubed — even more aggressive than Quadratic. Trackpad pinch
    /// zoom often wants this so a tiny finger move barely moves the
    /// camera but a bigger one ramps fast.
    Cubic,
    /// Linear past a deadzone in [0, 1). Below the threshold the
    /// output is 0 — used to reject gamepad-stick noise and
    /// trackpad jitter at rest.
    Deadzone(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_preserves_sign_and_gain() {
        let s = Sensitivity::linear(2.0);
        assert_eq!(s.apply(3.0), 6.0);
        assert_eq!(s.apply(-3.0), -6.0);
        assert_eq!(s.apply(0.0), 0.0);
    }

    #[test]
    fn quadratic_amplifies_and_keeps_sign() {
        let s = Sensitivity::quadratic(1.0);
        assert_eq!(s.apply(2.0), 4.0);
        assert_eq!(s.apply(-2.0), -4.0);
    }

    #[test]
    fn deadzone_clamps_small_inputs() {
        let s = Sensitivity::deadzoned(1.0, 0.2);
        assert_eq!(s.apply(0.1), 0.0);
        // 0.5 is 0.3 above threshold of 0.2 over remaining 0.8 → 0.375
        let v = s.apply(0.5);
        assert!((v - 0.375).abs() < 1e-5, "got {v}");
    }
}
