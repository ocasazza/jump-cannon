//! Open a URL in the user's default browser. Cross-platform via `open` crate.
//
// Future: --no-browser CLI flag (already plumbed in main.rs) skips this.

pub fn open_url(url: &str) {
    if let Err(e) = open::that(url) {
        tracing::warn!(error = %e, "failed to open browser; copy URL manually");
    }
}
