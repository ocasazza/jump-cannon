//! [`ObsidianLoader`] — the Obsidian vault adapter implementing
//! [`data_loader::Loader`].

use std::path::{Path, PathBuf};

use data_loader::{LoadResult, Loader};

use crate::extractor::extract_vault;

/// Loads a graph by walking an Obsidian vault on disk.
///
/// This is the canonical "first adapter" — it wraps the existing
/// `extract_vault` pipeline (walk `.md` files, parse frontmatter +
/// wikilinks, resolve edges) behind the generic [`Loader`] trait.
pub struct ObsidianLoader {
    root: PathBuf,
}

impl ObsidianLoader {
    /// Create a loader for the vault at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Loader for ObsidianLoader {
    fn name(&self) -> &str {
        "obsidian"
    }

    fn load(&self) -> LoadResult {
        let result = extract_vault(&self.root);
        LoadResult {
            graph: result.graph,
            unresolved: result.unresolved,
        }
    }

    fn root_path(&self) -> Option<&PathBuf> {
        Some(&self.root)
    }
}

/// Convenience: load a vault at `root` without going through the trait.
/// Kept for backward compatibility with callers that don't need the trait
/// object (tests, one-shot scripts).
pub fn load_vault(root: &Path) -> LoadResult {
    ObsidianLoader::new(root).load()
}