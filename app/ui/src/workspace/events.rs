//! App-first input handoff into the active Panel Kit workspace reducer.

use dioxus::events::{KeyboardEvent, PointerEvent as DioxusPointerEvent};
use dioxus::prelude::Readable;
use panel_kit::input::{
    clear_selection, keyboard_event, pointer_event, release_pointer, tile_drag_pointer_event,
    wheel_event,
};
use panel_kit_core::reducer::HitTarget;
use panel_kit_core::{FocusContext, PointerButton, PointerEventKind};

use super::state::{project_workspace, reduce_and_persist_workspace_event, PanelWorkspace};

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

/// Reduce in-flight workspace pointer motion. A held or lifted tile drag
/// resolves its swap target against the painted frame's resting slots.
pub(crate) fn handle_pointer_move(workspace: &PanelWorkspace, event: &DioxusPointerEvent) {
    let workspace_event = {
        let snapshot = workspace.snapshot.read();
        if snapshot.tile_drag.is_some_and(|tile| tile.tracks_pointer()) {
            let mut scratch = workspace.scratch.borrow_mut();
            let frame = project_workspace(&snapshot, &mut scratch);
            tile_drag_pointer_event(&frame, event)
        } else if snapshot.drag.is_some() {
            pointer_event(
                HitTarget::Workspace,
                event,
                PointerEventKind::Drag(PointerButton::Primary),
            )
        } else {
            pointer_event(HitTarget::Workspace, event, PointerEventKind::Moved)
        }
    };
    reduce_and_persist_workspace_event(workspace, workspace_event);
}

/// Settle root pointer gestures and persist settled layouts.
pub(crate) fn handle_pointer_up(workspace: &PanelWorkspace, event: &DioxusPointerEvent) {
    settle_pointer(
        workspace,
        event,
        PointerEventKind::Up(PointerButton::Primary),
    );
}

/// Abandon a pointer gesture the browser cancelled: a lifted tile flies home.
pub(crate) fn handle_pointer_cancel(workspace: &PanelWorkspace, event: &DioxusPointerEvent) {
    settle_pointer(workspace, event, PointerEventKind::Cancel);
}

fn settle_pointer(workspace: &PanelWorkspace, event: &DioxusPointerEvent, kind: PointerEventKind) {
    release_pointer(event);
    let changed = reduce_and_persist_workspace_event(
        workspace,
        pointer_event(HitTarget::Workspace, event, kind),
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
