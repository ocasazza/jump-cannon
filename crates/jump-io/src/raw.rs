//! Platform-agnostic input snapshot.
//!
//! Every adapter (egui, winit, web-sys PointerEvents, gilrs gamepad,
//! AppleScript MagicTrackpad shim, …) produces one of these per
//! frame. [`crate::InputCtx::poll`] consumes it without caring where
//! the bits came from.

use std::collections::HashSet;

use egui::{Key, PointerButton};
use serde::{Deserialize, Serialize};

/// `egui::PointerButton` doesn't implement `Hash`, so we keep a
/// fixed-slot bitset across the five known variants instead of
/// reaching for a `HashSet`. Cheap, allocation-free, and the API
/// reads the same as a set.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerButtonSet {
    bits: u8,
}

impl PointerButtonSet {
    const fn slot(btn: PointerButton) -> u8 {
        match btn {
            PointerButton::Primary => 1 << 0,
            PointerButton::Secondary => 1 << 1,
            PointerButton::Middle => 1 << 2,
            PointerButton::Extra1 => 1 << 3,
            PointerButton::Extra2 => 1 << 4,
        }
    }

    pub fn insert(&mut self, btn: PointerButton) {
        self.bits |= Self::slot(btn);
    }

    pub fn remove(&mut self, btn: PointerButton) {
        self.bits &= !Self::slot(btn);
    }

    pub fn contains(&self, btn: PointerButton) -> bool {
        self.bits & Self::slot(btn) != 0
    }

    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    pub fn clear(&mut self) {
        self.bits = 0;
    }

    pub fn iter(&self) -> impl Iterator<Item = PointerButton> + '_ {
        [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ]
        .into_iter()
        .filter(|b| self.contains(*b))
    }
}

/// Modifier-key state. Mirror of [`egui::Modifiers`] but
/// `Eq + Hash` so it can sit inside hashed binding tables.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub command: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        command: false,
    };

    pub const fn shift() -> Self {
        Self { shift: true, ..Self::NONE }
    }
    pub const fn ctrl() -> Self {
        Self { ctrl: true, ..Self::NONE }
    }
    pub const fn alt() -> Self {
        Self { alt: true, ..Self::NONE }
    }
    pub const fn command() -> Self {
        Self { command: true, ..Self::NONE }
    }

    pub fn from_egui(m: egui::Modifiers) -> Self {
        Self {
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            command: m.command,
        }
    }

    /// Strict equality on the modifier bitset.
    ///
    /// `Mods::NONE` matches **no modifiers held** (not "any") — this
    /// is the natural reading and stops a bare `Key::F` binding from
    /// firing when the user is also holding Cmd. Build a `KeyPress`
    /// with `Mods::shift()` etc. for combos.
    ///
    /// If a future use case needs "any-modifier-OK" semantics, add a
    /// `ModSpec::Any` variant to the trigger types — don't overload
    /// this struct.
    pub fn matches(&self, actual: Mods) -> bool {
        *self == actual
    }
}

/// Frame-scoped snapshot of every input device that the binding
/// engine cares about. Adapters fill the fields they support and
/// leave the rest at default — a winit-only build with no touchpad
/// pinch just leaves `pinch_delta` at 0.
#[derive(Default, Debug, Clone)]
pub struct RawInput {
    /// Seconds elapsed since the last poll. Held-key axes scale
    /// linearly with this so movement is frame-rate independent.
    pub dt: f32,

    pub modifiers: Mods,

    /// Keys currently held — drives `Trigger::KeyHeld`.
    pub keys_held: HashSet<Key>,
    /// Keys whose press edge happened during this frame — drives
    /// `Trigger::KeyPress`. Pulse triggers fire exactly once per
    /// press, even on platforms that auto-repeat.
    pub keys_pressed: HashSet<Key>,

    pub pointer_buttons_held: PointerButtonSet,
    pub pointer_buttons_pressed: PointerButtonSet,

    /// Cursor delta in screen pixels accumulated this frame. Used by
    /// `Trigger::PointerDrag`.
    pub pointer_delta: [f32; 2],

    /// Wheel scroll, in egui's smoothed pixel units. Touchpad
    /// two-finger scroll funnels through here too.
    pub wheel_delta: f32,

    /// Multiplicative pinch-zoom delta — 1.0 == no change. Sourced
    /// from `egui::InputState::zoom_delta()`, which already
    /// coalesces ctrl+wheel, macOS Magnify gestures, and browser
    /// touch pinch.
    pub pinch_delta: f32,
}
