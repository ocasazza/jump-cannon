//! App-first input handoff into the active Panel Kit workspace reducer.

use dioxus::events::{KeyboardEvent, PointerEvent as DioxusPointerEvent};
use dioxus::prelude::Readable;
use panel_kit::input::{clear_selection, keyboard_event, pointer_event, release_pointer, wheel_event};
use panel_kit_core::reducer::HitTarget;
use panel_kit_core::{FocusContext, PointerButton, PointerEventKind};

use super::state::{reduce_and_persist_workspace_event, PanelWorkspace};

/// Reduce root keyboard events after app handlers decline first refusal.
pub(crate) fn handle_key(workspace: &PanelWorkspace, event: &KeyboardEvent) {
    let focus = if panel_kit::input::is_editing() {
        FocusContext::TextInput
    } else if let Some(key) = workspace.snapshot.read().focused {
        FocusContext::Panel(key)
    } else {
        FocusContext::Workspace
    };

    let Some(workspace_event) = keyboard_event(event, focus) else {
        return;
    };
    if reduce_and_persist_workspace_event(workspace, workspace_event) {
        event.prevent_default();
    }
}

/// Reduce in-flight workspace pointer motion.
pub(crate) fn handle_pointer_move(workspace: &PanelWorkspace, event: &DioxusPointerEvent) {
    let kind = if workspace.snapshot.read().drag.is_some() {
        PointerEventKind::Drag(PointerButton::Primary)
    } else {
        PointerEventKind::Moved
    };
    reduce_and_persist_workspace_event(
        workspace,
        pointer_event(HitTarget::Workspace, event, kind),
    );
}

/// Settle root pointer gestures and persist settled layouts.
pub(crate) fn handle_pointer_up(workspace: &PanelWorkspace, event: &DioxusPointerEvent) {
    release_pointer(event);
    let changed = reduce_and_persist_workspace_event(
        workspace,
        pointer_event(
            HitTarget::Workspace,
            event,
            PointerEventKind::Up(PointerButton::Primary),
        ),
    );
    if changed {
        clear_selection();
    }
}

/// Reduce wheel input after native panel body scroll gets first refusal.
pub(crate) fn handle_wheel(workspace: &PanelWorkspace, event: &dioxus::events::WheelEvent) {
    if reduce_and_persist_workspace_event(workspace, wheel_event(event)) {
        event.prevent_default();
    }
}

