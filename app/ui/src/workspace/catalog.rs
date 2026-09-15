//! Stable panel identity catalog shared by Jump Cannon workspaces.

use std::rc::Rc;

use panel_kit::{LayoutBuilder, PanelWin};
use panel_kit_core::PanelCatalog;

use crate::Panel;

/// Shared catalog covering both app views without changing persisted enum IDs.
pub(crate) fn panel_catalog() -> Rc<PanelCatalog<Panel>> {
    Rc::new(
        PanelCatalog::from_panel_kind_layout(&catalog_layout())
            .expect("jump-cannon panels must serialize as stable string IDs"),
    )
}

/// One metadata seed for every panel kind that can appear in either view.
fn catalog_layout() -> Vec<PanelWin<Panel>> {
    let mut builder = LayoutBuilder::new();
    all_panel_kinds()
        .iter()
        .enumerate()
        .map(|(index, &kind)| {
            builder.at(kind, 16.0 + index as f64, 16.0, 320.0, 240.0)
        })
        .collect()
}

/// Authored panel order used only for catalog identity, not default placement.
fn all_panel_kinds() -> &'static [Panel] {
    &[
        Panel::Graph,
        Panel::Nodes,
        Panel::Inspector,
        Panel::Document,
        Panel::Progress,
        Panel::Settings,
        Panel::Help,
        Panel::Filter,
        Panel::Metrics,
        Panel::Instances,
        Panel::Generate,
        Panel::Timeline,
        Panel::Debug,
        Panel::Worlds,
        Panel::History,
        Panel::Branches,
        Panel::Merge,
        Panel::GitHub,
        Panel::GpuSessions,
        Panel::Importers,
    ]
}
