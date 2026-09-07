//! Generic byte-source connectors for runtime grammar packages.
//!
//! Every connector implements [`data_loader::SourceConnector`]: it acquires
//! bytes from one transport and emits [`data_loader::SourceRecord`] streams.
//! Parsing and graph projection belong to the engine crate (`crates/importer`);
//! these connectors deliberately stop at bytes plus acquisition metadata.
//!
//! Feature surface:
//!
//! | feature | contents | targets |
//! |---|---|---|
//! | `https` (default) | [`https::HttpsConnector`] GETs one URL into bytes | native via reqwest, wasm32 via gloo-net |
//! | `envelope` (default) | [`envelope::EnvelopeConnector`] expands tar/tar.gz/zip/gzip into one record per entry | native + wasm32 (pure Rust) |
//! | `ssh` | [`ssh::SshConnector`] reads one remote file over SSH | native only |
//! | `grpc` | [`grpc::GrpcConnector`] dynamically invokes one unary/server-streaming method | native only |
//!
//! Tokens, keys, and other credentials are runtime-bound connector
//! configuration. They never come from a grammar package, and every config
//! type redacts them from `Debug`.

#[cfg(feature = "envelope")]
pub mod envelope;
#[cfg(all(feature = "grpc", not(target_arch = "wasm32")))]
pub mod grpc;
#[cfg(feature = "https")]
pub mod https;
#[cfg(all(feature = "ssh", not(target_arch = "wasm32")))]
pub mod ssh;

#[cfg(feature = "envelope")]
pub use envelope::{EnvelopeConnector, EnvelopeLimits};
#[cfg(all(feature = "grpc", not(target_arch = "wasm32")))]
pub use grpc::{DescriptorSource, GrpcConfig, GrpcConnector};
#[cfg(feature = "https")]
pub use https::{HttpResponse, HttpTransport, HttpsConfig, HttpsConnector};
#[cfg(all(feature = "ssh", not(target_arch = "wasm32")))]
pub use ssh::{SshAuth, SshBackend, SshConfig, SshConnector};

/// Guess a media type from a file name or path extension. Connectors use this
/// for records whose transport carries no content-type (SSH files, archive
/// entries). Unknown extensions fall back to `application/octet-stream`.
pub fn guess_content_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("json" | "jsonl" | "ndjson") => "application/json",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("xml") => "application/xml",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        _ => "application/octet-stream",
    }
}
