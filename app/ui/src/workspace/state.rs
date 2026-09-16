//! Snapshot, projection, persistence, and viewport lifecycle for one workspace.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use panel_kit::store::LocalStorageLayoutStore;
use panel_kit::surface::{observe_viewport, surface_profile, viewport_size};
use panel_kit::PanelWin;
use panel_kit_core::frame::{
    project_into, ChromeProjectionInput, Placement, ProjectedFrame, ProjectionBuffer,
    ProjectionInput, TileLayoutMetrics,
};
use panel_kit_core::persist::{
    apply_save_decision, restore_snapshot, LayoutError, RestoreContext, SavePolicy,
};
use panel_kit_core::reducer::{reduce, ResizePolicy, Snapshot, Viewport, WorkspaceEvent};
use panel_kit_core::{
    ChromeMetrics, Clamp, CommandStep, Mode, PanelCatalog, SnapPolicy, TileMetrics, Units,
};

use crate::Panel;

/// Host-owned composable workspace state for one Jump Cannon view.
#[derive(Clone)]
pub(crate) struct PanelWorkspace {
    pub(crate) storage_key: &'static str,
    pub(crate) snapshot: Signal<Snapshot<Panel>>,
    pub(crate) catalog: Rc<PanelCatalog<Panel>>,
    pub(crate) scratch: Rc<RefCell<ProjectionBuffer<Panel>>>,
    store: Rc<LocalStorageLayoutStore>,
    save_policy: SavePolicy,
}

impl PartialEq for PanelWorkspace {
    fn eq(&self, other: &Self) -> bool {
        self.storage_key == other.storage_key
            && self.snapshot == other.snapshot
            && Rc::ptr_eq(&self.catalog, &other.catalog)
            && Rc::ptr_eq(&self.scratch, &other.scratch)
            && Rc::ptr_eq(&self.store, &other.store)
    }
}

/// Create one host-owned workspace from defaults and one stable layout key.
pub(crate) fn use_panel_workspace(
    storage_key: &'static str,
    defaults: fn() -> Vec<PanelWin<Panel>>,
    catalog: Rc<PanelCatalog<Panel>>,
) -> PanelWorkspace {
    let default_snapshot =
        Snapshot::from_defaults(defaults(), Mode::Floating, current_viewport());
    let store = use_hook(move || Rc::new(LocalStorageLayoutStore::new(storage_key)));
    let snapshot = use_signal({
        let catalog = catalog.clone();
        let store = store.clone();
        let defaults = default_snapshot.clone();
        move || restore_or_default(storage_key, &store, defaults.clone(), &catalog)
    });
    let scratch = use_hook({
        let panel_count = snapshot.peek().panels.len();
        move || Rc::new(RefCell::new(ProjectionBuffer::with_panel_capacity(panel_count)))
    });

    PanelWorkspace {
        storage_key,
        snapshot,
        catalog,
        scratch,
        store,
        save_policy: SavePolicy::OnSettle,
    }
}

/// Subscribe one workspace to viewport changes with Jump Cannon's scale policy.
pub(crate) fn mount_viewport_observer(workspace: &PanelWorkspace) {
    let emit = workspace_event_handler(workspace);
    observe_viewport(EventHandler::new(move |size: Viewport| {
        emit.call(WorkspaceEvent::ViewportChanged {
            size,
            policy: ResizePolicy::ScaleFloating,
        });
    }));
}

/// Build an event handler that reduces into one workspace and applies persistence.
pub(crate) fn workspace_event_handler(
    workspace: &PanelWorkspace,
) -> EventHandler<WorkspaceEvent<Panel>> {
    let workspace = workspace.clone();
    EventHandler::new(move |event| {
        reduce_and_persist_workspace_event(&workspace, event);
    })
}

/// Apply one reducer event and the explicit OnSettle persistence policy.
pub(super) fn reduce_and_persist_workspace_event(
    workspace: &PanelWorkspace,
    event: WorkspaceEvent<Panel>,
) -> bool {
    let mut snapshot_signal = workspace.snapshot;
    let mut snapshot = snapshot_signal.write();
    let context = reduce_context(&snapshot);
    let reduction = reduce(&mut snapshot, event, context);
    let changed = reduction.changed;
    let decision = workspace.save_policy.decide(&reduction);

    if let Err(error) =
        apply_save_decision(decision, &*workspace.store, &snapshot, &workspace.catalog)
    {
        log_layout_error("save layout", workspace.storage_key, &error);
    }

    changed
}

/// Project the current snapshot into caller-owned scratch for one render pass.
pub(crate) fn project_workspace<'frame>(
    snapshot: &Snapshot<Panel>,
    scratch: &'frame mut ProjectionBuffer<Panel>,
) -> ProjectedFrame<'frame, Panel> {
    let surface = surface_profile(snapshot.viewport.width);
    let chrome = ChromeProjectionInput::full(ChromeMetrics::WEB);
    let tile = TileLayoutMetrics::from_tile_metrics(TileMetrics::WEB, surface);

    project_into(
        ProjectionInput {
            snapshot,
            surface,
            chrome: &chrome,
            clamp: &Clamp::WEB,
            tile: &tile,
        },
        scratch,
    )
}

/// CSS class for the view area that contains projected panels.
pub(crate) fn workspace_area_class(frame: &ProjectedFrame<'_, Panel>) -> &'static str {
    if frame.panels.iter().any(|panel| {
        matches!(panel.placement, Placement::Maximized)
    }) {
        "ws maxed"
    } else if frame.mode == Mode::Tiling {
        "ws tiling"
    } else {
        "ws floating"
    }
}

/// Reducer context for Jump Cannon's browser workspace surface.
pub(crate) fn reduce_context(
    snapshot: &Snapshot<Panel>,
) -> panel_kit_core::reducer::ReduceContext<'static> {
    panel_kit_core::reducer::ReduceContext {
        surface: surface_profile(snapshot.viewport.width),
        clamp: &Clamp::WEB,
        command_step: CommandStep::WEB,
        tile: &TileMetrics::WEB,
        // Preserve continuous floating drag/resize; tiling span resize stays snapped.
        snap: SnapPolicy {
            resize: false,
            move_: false,
            ..SnapPolicy::default()
        },
    }
}

/// Current browser viewport expressed in the reducer's CSS-pixel units.
fn current_viewport() -> Viewport {
    let (width, height) = viewport_size();
    Viewport { width, height, units: Units::CssPx }
}

/// Restore a snapshot through the core V1/V2 reader, falling back visibly.
fn restore_or_default(
    storage_key: &str,
    store: &LocalStorageLayoutStore,
    defaults: Snapshot<Panel>,
    catalog: &PanelCatalog<Panel>,
) -> Snapshot<Panel> {
    let context = RestoreContext {
        units: Units::CssPx,
        viewport: (defaults.viewport.width, defaults.viewport.height),
    };

    match restore_snapshot(store, defaults.clone(), catalog, context) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log_layout_error("restore layout", storage_key, &error);
            defaults
        }
    }
}

/// Report layout persistence issues without hiding the host-side decision.
fn log_layout_error(action: &str, storage_key: &str, error: &LayoutError) {
    let message = format!("panel-kit {action} failed for storage key `{storage_key}`: {error}");

    #[cfg(target_arch = "wasm32")]
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&message));

    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}
