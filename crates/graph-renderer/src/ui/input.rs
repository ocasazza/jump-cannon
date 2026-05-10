//! Project-side input layer — bridges [`jump_io`] semantic actions to
//! the App's existing handlers.
//!
//! Why a project-local `AppAction` enum: jump-io is generic by design.
//! The actual action vocabulary (Cmd+P opens the palette, F fits the
//! camera, WASDQE pans, RMB-drag rotates, …) is jump-cannon-specific
//! and lives here.
//!
//! Bindings are constructed in code via [`default_bindings`]. When a
//! rebinding UI lands the [`jump_io::BindingSet`] serializes straight
//! into [`super::state::WorkspaceSettings`].
//!
//! ## Coordinate convention
//!
//! Pan axes use the *project* convention (W/S = vertical, Q/E =
//! forward/back, A/D = strafe — swapped from FPS, see workspace.rs
//! comment). The sign of the binding's `Sensitivity::gain` encodes
//! direction so the consumer just sums `Axis1` values into pan_{x,y,z}.

use eframe::egui;
use jump_io::{Binding, BindingSet, Mods, Sensitivity, Trigger};
use serde::{Deserialize, Serialize};

/// Semantic input actions for jump-cannon. Pulse vs axis is implied
/// by which [`Trigger`] each variant binds to — the binding evaluator
/// hands the right `Event` shape back.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppAction {
    // ---- pulses ----
    /// Toggle the Ctrl/⌘+P command palette.
    OpenPalette,
    /// Esc — closes whatever modal/overlay is on top.
    Cancel,
    /// Bare F — fit camera to graph bounds.
    FitCamera,

    // ---- axis-1 (held-key pan) ----
    /// Strafe along the camera right axis. Sign carries direction (A
    /// negative, D positive — encoded in the binding's gain).
    PanX,
    /// Vertical pan along the camera up axis (W positive, S negative).
    PanY,
    /// Forward/back pan along the camera look axis (Q positive, E
    /// negative — Minecraft-creative convention, swapped from FPS).
    PanZ,

    // ---- axis-2 (pointer drag) ----
    /// Camera yaw/pitch from RMB- or MMB-drag.
    CameraRotate,

    // ---- axis-1 (zoom) ----
    /// Mouse-wheel + two-finger trackpad scroll. Separate from
    /// `CameraZoomPinch` so the consumer can drop wheel events on
    /// frames where pinch also fires (some trackpads emit both).
    /// Lets users tune scroll- and pinch-sensitivity independently.
    CameraZoomWheel,
    /// Pinch / Magnify gestures + ctrl+wheel.
    CameraZoomPinch,
}

/// Built-in default bindings for jump-cannon. Mirrors the shortcuts
/// historically hard-coded in `app.rs` / `ui/workspace.rs`.
pub fn default_bindings() -> BindingSet<AppAction> {
    BindingSet::from_iter([
        // ---- shortcut pulses ----
        Binding::new(
            // Cmd+P / Ctrl+P — egui::Modifiers::command coalesces both,
            // so a single binding covers macOS ⌘ and Linux/Win Ctrl.
            Trigger::KeyPress { key: egui::Key::P, mods: Mods::command() },
            AppAction::OpenPalette,
        ),
        Binding::new(
            Trigger::KeyPress { key: egui::Key::Escape, mods: Mods::NONE },
            AppAction::Cancel,
        ),
        Binding::new(
            Trigger::KeyPress { key: egui::Key::F, mods: Mods::NONE },
            AppAction::FitCamera,
        ),

        // ---- WASDQE pan (held axis) ----
        // Sensitivity gain encodes direction: A=-1, D=+1, W=+1, S=-1,
        // Q=+1, E=-1. The consumer in workspace.rs multiplies the
        // emitted Axis1 value (= dt * gain) by the eased pan speed +
        // Shift multiplier on top — see PAN_BASE/PAN_MAX/PAN_RAMP.
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::A, mods: Mods::NONE },
            AppAction::PanX,
        ).with_sensitivity(Sensitivity::linear(-1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::D, mods: Mods::NONE },
            AppAction::PanX,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::W, mods: Mods::NONE },
            AppAction::PanY,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::S, mods: Mods::NONE },
            AppAction::PanY,
        ).with_sensitivity(Sensitivity::linear(-1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::Q, mods: Mods::NONE },
            AppAction::PanZ,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::E, mods: Mods::NONE },
            AppAction::PanZ,
        ).with_sensitivity(Sensitivity::linear(-1.0)),
        // Shift+WASDQE — same actions, same gain, just held with the
        // boost modifier. The binding requires Shift exactly so the
        // unmodified bindings above don't fire when boosting.
        // Speed-multiplier-while-held is applied consumer-side.
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::A, mods: Mods::shift() },
            AppAction::PanX,
        ).with_sensitivity(Sensitivity::linear(-1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::D, mods: Mods::shift() },
            AppAction::PanX,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::W, mods: Mods::shift() },
            AppAction::PanY,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::S, mods: Mods::shift() },
            AppAction::PanY,
        ).with_sensitivity(Sensitivity::linear(-1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::Q, mods: Mods::shift() },
            AppAction::PanZ,
        ).with_sensitivity(Sensitivity::linear(1.0)),
        Binding::new(
            Trigger::KeyHeld { key: egui::Key::E, mods: Mods::shift() },
            AppAction::PanZ,
        ).with_sensitivity(Sensitivity::linear(-1.0)),

        // ---- camera rotate (RMB / MMB drag) ----
        // Plain RMB-drag and MMB-drag both rotate. RMB+Shift is
        // reserved for the cursor "repel" tool, so the consumer
        // suppresses CameraRotate when Shift is held — the binding
        // itself doesn't gate on Shift because we still want
        // Mods::NONE drag (no modifiers) to rotate.
        Binding::new(
            Trigger::PointerDrag {
                button: egui::PointerButton::Secondary,
                mods: Mods::NONE,
            },
            AppAction::CameraRotate,
        ),
        Binding::new(
            Trigger::PointerDrag {
                button: egui::PointerButton::Middle,
                mods: Mods::NONE,
            },
            AppAction::CameraRotate,
        ),

        // ---- zoom (wheel + pinch) ----
        Binding::new(
            Trigger::Wheel { mods: Mods::NONE },
            AppAction::CameraZoomWheel,
        ),
        Binding::new(Trigger::Pinch, AppAction::CameraZoomPinch),
    ])
}
