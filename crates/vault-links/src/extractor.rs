use std::collections::{HashMap, HashSet};
use std::path::Path;

use data_loader::identity::Namespace;
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
        let bytes = std::fs::read(path).unwrap_or_default();
        let text = String::from_utf8(bytes.clone()).unwrap_or_default();
        Ok((parse_note(path, &text), bytes))
    })
    .expect("best-effort vault extraction is infallible")
}

/// Strict importer extraction that rejects walk, read, and frontmatter errors.
pub fn try_extract_vault(root: &Path) -> Result<ExtractionResult> {
    let paths = try_list_markdown(root)?;
    extract_paths(root, paths, |path| {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read markdown note {}", path.display()))?;
        let text = String::from_utf8(bytes.clone())
            .with_context(|| format!("read markdown note {} as UTF-8", path.display()))?;
        let note = try_parse_note(path, &text)
            .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?;
        Ok((note, bytes))
    })
}

/// Parse an in-memory vault corpus — `(vault-relative path, markdown text)`
/// pairs — through the exact on-disk pipeline and return the same
/// [`ExtractionResult`].
///
/// This is the no-filesystem entry point: browser/WASM hosts fetch note
/// bytes over HTTP (GitHub trees + raw endpoints) and hand them in here.
/// Paths are vault-relative with `/` separators; non-`.md` entries and
/// paths matching the canonical exclusion contract
/// ([`crate::walker::EXCLUDES`], via [`crate::walker::is_excluded`]) are
/// dropped exactly as the filesystem walk drops them. Note IDs are derived
/// byte-identically to [`list_markdown`] (relative path sans extension);
/// mtimes are 0 — an HTTP corpus carries no meaningful mtime.
///
/// Best-effort like [`extract_vault`]: malformed frontmatter parses with its
/// metadata stripped rather than failing the whole corpus.
pub fn extract_notes(files: &[(String, String)]) -> ExtractionResult {
    extract_memory(files, false).expect("best-effort note extraction is infallible")
}

/// Strict variant of [`extract_notes`] that rejects malformed frontmatter,
/// mirroring [`try_extract_vault`]'s importer-publication contract.
pub fn try_extract_notes(files: &[(String, String)]) -> Result<ExtractionResult> {
    extract_memory(files, true)
}

fn extract_memory(files: &[(String, String)], strict: bool) -> Result<ExtractionResult> {
    let root = Path::new("");
    let mut texts: HashMap<std::path::PathBuf, &str> = HashMap::with_capacity(files.len());
    let mut paths = Vec::with_capacity(files.len());
    for (rel, text) in files {
        let rel_path = std::path::PathBuf::from(rel);
        if rel_path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if crate::walker::is_excluded(&rel_path) {
            continue;
        }
        let id = rel_path
            .with_extension("")
            .to_string_lossy()
            .to_string()
            .replace('\\', "/");
        texts.insert(rel_path.clone(), text.as_str());
        paths.push((id, rel_path, 0));
    }
    extract_paths(root, paths, |path| {
        let text = texts.get(path).copied().unwrap_or_default();
        let note = if strict {
            try_parse_note(path, text)
                .with_context(|| format!("parse YAML frontmatter in {}", path.display()))?
        } else {
            parse_note(path, text)
        };
        Ok((note, text.as_bytes().to_vec()))
    })
}

fn extract_paths<F>(
    root: &Path,
    paths: Vec<(String, std::path::PathBuf, u64)>,
    mut parse_file: F,
) -> Result<ExtractionResult>
where
    F: FnMut(&Path) -> Result<(ParsedNote, Vec<u8>)>,
{
    // Unified identity contract: `obsidian:obsidian:{vault-relative path}`.
    let namespace = Namespace::new("obsidian", "obsidian")
        .expect("the obsidian namespace is valid by construction");

    // First pass: parse all notes and build a title → local-id lookup.
    let mut title_to_id: HashMap<String, String> = HashMap::new();
    let mut parsed: Vec<(String, ParsedNote, Vec<u8>, u64)> = Vec::new();

    for (id, path, mtime) in &paths {
        let (note, bytes) = parse_file(path)?;
        title_to_id
            .entry(note.title.clone())
            .or_insert_with(|| id.clone() as String);
        parsed.push((id.clone(), note, bytes, *mtime));
    }

    // Second pass: build graph nodes under the unified namespace.
    let mut graph = VaultGraph::new();
    let mut search_documents = Vec::with_capacity(parsed.len());
    let mut unresolved = Vec::new();
    let mut local_ids: HashSet<&str> = HashSet::with_capacity(parsed.len());

    for (idx, (id, note, bytes, mtime)) in parsed.iter().enumerate() {
        let node_id = match namespace.node_id(id) {
            Ok(node_id) => node_id,
            Err(error) => {
                unresolved.push(format!("{id}: {error}"));
                continue;
            }
        };
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
            // The vault-relative local path — /vault/page and the node-content
            // reader resolve it against the vault root.
            path: id.clone(),
            doctype: note.doctype.clone(),
            folder: folder.clone(),
            content_type: Some("text/markdown".into()),
            content_readable: true,
            content_writable: true,
        };

        graph.add_node(VaultNode {
            id: node_id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });
        local_ids.insert(id.as_str());

        let mut document = SearchDocument::new(&node_id)
            .with("id", node_id)
            .with("title", note.title.clone())
            .with("tags", serde_json::json!(note.tags))
            .with("path", id.clone())
            .with("body", note.body.clone())
            .with("content_hash", namespace.content_id(bytes));
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

    // Third pass: resolve wikilinks against local ids and titles, then emit
    // namespaced edge endpoints.
    for (id, note, _bytes, _mtime) in &parsed {
        if !local_ids.contains(id.as_str()) {
            continue;
        }
        let source = namespace
            .node_id(id)
            .expect("accepted local ids already passed namespace validation");
        for link in &note.links {
            // Try to resolve: exact local id match first, then title lookup.
            let target_local = if local_ids.contains(link.as_str()) {
                Some(link.clone())
            } else {
                title_to_id
                    .get(link.as_str())
                    .filter(|target| local_ids.contains(target.as_str()))
                    .cloned()
            };

            match target_local {
                Some(local) => {
                    let target = namespace
                        .node_id(&local)
                        .expect("accepted local ids already passed namespace validation");
                    graph.add_edge(VaultEdge {
                        source: source.clone(),
                        target,
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
