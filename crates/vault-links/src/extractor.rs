use std::collections::HashMap;
use std::path::Path;

use data_loader::SearchDocument;
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

use anyhow::{Context, Result};

use crate::{
    parser::{parse_note, try_parse_note, ParsedNote},
    walker::{list_markdown, try_list_markdown},
};

/// Result of a full vault extraction.
pub struct ExtractionResult {
    /// The populated graph (nodes + resolved edges).
    pub graph: VaultGraph,
    /// Search/facet documents aligned one-to-one with graph nodes.
    pub search_documents: Vec<SearchDocument>,
    /// Wikilink targets that could not be resolved to any known note.
    pub unresolved: Vec<String>,
}

/// Walk `root`, parse every markdown file, and return a `VaultGraph` with
/// nodes for each note and directed edges for each wikilink.
pub fn extract_vault(root: &Path) -> ExtractionResult {
    let paths = match list_markdown(root) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("vault walk failed: {err}");
            return ExtractionResult {
                graph: VaultGraph::new(),
                search_documents: Vec::new(),
                unresolved: Vec::new(),
            };
        }
    };

    extract_paths(root, paths, |path| {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        Ok(parse_note(path, &text))
    })
    .expect("best-effort vault extraction is infallible")
}

/// Strict importer extraction that rejects walk, read, and frontmatter errors.
pub fn try_extract_vault(root: &Path) -> Result<ExtractionResult> {
    let paths = try_list_markdown(root)?;
    extract_paths(root, paths, |path| {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read markdown note {}", path.display()))?;
        try_parse_note(path, &text)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))
    })
}

fn extract_paths<F>(
    root: &Path,
    paths: Vec<(String, std::path::PathBuf, u64)>,
    mut parse_file: F,
) -> Result<ExtractionResult>
where
    F: FnMut(&Path) -> Result<ParsedNote>,
{
    // First pass: parse all notes and build a title → id lookup.
    let mut title_to_id: HashMap<String, String> = HashMap::new();
    let mut parsed: Vec<(String, crate::parser::ParsedNote, u64)> = Vec::new();

    for (id, path, mtime) in &paths {
        let note = parse_file(path)?;
        title_to_id
            .entry(note.title.clone())
            .or_insert_with(|| id.clone() as String);
        parsed.push((id.clone(), note, *mtime));
    }

    // Second pass: build graph nodes.
    let mut graph = VaultGraph::new();
    let mut search_documents = Vec::with_capacity(parsed.len());

    for (idx, (id, note, mtime)) in parsed.iter().enumerate() {
        let (_rel_id, abs_path, _mtime) = &paths[idx];

        let folder: String = abs_path
            .strip_prefix(root)
            .unwrap_or(abs_path)
            .components()
            .next()
            .map(|c: std::path::Component| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();

        let meta = NodeMeta {
            source_id: "obsidian".into(),
            title: note.title.clone(),
            tags: note.tags.clone(),
            frontmatter: note.frontmatter.clone(),
            mtime: *mtime as i64,
            path: id.clone(),
            doctype: note.doctype.clone(),
            folder: folder.clone(),
            content_type: Some("text/markdown".into()),
            content_readable: true,
            content_writable: true,
        };

        graph.add_node(VaultNode {
            id: id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });

        let mut document = SearchDocument::new(id)
            .with("id", id.clone())
            .with("title", note.title.clone())
            .with("tags", serde_json::json!(note.tags))
            .with("path", id.clone())
            .with("body", note.body.clone());
        if let Some(doctype) = &note.doctype {
            document.insert("type", doctype.clone());
        }
        if !folder.is_empty() {
            document.insert("folder", folder);
        }
        if let Some(description) = note.frontmatter.get("description").and_then(|v| v.as_str()) {
            document.insert("description", description.to_string());
        }
        if let Some(status) = note
            .frontmatter
            .get("status")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
        {
            document.insert("status", status.to_string());
        }
        for key in ["authors", "entities", "key_topics"] {
            if let Some(value) = note.frontmatter.get(key) {
                let values = metadata_strings(value, key == "authors");
                if !values.is_empty() {
                    document.insert(key, serde_json::json!(values));
                }
            }
        }
        if let Some(value) = note.frontmatter.get("related") {
            let values = metadata_strings(value, false)
                .into_iter()
                .map(|value| normalize_related(&value))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                document.insert("related", serde_json::json!(values));
            }
        }
        search_documents.push(document);
    }

    // Third pass: resolve wikilinks and add edges.
    let mut unresolved = Vec::new();

    for (id, note, _mtime) in &parsed {
        for link in &note.links {
            // Try to resolve: exact id match first, then title lookup.
            let target = if graph.nodes.contains_key(link.as_str()) {
                Some(link.clone())
            } else {
                title_to_id.get(link.as_str()).cloned()
            };

            match target {
                Some(target_id) => {
                    graph.add_edge(VaultEdge {
                        source: id.clone(),
                        target: target_id,
                    });
                }
                None => {
                    unresolved.push(link.clone());
                }
            }
        }
    }

    Ok(ExtractionResult {
        graph,
        search_documents,
        unresolved,
    })
}

fn metadata_strings(value: &serde_json::Value, split_commas: bool) -> Vec<String> {
    let strings: Vec<&str> = match value {
        serde_json::Value::String(value) => vec![value],
        serde_json::Value::Array(values) => {
            values.iter().filter_map(|value| value.as_str()).collect()
        }
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for value in strings {
        let pieces: Box<dyn Iterator<Item = &str>> = if split_commas {
            Box::new(value.split(','))
        } else {
            Box::new(std::iter::once(value))
        };
        for piece in pieces {
            let piece = piece.trim();
            if !piece.is_empty() && !out.iter().any(|existing| existing == piece) {
                out.push(piece.to_string());
            }
        }
    }
    out
}

fn normalize_related(value: &str) -> String {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .unwrap_or(trimmed);
    inner
        .split_once('|')
        .map_or(inner, |(target, _)| target)
        .trim()
        .to_string()
}
