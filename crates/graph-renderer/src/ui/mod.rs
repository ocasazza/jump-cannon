pub mod actions;
pub mod anchored;
pub mod badge;
pub mod command_palette;
pub mod document_viewer;
pub mod examples;
pub mod field_index;
pub mod file_io;
pub mod filter_strip;
pub mod floating;
pub mod focus_set;
pub mod frontmatter_chip;
pub mod frontmatter_grid;
pub mod input;
pub mod inspector;
pub mod layout;
pub mod modal;
pub mod nix_extension;
pub mod page_viewer;
pub mod persist;
pub mod progress;
pub mod query;
pub mod sections;
pub mod share;
pub mod sidebar;
pub mod squircle;
pub mod state;
pub mod status_footer;
pub mod theme;
pub mod tiles;
pub mod traffic_lights;
pub mod widgets;
pub mod workspace;

pub use actions::{
    Action, ActionHandler, ActionInstance, ActionRegistry, ActionType, BuiltinAction, ParamValue,
    ParameterType,
};
pub use command_palette::{show as show_command_palette, CommandPaletteState, PaletteOutcome};
pub use modal::{show_modal, ModalAction, ModalState};
pub use state::{AppState, Section, STORAGE_KEY};
pub use theme::apply_default as apply_theme;
