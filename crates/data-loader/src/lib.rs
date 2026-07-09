//! data-loader — generic data-loading trait.
//!
//! Adapters implement [`Loader`] to produce a [`vault_data::VaultGraph`] from
//! any data source: an Obsidian vault, a tvix-generated graph, a CSV edge list,
//! a database dump, etc.
//!
//! The trait is the contract. `graph-api` consumes it through a boxed trait
//! object so the loader is selected at startup (CLI flag / env var) and the
//! rest of the pipeline — metrics, binary caches, watcher reloads — stays
//! loader-agnostic.

use std::path::PathBuf;

use vault_data::VaultGraph;

/// The result of a single load pass.
#[derive(Debug)]
pub struct LoadResult {
    /// The populated graph (nodes + resolved edges).
    pub graph: VaultGraph,
    /// References that could not be resolved to any known node.
    /// For Obsidian: wikilinks with no matching note.
    /// For tvix: always empty (generated graphs are self-consistent).
    pub unresolved: Vec<String>,
}

/// A data source that can produce a [`VaultGraph`].
///
/// Implementations are stateless request processors: each call to [`load`]
/// produces a fresh graph from the source. The caller (graph-api) owns the
/// lifecycle — caching, metrics, binary buffers, watcher reloads.
///
/// # Watching for changes
///
/// Loaders that back a live filesystem (Obsidian vault) can optionally provide
/// a [`Watcher`] handle. Loaders for static / generated data (tvix, CSV) return
/// `None` — the caller skips the filesystem watcher for those sources.
pub trait Loader: Send + Sync {
    /// Human-readable name for progress / UI (e.g. "obsidian", "tvix").
    fn name(&self) -> &str;

    /// Produce a fresh graph from the source.
    fn load(&self) -> LoadResult;

    /// The root path this loader reads from, if any. Used by the watcher to
    /// know *what* to watch. Returns `None` for sources that have no
    /// filesystem root (tvix, in-memory generators).
    fn root_path(&self) -> Option<&PathBuf> {
        None
    }
}

/// Enum of known loader types. Used for CLI dispatch (`--source <name>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// Walk an Obsidian vault on disk (the default).
    Obsidian,
    /// Evaluate a tvix Nix expression to produce a graph.
    Tvix,
    /// Generate a random graph directly in Rust (fast, no Nix eval).
    /// Controlled by --nodes and --edges CLI flags.
    Generate,
}

impl SourceKind {
    /// Parse from a CLI string. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "obsidian" | "vault" => Some(Self::Obsidian),
            "tvix" | "nix" => Some(Self::Tvix),
            "generate" | "gen" | "random" => Some(Self::Generate),
            _ => None,
        }
    }

    /// All known source kinds (for help text).
    pub fn all() -> &'static [&'static str] {
        &["obsidian", "tvix", "generate"]
    }
}