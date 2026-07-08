//! Tray-parity panels — one module per panel from the egui app's footer
//! launcher row (`status_footer.rs::show_tray`). Each module owns its file
//! completely: panel-local state as module-level `GlobalSignal`s, private
//! API helpers built on `crate::api::{get_json, get_proto, get_bytes, url}`,
//! and styles under its own anchor block in assets/app.css.

pub mod layout;
pub mod nodes;
pub mod style;
pub mod camera;
pub mod filter;
pub mod metrics;
pub mod instances;
pub mod generate;
pub mod timeline;
pub mod debug;
// phase 4: Inspector + Document moved out of main.rs for the parity port.
pub mod document;
pub mod inspector;
