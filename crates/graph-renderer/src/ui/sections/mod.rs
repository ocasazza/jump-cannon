pub mod camera;
pub mod cursor;
pub mod debug;
pub mod filter;
pub mod focus;
pub mod instances;
pub mod layout;
pub mod stats;
pub mod style;

use eframe::egui;

use super::actions::ActionRegistry;
use super::layout::registry::LayoutRegistry;
use super::state::Section;
use super::theme;
use super::widgets;
use crate::perf::PerfCollector;

// Re-export shared widget helpers under their historical names so the
// rest of the codebase (sections/* and layout/algorithms/*) keeps
// importing them via `use super::{header, ...}` without churn.
pub use widgets::{header, hint_label, reset_row, row, subgroup_label, subgroup_separator};

pub fn show(
    ui: &mut egui::Ui,
    section: Section,
    state: &mut super::state::AppState,
    registry: &mut ActionRegistry,
    layout_registry: &LayoutRegistry,
    perf: &PerfCollector,
) {
    header(ui, section.title());
    match section {
        Section::Filter => filter::show(ui, state),
        Section::Style => style::show(ui, state),
        Section::Layout => layout::show(ui, state, layout_registry),
        Section::Camera => camera::show(ui, state),
        Section::Focus => focus::show(ui, state),
        Section::Cursor => cursor::show(ui, state),
        Section::Stats => stats::show(ui, state),
        Section::Instances => instances::show(ui, state, registry),
        Section::Debug => debug::show(ui, state, perf),
    }
    let _ = theme::accent::RED; // keep accent module referenced from here for tooling
}
