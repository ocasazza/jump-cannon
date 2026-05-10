//! [`Trigger`]: the input pattern that fires a binding.
//!
//! Each variant has a defined output shape (Pulse / Axis1 / Axis2)
//! that `Binding::evaluate` honours when emitting events. Modifier
//! bits are part of the pattern — `KeyPress { Escape, NONE }` and
//! `KeyPress { Escape, ctrl }` are distinct triggers.

use egui::{Key, PointerButton};
use serde::{Deserialize, Serialize};

use crate::raw::Mods;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    /// Edge-triggered. Fires once when the key transitions to held.
    /// Good for Cmd+P, Esc, F-to-fit. Output: [`Output::Pulse`].
    KeyPress { key: Key, mods: Mods },

    /// Level-triggered. Fires every frame while the key is held —
    /// the emitted axis value is `dt` (so consumers multiply by their
    /// own world-space speed). Sensitivity is applied on top.
    /// Output: [`Output::Axis1`].
    KeyHeld { key: Key, mods: Mods },

    /// Edge-triggered. Fires on press of a pointer button.
    /// Output: [`Output::Pulse`].
    PointerPress { button: PointerButton, mods: Mods },

    /// Level-triggered while the pointer button is held. Emits the
    /// pointer delta accumulated this frame. Sensitivity scales each
    /// component independently. Output: [`Output::Axis2`].
    PointerDrag { button: PointerButton, mods: Mods },

    /// Wheel scroll — egui smoothed delta. Output: [`Output::Axis1`].
    Wheel { mods: Mods },

    /// Pinch / Magnify — log-space so `apply` curves work intuitively
    /// (zooming in 1.05x reads as +0.0488). Output: [`Output::Axis1`].
    Pinch,
}

/// Declares what shape of [`crate::Event`] a trigger produces. Used
/// by the binding evaluator to keep the type-safety story tidy
/// without per-trigger plumbing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    Pulse,
    Axis1,
    Axis2,
}

impl Trigger {
    pub fn output(&self) -> Output {
        match self {
            Trigger::KeyPress { .. } | Trigger::PointerPress { .. } => Output::Pulse,
            Trigger::KeyHeld { .. } | Trigger::Wheel { .. } | Trigger::Pinch => Output::Axis1,
            Trigger::PointerDrag { .. } => Output::Axis2,
        }
    }
}
