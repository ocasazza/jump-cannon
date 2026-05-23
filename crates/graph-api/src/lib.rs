//! graph-api — axum backend exposing the vault graph as protobuf + binary endpoints.
//!
//! Wire format split:
//!   - bulk numeric (positions, edges, metrics): raw little-endian arrays
//!   - structured (init manifest, node metadata, search results): protobuf
//
// Future: this crate's lib surface is consumed by integration tests; `main.rs`
// is the CLI entry point.

pub mod binary;
pub mod browser;
pub mod compute_broker;
pub mod progress;
pub mod proto;
pub mod server;
pub mod state;
pub mod subprocess;
pub mod vault_loader;
pub mod watcher;

pub use server::router;
pub use state::AppState;
