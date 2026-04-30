//! Static frontend bundle for the graph viewer. Embedded via `include_dir!()`
//! and served by `graph-api`.
//
// Future: this crate could ship as a separate static bundle for CDN hosting
// when the API moves to a remote machine; the embed-via-Rust path stays as
// the single-binary deployment option.

use include_dir::{include_dir, Dir};

static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

pub fn assets() -> &'static Dir<'static> {
    &ASSETS
}
