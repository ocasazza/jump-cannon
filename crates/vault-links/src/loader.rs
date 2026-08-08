//! [`ObsidianLoader`] — the Obsidian vault adapter implementing
//! [`data_loader::Loader`].

use std::path::{Path, PathBuf};

use data_loader::{
    ContentSchema, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportError,
    ImporterSchema, LoadResult, Loader, TagHierarchySchema,
};

use crate::extractor::{extract_vault, try_extract_vault};

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

    fn schema(&self) -> ImporterSchema {
        ImporterSchema::new(
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                    .searchable(4)
                    .snippet(),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(3)
                    .facetable(),
                DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("type", DiscoveryFieldType::Keyword, false)
                    .searchable(2)
                    .facetable(),
                DiscoveryField::new("folder", DiscoveryFieldType::Keyword, false)
                    .searchable(1)
                    .facetable(),
                DiscoveryField::new("body", DiscoveryFieldType::Text, true)
                    .searchable(1)
                    .snippet(),
                DiscoveryField::new("description", DiscoveryFieldType::Text, false)
                    .searchable(2)
                    .snippet(),
                DiscoveryField::new("status", DiscoveryFieldType::Keyword, false)
                    .searchable(1)
                    .facetable(),
                DiscoveryField::new("authors", DiscoveryFieldType::KeywordList, false)
                    .searchable(1)
                    .facetable(),
                DiscoveryField::new("entities", DiscoveryFieldType::KeywordList, false)
                    .searchable(1)
                    .facetable(),
                DiscoveryField::new("key_topics", DiscoveryFieldType::KeywordList, false)
                    .searchable(1)
                    .facetable(),
                DiscoveryField::new("related", DiscoveryFieldType::KeywordList, false)
                    .searchable(1)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed(
                "wikilink",
                "Obsidian wikilink from the source note to its target note",
            )],
            TagHierarchySchema::slash(),
        )
        .with_input_media_types(["text/markdown"])
        .with_content(ContentSchema {
            readable: true,
            writable: true,
            media_types: vec!["text/markdown".into()],
        })
    }

    fn load(&self) -> LoadResult {
        let result = extract_vault(&self.root);
        LoadResult {
            graph: result.graph,
            search_documents: result.search_documents,
            unresolved: result.unresolved,
        }
    }

    fn try_load(&self) -> Result<LoadResult, ImportError> {
        let result = try_extract_vault(&self.root).map_err(|error| ImportError::SourceRead {
            origin: self.root.to_string_lossy().into_owned(),
            message: format!("{error:#}"),
        })?;
        Ok(LoadResult {
            graph: result.graph,
            search_documents: result.search_documents,
            unresolved: result.unresolved,
        })
    }

    fn root_path(&self) -> Option<&PathBuf> {
        Some(&self.root)
    }

    fn additional_effects(&self) -> &'static [Effect] {
        &[Effect::Search, Effect::ContentRead, Effect::ContentWrite]
    }
}

/// Convenience: load a vault at `root` without going through the trait.
/// Kept for backward compatibility with callers that don't need the trait
/// object (tests, one-shot scripts).
pub fn load_vault(root: &Path) -> LoadResult {
    ObsidianLoader::new(root).load()
}
