//! Host-owned Panel Kit workspace plumbing for Jump Cannon's two app views.

mod catalog;
mod events;
mod state;

pub(crate) use catalog::panel_catalog;
pub(crate) use events::{handle_key, handle_pointer_move, handle_pointer_up, handle_wheel};
pub(crate) use state::{
    mount_viewport_observer, project_workspace, use_panel_workspace, workspace_area_class,
    workspace_event_handler, PanelWorkspace,
};
