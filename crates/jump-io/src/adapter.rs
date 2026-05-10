//! Adapters from platform-specific input streams into [`RawInput`].
//!
//! `egui_raw` is the production path — eframe gives the same
//! `egui::InputState` to native and WASM, so most app code stays on
//! that one adapter. `winit_raw` is a hand-written companion for
//! the standalone native binary that doesn't run egui.
//!
//! Adapters for additional devices — gilrs gamepads, web-sys
//! PointerEvents (stylus pressure, pointerType), Bluetooth HID
//! shims — drop in next to these without changing the binding API.

use egui::InputState;

use crate::raw::{Mods, PointerButtonSet, RawInput};

/// Build a [`RawInput`] from an `egui::InputState` snapshot.
///
/// `dt` is the seconds elapsed since the last call — eframe gives
/// you this via `ctx.input(|i| i.stable_dt)`.
pub fn egui_raw(input: &InputState, dt: f32) -> RawInput {
    let mut keys_held = std::collections::HashSet::new();
    let mut keys_pressed = std::collections::HashSet::new();
    for ev in &input.events {
        if let egui::Event::Key {
            key,
            pressed,
            repeat,
            ..
        } = ev
        {
            if *pressed && !*repeat {
                keys_pressed.insert(*key);
            }
        }
        let _ = ev;
    }
    // egui's keys_down lists every key currently held — feed it directly.
    for &k in &input.keys_down {
        keys_held.insert(k);
    }

    let mut pointer_buttons_held = PointerButtonSet::default();
    let mut pointer_buttons_pressed = PointerButtonSet::default();
    for btn in [
        egui::PointerButton::Primary,
        egui::PointerButton::Secondary,
        egui::PointerButton::Middle,
        egui::PointerButton::Extra1,
        egui::PointerButton::Extra2,
    ] {
        if input.pointer.button_down(btn) {
            pointer_buttons_held.insert(btn);
        }
        if input.pointer.button_pressed(btn) {
            pointer_buttons_pressed.insert(btn);
        }
    }

    let pointer_delta = {
        let d = input.pointer.delta();
        [d.x, d.y]
    };

    let wheel_delta = input.smooth_scroll_delta.y;
    let pinch_delta = input.zoom_delta();

    RawInput {
        dt,
        modifiers: Mods::from_egui(input.modifiers),
        keys_held,
        keys_pressed,
        pointer_buttons_held,
        pointer_buttons_pressed,
        pointer_delta,
        wheel_delta,
        pinch_delta,
    }
}
