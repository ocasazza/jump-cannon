//! Semantic input layer for jump-cannon.
//!
//! Translates platform-raw events (egui, winit, touch, gamepad) into
//! user-defined [`Action`]s through rebindable [`Binding`]s with
//! per-device [`Sensitivity`] curves.
//!
//! ## Shape
//!
//! 1. Define your app's action enum (e.g. `enum Cam { Pan, Rotate, Zoom, FitView, OpenPalette }`).
//! 2. Build a [`InputCtx<A>`] with default [`Binding`]s.
//! 3. Each frame, build a [`RawInput`] (use [`adapter::egui_raw`] from
//!    an `egui::InputState`, or fill it manually from winit / web-sys
//!    PointerEvents / `gilrs` for gamepads).
//! 4. Call [`InputCtx::poll`] and consume the resulting [`Event<A>`]s.
//!
//! The crate has no opinion on what an action *means* — consumers are
//! free to map `Cam::Pan` to camera deltas, scene rotation, or
//! anything else. Adding a new device (Bluetooth, stylus pressure)
//! means writing a new adapter that fills [`RawInput`] — no changes
//! to consumer call sites.

pub mod action;
pub mod adapter;
pub mod binding;
pub mod event;
pub mod raw;
pub mod sensitivity;
pub mod trigger;

pub use action::Action;
pub use binding::{Binding, BindingSet};
pub use event::Event;
pub use raw::{Mods, PointerButtonSet, RawInput};
pub use sensitivity::{Curve, Sensitivity};
pub use trigger::Trigger;

/// Top-level input context. Owns the binding set; reused across frames.
///
/// `A` is the consumer's action type — typically an enum implementing
/// the [`Action`] supertrait via the blanket impl.
#[derive(Debug, Clone)]
pub struct InputCtx<A: Action> {
    bindings: BindingSet<A>,
    /// Pointer buttons that started a drag in a previous frame and
    /// remain held — deltas keep flowing while these are non-empty.
    active_drags: PointerButtonSet,
}

impl<A: Action> Default for InputCtx<A> {
    fn default() -> Self {
        Self {
            bindings: BindingSet::default(),
            active_drags: PointerButtonSet::default(),
        }
    }
}

impl<A: Action> InputCtx<A> {
    pub fn new(bindings: BindingSet<A>) -> Self {
        Self {
            bindings,
            active_drags: PointerButtonSet::default(),
        }
    }

    pub fn bindings(&self) -> &BindingSet<A> {
        &self.bindings
    }

    pub fn bindings_mut(&mut self) -> &mut BindingSet<A> {
        &mut self.bindings
    }

    /// Walk every binding against `raw` and emit one [`Event`] per
    /// firing trigger.
    pub fn poll(&mut self, raw: &RawInput) -> Vec<Event<A>> {
        let mut out = Vec::new();
        // Track new active drags so subsequent frames keep emitting
        // PointerDrag events even after the press edge is gone.
        // Refresh the carry-set: any button still held stays in,
        // any button released drops out.
        let mut next = PointerButtonSet::default();
        for btn in raw.pointer_buttons_held.iter() {
            next.insert(btn);
        }
        self.active_drags = next;

        for b in self.bindings.iter() {
            if let Some(ev) = b.evaluate(raw, &self.active_drags) {
                out.push(ev);
            }
        }
        out
    }
}
