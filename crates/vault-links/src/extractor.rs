use std::collections::HashMap;
use std::path::Path;

use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

use crate::{parser::parse_note, walker::list_markdown};

/// Result of a full vault extraction.
pub struct ExtractionResult {
    /// The populated graph (nodes + resolved edges).
    pub graph: VaultGraph,
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
                unresolved: Vec::new(),
            };
        }
    };

    // First pass: parse all notes and build a title → id lookup.
    let mut title_to_id: HashMap<String, String> = HashMap::new();
    let mut parsed: Vec<(String, crate::parser::ParsedNote, u64)> = Vec::new();

    for (id, path, mtime) in &paths {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let note = parse_note(path, &text);
        title_to_id.entry(note.title.clone()).or_insert_with(|| id.clone() as String);
        parsed.push((id.clone(), note, *mtime));
    }

    // Second pass: build graph nodes.
    let mut graph = VaultGraph::new();

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
            title: note.title.clone(),
            tags: note.tags.clone(),
            frontmatter: note.frontmatter.clone(),
            mtime: *mtime as i64,
            path: id.clone(),
            doctype: note.doctype.clone(),
            folder,
        };

        graph.add_node(VaultNode {
            id: id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });
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

    ExtractionResult { graph, unresolved }
}
