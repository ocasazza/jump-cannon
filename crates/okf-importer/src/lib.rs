//! Open Knowledge Format (OKF) v0.2 filesystem importer.
//!
//! OKF concepts are Markdown files with YAML frontmatter. This crate follows
//! the format's graph semantics instead of Obsidian conventions: tags come
//! only from the frontmatter `tags` string list, and edges come from standard
//! Markdown links plus internal `sources[].resource` provenance references.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use data_loader::{
    identity::Namespace, Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect,
    ImportError, ImportFuture, Importer, ImporterDescriptor, ImporterSchema, LoadResult,
    SearchDocument, TagHierarchySchema, Transport, WatchPlan,
};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use thiserror::Error;
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};
use walkdir::WalkDir;

/// Absolute safety ceiling. Callers may only make these values smaller.
pub const HARD_LIMITS: Limits = Limits {
    walk_entries: 2_000_000,
    files: 1_000_000,
    file_bytes: 32 * 1024 * 1024,
    total_bytes: 8 * 1024 * 1024 * 1024,
    identifier_bytes: 1024 * 1024 * 1024,
    frontmatter_nodes: 20_000_000,
    reference_occurrences: 20_000_000,
    reference_bytes: 2 * 1024 * 1024 * 1024,
    summary_values: 20_000_000,
    summary_value_bytes: 2 * 1024 * 1024 * 1024,
    nodes: 1_000_000,
    edges: 20_000_000,
    edge_endpoint_bytes: 8 * 1024 * 1024 * 1024,
    unresolved: 1_000_000,
    unresolved_bytes: 1024 * 1024 * 1024,
};

/// Defaults sized to leave headroom in graph-api's 2 GiB chart limit.
pub const DEFAULT_LIMITS: Limits = Limits {
    walk_entries: 500_000,
    files: 100_000,
    file_bytes: 8 * 1024 * 1024,
    total_bytes: 256 * 1024 * 1024,
    identifier_bytes: 64 * 1024 * 1024,
    frontmatter_nodes: 2_000_000,
    reference_occurrences: 2_000_000,
    reference_bytes: 128 * 1024 * 1024,
    summary_values: 1_000_000,
    summary_value_bytes: 64 * 1024 * 1024,
    nodes: 100_000,
    edges: 1_000_000,
    edge_endpoint_bytes: 256 * 1024 * 1024,
    unresolved: 100_000,
    unresolved_bytes: 64 * 1024 * 1024,
};

/// Resource limits applied before a graph is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// All filesystem entries visited, including non-Markdown paths.
    pub walk_entries: usize,
    pub files: usize,
    pub file_bytes: usize,
    pub total_bytes: u64,
    /// Aggregate bytes in bundle-relative Markdown paths.
    pub identifier_bytes: u64,
    /// Aggregate structured values retained from YAML frontmatter.
    pub frontmatter_nodes: usize,
    /// Aggregate body-link and provenance-reference occurrences retained.
    pub reference_occurrences: usize,
    /// Aggregate bytes in retained link and provenance destinations.
    pub reference_bytes: u64,
    /// Expanded values consumed by graph-api's filter-summary index.
    pub summary_values: usize,
    /// Aggregate bytes in expanded filter-summary values.
    pub summary_value_bytes: u64,
    pub nodes: usize,
    pub edges: usize,
    /// Aggregate bytes in materialized graph-edge endpoint IDs.
    pub edge_endpoint_bytes: u64,
    pub unresolved: usize,
    /// Aggregate bytes in unique unresolved-reference diagnostics.
    pub unresolved_bytes: u64,
}

impl Limits {
    fn validate(self) -> Result<Self, OkfError> {
        let positive = self.walk_entries > 0
            && self.files > 0
            && self.file_bytes > 0
            && self.total_bytes > 0
            && self.identifier_bytes > 0
            && self.frontmatter_nodes > 0
            && self.reference_occurrences > 0
            && self.reference_bytes > 0
            && self.summary_values > 0
            && self.summary_value_bytes > 0
            && self.nodes > 0
            && self.edges > 0
            && self.edge_endpoint_bytes > 0
            && self.unresolved > 0
            && self.unresolved_bytes > 0;
        if !positive {
            return Err(OkfError::InvalidConfiguration(
                "all OKF limits must be greater than zero".into(),
            ));
        }
        if self.walk_entries > HARD_LIMITS.walk_entries
            || self.files > HARD_LIMITS.files
            || self.file_bytes > HARD_LIMITS.file_bytes
            || self.total_bytes > HARD_LIMITS.total_bytes
            || self.identifier_bytes > HARD_LIMITS.identifier_bytes
            || self.frontmatter_nodes > HARD_LIMITS.frontmatter_nodes
            || self.reference_occurrences > HARD_LIMITS.reference_occurrences
            || self.reference_bytes > HARD_LIMITS.reference_bytes
            || self.summary_values > HARD_LIMITS.summary_values
            || self.summary_value_bytes > HARD_LIMITS.summary_value_bytes
            || self.nodes > HARD_LIMITS.nodes
            || self.edges > HARD_LIMITS.edges
            || self.edge_endpoint_bytes > HARD_LIMITS.edge_endpoint_bytes
            || self.unresolved > HARD_LIMITS.unresolved
            || self.unresolved_bytes > HARD_LIMITS.unresolved_bytes
        {
            return Err(OkfError::InvalidConfiguration(
                "an OKF limit exceeds the compiled hard ceiling".into(),
            ));
        }
        Ok(self)
    }
}

/// A configured OKF source instance.
#[derive(Debug, Clone)]
pub struct OkfImporter {
    root: PathBuf,
    source_id: String,
    namespace: Namespace,
    limits: Limits,
}

impl OkfImporter {
    pub fn new(root: impl Into<PathBuf>, source_id: impl Into<String>) -> Result<Self, OkfError> {
        Self::with_limits(root, source_id, DEFAULT_LIMITS)
    }

    pub fn with_limits(
        root: impl Into<PathBuf>,
        source_id: impl Into<String>,
        limits: Limits,
    ) -> Result<Self, OkfError> {
        let configured_root = root.into();
        let source_id = source_id.into();
        // The shared namespace-ambiguity rule (`[a-z0-9._-]{1,128}`, no `:`),
        // enforced for every importer by data-loader's identity contract.
        let namespace = Namespace::new("okf", &source_id)
            .map_err(|error| OkfError::InvalidConfiguration(error.to_string()))?;
        let root = configured_root
            .canonicalize()
            .map_err(|source| OkfError::Io {
                path: configured_root,
                source,
            })?;
        let metadata = fs::symlink_metadata(&root).map_err(|source| OkfError::Io {
            path: root.clone(),
            source,
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(OkfError::InvalidConfiguration(format!(
                "OKF root must resolve to a real directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            root,
            source_id,
            namespace,
            limits: limits.validate()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Load and validate one complete snapshot.
    pub fn load_checked(&self) -> Result<LoadResult, OkfError> {
        let concepts = self.read_concepts()?;
        self.map_concepts(concepts)
    }

    fn read_concepts(&self) -> Result<Vec<ParsedConcept>, OkfError> {
        let metadata = fs::symlink_metadata(&self.root).map_err(|source| OkfError::Io {
            path: self.root.clone(),
            source,
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(OkfError::InvalidConfiguration(format!(
                "OKF root is not a directory: {}",
                self.root.display()
            )));
        }
        let canonical_root = self.root.clone();

        let mut markdown_paths = Vec::new();
        let mut identifier_bytes = 0_u64;
        let mut walk_entries = 0_usize;
        for entry in WalkDir::new(&self.root).follow_links(false) {
            let entry = entry.map_err(|error| OkfError::Walk {
                root: self.root.clone(),
                message: error.to_string(),
            })?;
            walk_entries += 1;
            if walk_entries > self.limits.walk_entries {
                return Err(OkfError::LimitExceeded {
                    resource: "walked filesystem entries",
                    limit: self.limits.walk_entries as u64,
                });
            }
            if entry.file_type().is_file() && entry.path().extension() == Some(OsStr::new("md")) {
                let path = entry.into_path();
                let relative = relative_slash_path(&self.root, &path)?;
                identifier_bytes = identifier_bytes.saturating_add(relative.len() as u64);
                if identifier_bytes > self.limits.identifier_bytes {
                    return Err(OkfError::LimitExceeded {
                        resource: "identifier bytes",
                        limit: self.limits.identifier_bytes,
                    });
                }
                markdown_paths.push((relative, path));
                if markdown_paths.len() > self.limits.files {
                    return Err(OkfError::LimitExceeded {
                        resource: "Markdown files",
                        limit: self.limits.files as u64,
                    });
                }
            }
        }
        markdown_paths.sort_by(|left, right| left.0.cmp(&right.0));

        let mut total_bytes = 0_u64;
        let mut frontmatter_nodes = 0_usize;
        let mut reference_occurrences = 0_usize;
        let mut reference_bytes = 0_u64;
        let mut summary_values = 0_usize;
        let mut summary_value_bytes = 0_u64;
        let mut concepts = Vec::new();
        for (rel_path, path) in markdown_paths {
            if is_reserved(&rel_path) {
                continue;
            }
            if concepts.len() >= self.limits.nodes {
                return Err(OkfError::LimitExceeded {
                    resource: "concept nodes",
                    limit: self.limits.nodes as u64,
                });
            }

            let (bytes, mtime) =
                read_bounded_regular_file(&canonical_root, &rel_path, self.limits.file_bytes)?;
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            if total_bytes > self.limits.total_bytes {
                return Err(OkfError::LimitExceeded {
                    resource: "total concept bytes",
                    limit: self.limits.total_bytes,
                });
            }

            let raw = String::from_utf8(bytes).map_err(|error| OkfError::InvalidUtf8 {
                path: path.clone(),
                message: error.to_string(),
            })?;
            let concept_id = rel_path
                .strip_suffix(".md")
                .expect("Markdown paths were filtered above")
                .to_string();
            let concept = parse_concept(&concept_id, raw, mtime)?;
            let concept_frontmatter_nodes = concept.frontmatter.len().saturating_add(
                concept
                    .frontmatter
                    .values()
                    .map(json_value_nodes)
                    .sum::<usize>(),
            );
            frontmatter_nodes = frontmatter_nodes.saturating_add(concept_frontmatter_nodes);
            if frontmatter_nodes > self.limits.frontmatter_nodes {
                return Err(OkfError::LimitExceeded {
                    resource: "retained frontmatter values",
                    limit: self.limits.frontmatter_nodes as u64,
                });
            }
            reference_occurrences = reference_occurrences
                .saturating_add(concept.links.len())
                .saturating_add(concept.source_resources.len());
            if reference_occurrences > self.limits.reference_occurrences {
                return Err(OkfError::LimitExceeded {
                    resource: "reference occurrences",
                    limit: self.limits.reference_occurrences as u64,
                });
            }
            reference_bytes = reference_bytes.saturating_add(
                concept
                    .links
                    .iter()
                    .chain(&concept.source_resources)
                    .map(|reference| reference.len() as u64)
                    .sum::<u64>(),
            );
            if reference_bytes > self.limits.reference_bytes {
                return Err(OkfError::LimitExceeded {
                    resource: "reference bytes",
                    limit: self.limits.reference_bytes,
                });
            }
            account_summary_values(
                &concept,
                &mut summary_values,
                &mut summary_value_bytes,
                self.limits,
            )?;
            concepts.push(concept);
        }
        Ok(concepts)
    }

    fn map_concepts(&self, concepts: Vec<ParsedConcept>) -> Result<LoadResult, OkfError> {
        let concept_indices: HashMap<String, usize> = concepts
            .iter()
            .enumerate()
            .map(|(index, concept)| (concept.id.clone(), index))
            .collect();
        let mut graph = VaultGraph::new();
        let mut relations = Vec::with_capacity(concepts.len());
        let mut search_documents = Vec::with_capacity(concepts.len());

        for concept in concepts {
            let ParsedConcept {
                id,
                title,
                concept_type,
                tags,
                frontmatter,
                source_resources,
                path_references,
                links,
                body,
                mtime,
            } = concept;
            let node_id = self.node_id(&id)?;
            let folder = concept_folder(&id);
            search_documents.push(okf_search_document(
                &node_id,
                &id,
                &title,
                &concept_type,
                &tags,
                &folder,
                &body,
                &frontmatter,
                &source_resources,
            ));
            graph
                .try_add_node(VaultNode {
                    id: node_id,
                    meta: NodeMeta {
                        source_id: self.source_id.clone(),
                        title,
                        tags,
                        frontmatter,
                        mtime,
                        path: id.clone(),
                        doctype: Some(concept_type),
                        folder,
                        content_type: Some("text/markdown".into()),
                        // OKF pages are readable through the importer's own
                        // `read_body` trait method (which renders the body
                        // after YAML frontmatter, or the `description`
                        // frontmatter string when the body is empty — the
                        // lavender-ingest corpus stores its page content
                        // in `description`). Writes stay on the deployment
                        // default; OKF is read-only.
                        content_readable: true,
                        content_writable: false,
                    },
                    metrics: NodeMetrics::default(),
                    x: 0.0,
                    y: 0.0,
                })
                .map_err(|error| OkfError::Graph(error.to_string()))?;
            relations.push((id, links, path_references));
        }

        let mut edge_ids = HashSet::new();
        let mut edge_endpoint_bytes = 0_u64;
        let mut unresolved_ids = HashSet::new();
        let mut unresolved_bytes = 0_u64;
        for (source_index, (concept_id, links, path_references)) in relations.iter().enumerate() {
            for destination in links {
                let Some(target) = resolve_body_link(concept_id, destination) else {
                    continue;
                };
                let Some(&target_index) = concept_indices.get(&target) else {
                    if !unresolved_ids.insert((source_index, destination.clone())) {
                        continue;
                    }
                    if unresolved_ids.len() > self.limits.unresolved {
                        return Err(OkfError::LimitExceeded {
                            resource: "unresolved references",
                            limit: self.limits.unresolved as u64,
                        });
                    }
                    let diagnostic_bytes =
                        concept_id.len() as u64 + " -> ".len() as u64 + destination.len() as u64;
                    let next_bytes = unresolved_bytes.saturating_add(diagnostic_bytes);
                    if next_bytes > self.limits.unresolved_bytes {
                        return Err(OkfError::LimitExceeded {
                            resource: "unresolved diagnostic bytes",
                            limit: self.limits.unresolved_bytes,
                        });
                    }
                    unresolved_bytes = next_bytes;
                    continue;
                };
                self.push_edge(
                    &mut graph,
                    &mut edge_ids,
                    &mut edge_endpoint_bytes,
                    source_index,
                    target_index,
                    (concept_id, &target),
                )?;
            }

            for resource in path_references {
                let Some(target) = resolve_source_resource(concept_id, resource) else {
                    continue;
                };
                // `resource` may be an external data object or a free-form
                // scope descriptor, or an OKF §6.2 path-valued field
                // (`resource`, `computation`, `executor.resource`,
                // `attester.resource`). It is provenance metadata unless it
                // resolves to another concept in this bundle.
                if let Some(&target_index) = concept_indices.get(&target) {
                    self.push_edge(
                        &mut graph,
                        &mut edge_ids,
                        &mut edge_endpoint_bytes,
                        source_index,
                        target_index,
                        (concept_id, &target),
                    )?;
                }
            }
        }

        let mut unresolved: Vec<String> = unresolved_ids
            .into_iter()
            .map(|(source_index, destination)| {
                format!("{} -> {destination}", relations[source_index].0)
            })
            .collect();
        unresolved.sort();
        graph
            .validate()
            .map_err(|error| OkfError::Graph(error.to_string()))?;
        Ok(LoadResult {
            graph,
            search_documents,
            unresolved,
        })
    }

    fn push_edge(
        &self,
        graph: &mut VaultGraph,
        edge_ids: &mut HashSet<(usize, usize)>,
        edge_endpoint_bytes: &mut u64,
        source_index: usize,
        target_index: usize,
        endpoints: (&str, &str),
    ) -> Result<(), OkfError> {
        let (source, target) = endpoints;
        if source_index == target_index {
            return Ok(());
        }
        if !edge_ids.insert((source_index, target_index)) {
            return Ok(());
        }
        if graph.edges.len() >= self.limits.edges {
            return Err(OkfError::LimitExceeded {
                resource: "graph edges",
                limit: self.limits.edges as u64,
            });
        }
        let new_endpoint_bytes = self.node_id_len(source) as u64 + self.node_id_len(target) as u64;
        let next_endpoint_bytes = edge_endpoint_bytes.saturating_add(new_endpoint_bytes);
        if next_endpoint_bytes > self.limits.edge_endpoint_bytes {
            return Err(OkfError::LimitExceeded {
                resource: "edge endpoint bytes",
                limit: self.limits.edge_endpoint_bytes,
            });
        }
        *edge_endpoint_bytes = next_endpoint_bytes;
        let source = self.node_id(source)?;
        let target = self.node_id(target)?;
        graph.add_edge(VaultEdge { source, target });
        Ok(())
    }

    fn node_id(&self, concept_id: &str) -> Result<String, OkfError> {
        self.namespace
            .node_id(concept_id)
            .map_err(|error| OkfError::Graph(error.to_string()))
    }

    fn node_id_len(&self, concept_id: &str) -> usize {
        self.namespace.prefix().len() + concept_id.len()
    }
}

impl Importer for OkfImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let scope = self.root.to_string_lossy().into_owned();
        ImporterDescriptor::new(
            "okf",
            "Open Knowledge Format",
            "0.2",
            vec![
                Capability::new(Effect::Read, Transport::Filesystem, scope.clone()),
                Capability::new(Effect::Watch, Transport::Filesystem, scope.clone()),
                // Per-node body reads are served by this importer's own
                // `Importer::read_body` implementation; the capability is
                // scoped to its own root so cross-source reads stay denied.
                Capability::new(Effect::ContentRead, Transport::Filesystem, scope),
            ],
            okf_schema(),
        )
        .with_watch(WatchPlan::Filesystem {
            root: self.root.clone(),
        })
    }
    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        let importer = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || importer.load_checked())
                .await
                .map_err(|error| ImportError::SourceRead {
                    origin: self.root.display().to_string(),
                    message: format!("OKF importer worker failed: {error}"),
                })?
                .map_err(|error| ImportError::Decode {
                    origin: self.root.display().to_string(),
                    message: error.to_string(),
                })
        })
    }

    /// Read a node's body for the document-panel preview. Most OKF pages
    /// store their content in the `description` frontmatter string (the
    /// lavender-ingest auto-imported corpus does this; manually-authored
    /// OKF bundles sometimes have it in the markdown body). We return the
    /// non-empty one, so the rendered preview reflects what's actually on
    /// disk. Returns `None` when the file doesn't exist or can't be
    /// parsed as UTF-8 — the host treats that as "no body" rather than an
    /// error, matching `read_body`'s soft-fallback contract.
    fn read_body(&self, path: &str) -> Option<String> {
        let file_path = self.root.join(format!("{path}.md"));
        let raw = std::fs::read_to_string(&file_path).ok()?;
        let (frontmatter_raw, body) = split_frontmatter(&raw)?;
        let yaml: serde_yaml::Value =
            serde_saphyr::from_str_with_options(frontmatter_raw, okf_frontmatter_options())
                .ok()?;
        let description = yaml
            .as_mapping()
            .and_then(|map| map.get(serde_yaml::Value::String("description".into())))
            .and_then(serde_yaml::Value::as_str);
        let body_trimmed = body.trim();
        if !body_trimmed.is_empty() {
            Some(body_trimmed.to_string())
        } else {
            description.map(str::to_string)
        }
    }
}


#[derive(Debug)]
struct ParsedConcept {
    id: String,
    title: String,
    concept_type: String,
    tags: Vec<String>,
    frontmatter: HashMap<String, serde_json::Value>,
    /// Every `sources[].resource` string (OKF §5.1) — drives the
    /// `source_resources` discovery projection.
    source_resources: Vec<String>,
    /// Every OKF §6.2 path-valued field string (`sources[].resource`,
    /// `resource`, `computation`, `executor.resource`,
    /// `attester.resource`). Used for graph-edge creation: any value that
    /// resolves to another concept in the bundle becomes a directed
    /// `relationship` edge from the source concept to the target concept.
    /// External URLs and scope descriptors resolve to `None` and are
    /// silently dropped, matching the spec's permissive conformance.
    path_references: Vec<String>,
    /// Standard CommonMark body link destinations (OKF §6.1).
    links: Vec<String>,
    body: String,
    mtime: i64,
}

fn parse_concept(id: &str, raw: String, mtime: i64) -> Result<ParsedConcept, OkfError> {
    let (frontmatter_raw, body) =
        split_frontmatter(&raw).ok_or_else(|| OkfError::InvalidConcept {
            concept: id.into(),
            message: "missing or unterminated YAML frontmatter".into(),
        })?;
    let yaml: serde_yaml::Value =
        serde_saphyr::from_str_with_options(frontmatter_raw, okf_frontmatter_options())
            .map_err(|error| OkfError::InvalidConcept {
                concept: id.into(),
                message: format!("invalid YAML frontmatter: {error}"),
            })?;
    let mapping = yaml.as_mapping().ok_or_else(|| OkfError::InvalidConcept {
        concept: id.into(),
        message: "frontmatter must be a YAML mapping".into(),
    })?;

    let concept_type = yaml_string(mapping, "type")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| OkfError::InvalidConcept {
            concept: id.into(),
            message: "frontmatter requires a non-empty string `type`".into(),
        })?
        .to_string();
    if let Some(status) = mapping.get(serde_yaml::Value::String("status".into())) {
        let Some(status) = status.as_str() else {
            return Err(OkfError::InvalidConcept {
                concept: id.into(),
                message: "frontmatter `status` must be draft, stable, or deprecated".into(),
            });
        };
        if !matches!(status, "draft" | "stable" | "deprecated") {
            return Err(OkfError::InvalidConcept {
                concept: id.into(),
                message: "frontmatter `status` must be draft, stable, or deprecated".into(),
            });
        }
    }
    let title = yaml_string(mapping, "title")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id).to_string());
    let mut tags = match mapping.get(serde_yaml::Value::String("tags".into())) {
        None => Vec::new(),
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| OkfError::InvalidConcept {
                        concept: id.into(),
                        message: "frontmatter `tags` must be a YAML list of non-empty strings"
                            .into(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(OkfError::InvalidConcept {
                concept: id.into(),
                message: "frontmatter `tags` must be a YAML list of strings".into(),
            })
        }
    };
    tags.sort();
    tags.dedup();
    let mut source_resources = extract_source_resources(mapping);
    source_resources.sort();
    source_resources.dedup();
    let mut path_references = extract_path_references(mapping);
    path_references.sort();
    path_references.dedup();
    let mut links = markdown_links(body);
    links.sort();
    links.dedup();

    let mut frontmatter = HashMap::new();
    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            // Unknown fields cannot affect the graph mapping, and OKF readers
            // must not reject extensions they do not understand.
            continue;
        };
        let value = serde_json::to_value(value).unwrap_or_else(|_| {
            serde_json::Value::String(
                serde_yaml::to_string(value)
                    .unwrap_or_else(|_| "<unrepresentable YAML extension>".into()),
            )
        });
        frontmatter.insert(key.to_string(), value);
    }

    Ok(ParsedConcept {
        id: id.into(),
        title,
        concept_type,
        tags,
        frontmatter,
        source_resources,
        path_references,
        links,
        body: body.to_string(),
        mtime,
    })
}
fn yaml_string<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a str> {
    mapping
        .get(serde_yaml::Value::String(key.into()))
        .and_then(serde_yaml::Value::as_str)
}

fn extract_source_resources(mapping: &serde_yaml::Mapping) -> Vec<String> {
    mapping
        .get(serde_yaml::Value::String("sources".into()))
        .and_then(serde_yaml::Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(serde_yaml::Value::as_mapping)
        .filter_map(|source| yaml_string(source, "resource"))
        .filter(|resource| !resource.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// OKF §6.2 link-extraction grammar: every path-valued field the spec
/// permits a concept to name. Each rule resolves a YAML frontmatter path
/// into candidate concept references; the importer's link resolver then
/// turns each one into a directed `relationship` edge when it matches
/// another concept in this bundle. Centralized here so adding a new
/// OKF path-valued field is a one-line schema change.
const OKF_LINK_RULES: &[(&str, &str)] = &[
    ("sources[*].resource", "relationship"),
    ("resource", "relationship"),
    ("computation", "relationship"),
    ("executor.resource", "relationship"),
    ("attester.resource", "relationship"),
];

/// Collect every OKF §6.2 path-valued field string the concept declares,
/// driven by the declarative [`OKF_LINK_RULES`] grammar. Each rule's path
/// is walked against the parsed frontmatter mapping; every leaf string is
/// emitted as a candidate target for the existing link resolver (which
/// decides whether the value resolves to another concept in the bundle).
fn extract_path_references(mapping: &serde_yaml::Mapping) -> Vec<String> {
    let value = serde_yaml::Value::Mapping(mapping.clone());
    let mut paths = Vec::new();
    for (path, _kind) in OKF_LINK_RULES {
        for leaf in yaml_path_strings(&value, path) {
            let trimmed = leaf.trim();
            if !trimmed.is_empty() {
                paths.push(trimmed.to_string());
            }
        }
    }
    paths
}

/// Walk the given dotted YAML path and collect every leaf string. Segments
/// ending in `[*]` iterate the field as a sequence; non-array segments
/// recurse into YAML mappings by key; landing on a sequence without a
/// further segment collects every scalar element. The companion to the
/// schema-level [`data_loader::LinkRule`] grammar — OKF walks its own YAML
/// directly because `serde_yaml` lives only in this crate today, but the
/// path syntax is identical so other importers can adopt the same shape.
fn yaml_path_strings(value: &serde_yaml::Value, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    walk_yaml_path(value, path, &mut out);
    out
}

fn walk_yaml_path(value: &serde_yaml::Value, path: &str, out: &mut Vec<String>) {
    let (segment, rest) = match path.split_once('.') {
        Some((s, r)) => (s, r),
        None => (path, ""),
    };

    if let Some(field) = segment.strip_suffix("[*]") {
        // Resolve the named field (or treat the current value as a sequence
        // when the field segment is empty), then iterate. Each iteration
        // either recurses on the remaining path or treats the item as a
        // leaf scalar collection point.
        let sequence = if field.is_empty() {
            value.as_sequence()
        } else {
            yaml_mapping_get(value, field).and_then(|v| v.as_sequence())
        };
        let Some(seq) = sequence else {
            return;
        };
        for item in seq {
            if rest.is_empty() {
                collect_yaml_scalars(item, out);
            } else {
                walk_yaml_path(item, rest, out);
            }
        }
        return;
    }

    if rest.is_empty() {
        // Single segment at the leaf. Two cases:
        // (a) The current value is a mapping and `segment` is one of its keys
        //     (e.g. path "resource" against {"resource": "x"}).
        // (b) `segment` names the current value itself (a scalar/sequence).
        if let Some(child) = yaml_mapping_get(value, segment) {
            collect_yaml_scalars(child, out);
        } else {
            collect_yaml_scalars(value, out);
        }
        return;
    }

    if let Some(child) = yaml_mapping_get(value, segment) {
        walk_yaml_path(child, rest, out);
    }
}


fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()
        .and_then(|map| map.get(serde_yaml::Value::String(key.into())))
}

fn collect_yaml_scalars(value: &serde_yaml::Value, out: &mut Vec<String>) {
    match value {
        serde_yaml::Value::String(s) => out.push(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                collect_yaml_scalars(item, out);
            }
        }
        serde_yaml::Value::Mapping(_)
        | serde_yaml::Value::Null
        | serde_yaml::Value::Bool(_)
        | serde_yaml::Value::Number(_) => {}
        _ => {}
    }
}


fn okf_schema() -> ImporterSchema {
    ImporterSchema::new(
        "okf",
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                .searchable(4)
                .snippet(),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("type", DiscoveryFieldType::Keyword, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("folder", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("body", DiscoveryFieldType::Text, true)
                .searchable(1)
                .snippet(),
            DiscoveryField::new("description", DiscoveryFieldType::Text, false)
                .searchable(3)
                .snippet(),
            DiscoveryField::new("resource", DiscoveryFieldType::Url, false).searchable(2),
            DiscoveryField::new("status", DiscoveryFieldType::Keyword, true)
                .searchable(2)
                .facetable()
                .with_default("stable"),
            DiscoveryField::new("stale_after", DiscoveryFieldType::Date, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("trust_tier", DiscoveryFieldType::Keyword, true)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("generated_by", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("generated_at", DiscoveryFieldType::Date, false).searchable(1),
            DiscoveryField::new("verified_by", DiscoveryFieldType::KeywordList, false)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("source_resources", DiscoveryFieldType::KeywordList, false)
                .searchable(2),
            DiscoveryField::new("source_titles", DiscoveryFieldType::KeywordList, false)
                .searchable(2),
            DiscoveryField::new("source_authors", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("runtime", DiscoveryFieldType::Keyword, false)
                .searchable(2)
                .facetable(),
        ],
        vec![EdgeTypeSchema::directed(
            "relationship",
            "Standard Markdown concept link or internal provenance reference",
        )],
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["text/markdown"])
    .with_link_rules(
        OKF_LINK_RULES
            .iter()
            .map(|(path, kind)| data_loader::LinkRule {
                path: (*path).to_string(),
                kind: (*kind).to_string(),
            }),
    )
    .with_content(data_loader::ContentSchema {
        readable: true,
        writable: false,
        media_types: vec!["text/markdown".to_string()],
    })
}

#[allow(clippy::too_many_arguments)]
fn okf_search_document(
    node_id: &str,
    path: &str,
    title: &str,
    concept_type: &str,
    tags: &[String],
    folder: &str,
    body: &str,
    frontmatter: &HashMap<String, serde_json::Value>,
    source_resources: &[String],
) -> SearchDocument {
    let mut document = SearchDocument::new(node_id)
        .with("id", node_id)
        .with("title", title)
        .with("tags", serde_json::json!(tags))
        .with("path", path)
        .with("type", concept_type)
        .with("body", body)
        .with(
            "status",
            frontmatter
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("stable"),
        )
        .with("trust_tier", okf_trust_tier(frontmatter));
    if !folder.is_empty() {
        document.insert("folder", folder);
    }
    for (key, output_key) in [
        ("description", "description"),
        ("resource", "resource"),
        ("stale_after", "stale_after"),
        ("runtime", "runtime"),
    ] {
        if let Some(value) = frontmatter
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            document.insert(output_key, value);
        }
    }
    if let Some(generated) = frontmatter
        .get("generated")
        .and_then(serde_json::Value::as_object)
    {
        if let Some(actor) = generated
            .get("by")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            document.insert("generated_by", actor);
        }
        if let Some(at) = generated
            .get("at")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            document.insert("generated_at", at);
        }
    }
    let verified_by = okf_verified_actors(frontmatter);
    if !verified_by.is_empty() {
        document.insert("verified_by", serde_json::json!(verified_by));
    }
    if !source_resources.is_empty() {
        document.insert("source_resources", serde_json::json!(source_resources));
    }
    let (source_titles, source_authors) = okf_source_discovery(frontmatter);
    if !source_titles.is_empty() {
        document.insert("source_titles", serde_json::json!(source_titles));
    }
    if !source_authors.is_empty() {
        document.insert("source_authors", serde_json::json!(source_authors));
    }
    document
}

fn okf_verified_actors(frontmatter: &HashMap<String, serde_json::Value>) -> Vec<String> {
    let Some(verified) = frontmatter.get("verified") else {
        return Vec::new();
    };
    let entries: Vec<&serde_json::Map<String, serde_json::Value>> = match verified {
        serde_json::Value::Object(entry) => vec![entry],
        serde_json::Value::Array(entries) => entries
            .iter()
            .filter_map(serde_json::Value::as_object)
            .collect(),
        _ => Vec::new(),
    };
    let mut actors = entries
        .into_iter()
        .filter_map(|entry| entry.get("by").and_then(serde_json::Value::as_str))
        .filter(|actor| !actor.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    actors.sort();
    actors.dedup();
    actors
}

fn okf_trust_tier(frontmatter: &HashMap<String, serde_json::Value>) -> &'static str {
    let actors = okf_verified_actors(frontmatter);
    if actors.iter().any(|actor| actor.starts_with("human:")) {
        "human-reviewed"
    } else if actors.is_empty() {
        "unverified"
    } else {
        "machine-confirmed"
    }
}

fn okf_source_discovery(
    frontmatter: &HashMap<String, serde_json::Value>,
) -> (Vec<String>, Vec<String>) {
    let mut titles = Vec::new();
    let mut authors = Vec::new();
    for source in frontmatter
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
    {
        if let Some(title) = source
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            titles.push(title.to_string());
        }
        if let Some(author) = source
            .get("author")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            authors.push(author.to_string());
        }
    }
    titles.sort();
    titles.dedup();
    authors.sort();
    authors.dedup();
    (titles, authors)
}

/// Centralized [`serde_saphyr`] parse options for OKF frontmatter. Used by
/// both [`parse_concept`] (full ingest) and [`OkfImporter::read_body`]
/// (per-node body fetch for the document panel preview) so the two paths
/// agree on what counts as a valid mapping.
fn okf_frontmatter_options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_events: 500_000,
            max_aliases: 256,
            max_anchors: 64,
            max_depth: 64,
            max_documents: 1,
            max_nodes: 200_000,
            max_total_scalar_bytes: 8 * 1024 * 1024,
            max_total_comment_bytes: 8 * 1024 * 1024,
            max_merge_keys: 64,
        },
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: 50_000,
            max_replay_stack_depth: 16,
            max_alias_expansions_per_anchor: 32,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Merge,
        with_snippet: false,
        crop_radius: 0,
    }
}

 fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let line_without_ending = line.trim_end_matches(['\r', '\n']);
        if line_without_ending == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    if rest[offset..].trim_end_matches('\r') == "---" {
        return Some((&rest[..offset], ""));
    }
    None
}

fn markdown_links(body: &str) -> Vec<String> {
    #[derive(Debug)]
    struct PendingLink {
        destination: String,
        contains_image: bool,
    }

    let mut pending: Option<PendingLink> = None;
    let mut links = Vec::new();
    for event in Parser::new_ext(body, Options::all()) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                pending = Some(PendingLink {
                    destination: dest_url.into_string(),
                    contains_image: false,
                });
            }
            Event::Start(Tag::Image { .. }) => {
                if let Some(link) = &mut pending {
                    link.contains_image = true;
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = pending.take() {
                    if !link.contains_image {
                        links.push(link.destination);
                    }
                }
            }
            _ => {}
        }
    }
    links
}

fn resolve_body_link(source_id: &str, raw: &str) -> Option<String> {
    resolve_reference(source_id, raw, ReferenceBase::Document)
}

fn resolve_source_resource(source_id: &str, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let path = trimmed.split('#').next()?;
    let decoded = percent_decode_str(path).decode_utf8().ok()?;
    let base = if decoded.starts_with("./") || decoded.starts_with("../") {
        ReferenceBase::Document
    } else {
        ReferenceBase::Bundle
    };
    resolve_reference(source_id, trimmed, base)
}

#[derive(Clone, Copy)]
enum ReferenceBase {
    Document,
    Bundle,
}

fn resolve_reference(source_id: &str, raw: &str, base: ReferenceBase) -> Option<String> {
    let without_fragment = raw.split('#').next()?.trim();
    if without_fragment.is_empty()
        || without_fragment.contains('?')
        || without_fragment.starts_with("//")
        || has_uri_scheme(without_fragment)
    {
        return None;
    }
    let decoded = percent_decode_str(without_fragment).decode_utf8().ok()?;
    if decoded.contains('\0')
        || decoded.contains('\\')
        || decoded.starts_with("//")
        || has_uri_scheme(&decoded)
    {
        return None;
    }
    let root_relative = decoded.starts_with('/');
    let mut segments = Vec::new();
    if !root_relative && matches!(base, ReferenceBase::Document) {
        segments.extend(
            source_id
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent)
                .split('/'),
        );
        segments.retain(|segment| !segment.is_empty());
    }

    for segment in decoded.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value),
        }
    }
    let path = segments.join("/");
    path.strip_suffix(".md").map(str::to_string)
}

fn has_uri_scheme(value: &str) -> bool {
    let before_slash = value.split('/').next().unwrap_or(value);
    before_slash.find(':').is_some_and(|index| {
        let scheme = &before_slash[..index];
        scheme
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && scheme.bytes().skip(1).all(is_scheme_byte)
    })
}

fn is_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

fn json_value_nodes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => 1_usize.saturating_add(
            values
                .iter()
                .map(json_value_nodes)
                .fold(0_usize, usize::saturating_add),
        ),
        serde_json::Value::Object(values) => 1_usize.saturating_add(
            values.len().saturating_add(
                values
                    .values()
                    .map(json_value_nodes)
                    .fold(0_usize, usize::saturating_add),
            ),
        ),
        _ => 1,
    }
}

fn account_summary_values(
    concept: &ParsedConcept,
    value_count: &mut usize,
    value_bytes: &mut u64,
    limits: Limits,
) -> Result<(), OkfError> {
    // Keep this list synchronized with graph-api's build_meta_summary_bytes.
    // This output-side budget prevents accepted extension fields from
    // expanding into an unbounded filter index before snapshot publication.
    fn account(
        value: &str,
        value_count: &mut usize,
        value_bytes: &mut u64,
        limits: Limits,
    ) -> Result<(), OkfError> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(());
        }
        *value_count = value_count.saturating_add(1);
        if *value_count > limits.summary_values {
            return Err(OkfError::LimitExceeded {
                resource: "filter-summary values",
                limit: limits.summary_values as u64,
            });
        }
        *value_bytes = value_bytes.saturating_add(value.len() as u64);
        if *value_bytes > limits.summary_value_bytes {
            return Err(OkfError::LimitExceeded {
                resource: "filter-summary value bytes",
                limit: limits.summary_value_bytes,
            });
        }
        Ok(())
    }

    fn account_json_strings(
        value: &serde_json::Value,
        split_commas: bool,
        value_count: &mut usize,
        value_bytes: &mut u64,
        limits: Limits,
    ) -> Result<(), OkfError> {
        let strings = match value {
            serde_json::Value::String(value) => std::slice::from_ref(value),
            serde_json::Value::Array(values) => {
                for value in values.iter().filter_map(serde_json::Value::as_str) {
                    if split_commas {
                        for part in value.split(',') {
                            account(part, value_count, value_bytes, limits)?;
                        }
                    } else {
                        account(value, value_count, value_bytes, limits)?;
                    }
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        for value in strings {
            if split_commas {
                for part in value.split(',') {
                    account(part, value_count, value_bytes, limits)?;
                }
            } else {
                account(value, value_count, value_bytes, limits)?;
            }
        }
        Ok(())
    }

    for tag in &concept.tags {
        account(tag, value_count, value_bytes, limits)?;
    }
    account(&concept.concept_type, value_count, value_bytes, limits)?;
    account(
        &concept_folder(&concept.id),
        value_count,
        value_bytes,
        limits,
    )?;
    if let Some(serde_json::Value::String(status)) = concept.frontmatter.get("status") {
        account(status, value_count, value_bytes, limits)?;
    }
    for (field, split_commas) in [
        ("authors", true),
        ("entities", false),
        ("key_topics", false),
        ("related", false),
    ] {
        if let Some(value) = concept.frontmatter.get(field) {
            account_json_strings(value, split_commas, value_count, value_bytes, limits)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_bounded_regular_file(
    root: &Path,
    relative: &str,
    max_bytes: usize,
) -> Result<(Vec<u8>, i64), OkfError> {
    use rustix::fs::{openat, Mode, OFlags, CWD};

    let display_path = root.join(relative);
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    // NONBLOCK prevents a raced replacement with a FIFO from stalling an
    // import before the regular-file check below can reject it.
    let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut directory =
        openat(CWD, root, directory_flags, Mode::empty()).map_err(|error| OkfError::Io {
            path: root.into(),
            source: error.into(),
        })?;
    let mut components = Path::new(relative).components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            return Err(OkfError::PathEscape { path: display_path });
        };
        if components.peek().is_some() {
            directory =
                openat(&directory, name, directory_flags, Mode::empty()).map_err(|error| {
                    OkfError::Io {
                        path: display_path.clone(),
                        source: error.into(),
                    }
                })?;
        } else {
            let descriptor =
                openat(&directory, name, file_flags, Mode::empty()).map_err(|error| {
                    OkfError::Io {
                        path: display_path.clone(),
                        source: error.into(),
                    }
                })?;
            let file: fs::File = descriptor.into();
            return read_open_file(file, display_path, max_bytes);
        }
    }
    Err(OkfError::PathEscape { path: display_path })
}

#[cfg(not(unix))]
fn read_bounded_regular_file(
    root: &Path,
    relative: &str,
    _max_bytes: usize,
) -> Result<(Vec<u8>, i64), OkfError> {
    Err(OkfError::InvalidConfiguration(format!(
        "OKF filesystem imports require Unix no-follow openat support (cannot safely open {})",
        root.join(relative).display()
    )))
}

fn read_open_file(
    file: fs::File,
    path: PathBuf,
    max_bytes: usize,
) -> Result<(Vec<u8>, i64), OkfError> {
    let metadata = file.metadata().map_err(|source| OkfError::Io {
        path: path.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(OkfError::InvalidConcept {
            concept: path.display().to_string(),
            message: "concept path is not a regular file".into(),
        });
    }
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64);
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| OkfError::Io {
            path: path.clone(),
            source,
        })?;
    if bytes.len() > max_bytes {
        return Err(OkfError::LimitExceeded {
            resource: "bytes in one concept file",
            limit: max_bytes as u64,
        });
    }
    Ok((bytes, mtime))
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String, OkfError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| OkfError::PathEscape { path: path.into() })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(value) => {
                let value = value
                    .to_str()
                    .ok_or_else(|| OkfError::NonUtf8Path { path: path.into() })?;
                parts.push(value);
            }
            _ => return Err(OkfError::PathEscape { path: path.into() }),
        }
    }
    Ok(parts.join("/"))
}

fn is_reserved(relative_path: &str) -> bool {
    matches!(
        relative_path.rsplit('/').next(),
        Some("index.md" | "log.md")
    )
}

fn concept_folder(concept_id: &str) -> String {
    concept_id
        .rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_string())
}

#[derive(Debug, Error)]
pub enum OkfError {
    #[error("invalid OKF configuration: {0}")]
    InvalidConfiguration(String),
    #[error("failed to walk OKF root {root}: {message}")]
    Walk { root: PathBuf, message: String },
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("OKF path escapes the source root: {path}")]
    PathEscape { path: PathBuf },
    #[error("OKF path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    #[error("OKF file is not valid UTF-8 at {path}: {message}")]
    InvalidUtf8 { path: PathBuf, message: String },
    #[error("invalid OKF concept {concept:?}: {message}")]
    InvalidConcept { concept: String, message: String },
    #[error("OKF {resource} exceed the limit of {limit}")]
    LimitExceeded { resource: &'static str, limit: u64 },
    #[error("invalid OKF graph: {0}")]
    Graph(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn concept(concept_type: &str, body: &str) -> String {
        format!("---\ntype: {concept_type}\n---\n{body}")
    }

    fn load(root: &Path) -> LoadResult {
        OkfImporter::new(root, "fixture")
            .unwrap()
            .load_checked()
            .unwrap()
    }

    fn edges(result: &LoadResult) -> BTreeSet<(String, String)> {
        result
            .graph
            .edges
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone()))
            .collect()
    }

    #[test]
    fn v02_tags_and_commonmark_links_map_to_the_graph() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "index.md",
            "---\nokf_version: \"0.2\"\n---\n# Bundle",
        );
        write(
            fixture.path(),
            "concepts/alpha.md",
            r##"---
type: metric
title: Alpha
tags:
  - Kubernetes
  - team blue
extension:
  owner: platform
sources:
  - resource: /concepts/root.md
---
[Beta](./beta.md#api)
[Beta again][beta]
[Root](/concepts/root.md "title")
[external](https://example.com/file.md)
[broken](/missing.md)
![image](./image-target.md)
[![linked image](./picture.png)](./image-target.md)
`[code](./image-target.md)`

[beta]: ./beta.md "reference title"
"##,
        );
        write(
            fixture.path(),
            "concepts/beta.md",
            "---\ntype: table\ntags: [finance, revenue]\n---\n",
        );
        write(fixture.path(), "concepts/root.md", &concept("policy", ""));
        write(
            fixture.path(),
            "concepts/image-target.md",
            &concept("image", ""),
        );

        let result = load(fixture.path());

        assert_eq!(result.graph.node_count(), 4);
        assert_eq!(
            edges(&result),
            BTreeSet::from([
                (
                    "okf:fixture:concepts/alpha".into(),
                    "okf:fixture:concepts/beta".into(),
                ),
                (
                    "okf:fixture:concepts/alpha".into(),
                    "okf:fixture:concepts/root".into(),
                ),
            ])
        );
        assert_eq!(result.unresolved, ["concepts/alpha -> /missing.md"]);

        let alpha = &result.graph.nodes["okf:fixture:concepts/alpha"].meta;
        assert_eq!(alpha.tags, ["Kubernetes", "team blue"]);
        assert_eq!(alpha.doctype.as_deref(), Some("metric"));
        assert_eq!(alpha.path, "concepts/alpha");
        assert_eq!(alpha.folder, "concepts");
        assert_eq!(alpha.source_id, "fixture");
        // OKF nodes are now readable via the importer's own `read_body`
        // (which falls back to the `description` frontmatter when the
        // markdown body is empty — see `Importer::read_body`).
        assert!(alpha.content_readable);
        assert!(!alpha.content_writable);
        assert_eq!(alpha.frontmatter["extension"]["owner"], "platform");
        assert_eq!(
            result.graph.nodes["okf:fixture:concepts/beta"].meta.tags,
            ["finance", "revenue"]
        );
    }

    #[test]
    fn okf_schema_exposes_search_keys_and_documents_with_stable_default() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "metrics/revenue.md",
            r#"---
type: metric
title: Revenue
tags: [finance, canonical]
description: Net recognized revenue
verified:
  - by: human:reviewer
sources:
  - resource: https://example.com/source
    title: Finance handbook
    author: Finance team
---
Revenue body token.
"#,
        );
        let importer = OkfImporter::new(fixture.path(), "fixture").unwrap();
        let descriptor = importer.descriptor();
        let result = importer.load_checked().unwrap();

        descriptor.validate().unwrap();
        descriptor.schema.validate_result(&result).unwrap();
        let searchable = descriptor
            .schema
            .searchable_fields()
            .map(|field| field.key.as_str())
            .collect::<BTreeSet<_>>();
        for required in [
            "id",
            "title",
            "tags",
            "type",
            "body",
            "description",
            "status",
            "trust_tier",
            "verified_by",
            "source_resources",
            "source_titles",
            "source_authors",
        ] {
            assert!(
                searchable.contains(required),
                "missing search key {required}"
            );
        }

        let document = &result.search_documents[0];
        assert_eq!(document.node_id, "okf:fixture:metrics/revenue");
        assert_eq!(document.fields["status"], "stable");
        assert_eq!(document.fields["trust_tier"], "human-reviewed");
        assert_eq!(
            document.fields["tags"],
            serde_json::json!(["canonical", "finance"])
        );
        assert_eq!(
            document.fields["verified_by"],
            serde_json::json!(["human:reviewer"])
        );
        assert_eq!(
            document.fields["source_titles"],
            serde_json::json!(["Finance handbook"])
        );
        assert_eq!(
            document.fields["source_authors"],
            serde_json::json!(["Finance team"])
        );
        assert!(document.fields["body"]
            .as_str()
            .unwrap()
            .contains("Revenue body token"));
    }

    #[test]
    fn malformed_okf_tags_are_rejected_instead_of_silently_dropped() {
        for (name, tags) in [
            ("scalar", "tags: finance, revenue"),
            ("non-string", "tags: [finance, 42]"),
            ("empty", "tags: [finance, '']"),
        ] {
            let fixture = TempDir::new().unwrap();
            write(
                fixture.path(),
                "bad.md",
                &format!("---\ntype: metric\n{tags}\n---\n"),
            );

            let error = OkfImporter::new(fixture.path(), "fixture")
                .unwrap()
                .load_checked()
                .unwrap_err();
            assert!(
                matches!(error, OkfError::InvalidConcept { .. }),
                "{name}: {error}"
            );
            assert!(
                error.to_string().contains("`tags` must be a YAML list"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn provenance_resources_use_bundle_paths_and_explicit_relative_paths() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "metrics/revenue.md",
            r#"---
type: metric
sources:
  - resource: policies/revenue.md
  - resource: ' ./local.md '
  - resource: '%2E%2Flocal-encoded.md'
  - resource: missing/source.md
  - resource: https://example.com/source
  - resource: production warehouse
---
[Computation](../computations/revenue.md)
"#,
        );
        write(fixture.path(), "metrics/local.md", &concept("dataset", ""));
        write(
            fixture.path(),
            "metrics/local-encoded.md",
            &concept("dataset", ""),
        );
        write(
            fixture.path(),
            "policies/revenue.md",
            &concept("policy", ""),
        );
        write(
            fixture.path(),
            "computations/revenue.md",
            &concept("computation", ""),
        );

        let result = load(fixture.path());

        assert_eq!(
            edges(&result),
            BTreeSet::from([
                (
                    "okf:fixture:metrics/revenue".into(),
                    "okf:fixture:computations/revenue".into(),
                ),
                (
                    "okf:fixture:metrics/revenue".into(),
                    "okf:fixture:metrics/local".into(),
                ),
                (
                    "okf:fixture:metrics/revenue".into(),
                    "okf:fixture:metrics/local-encoded".into(),
                ),
                (
                    "okf:fixture:metrics/revenue".into(),
                    "okf:fixture:policies/revenue".into(),
                ),
            ])
        );
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn okf_section_6_2_path_valued_fields_create_relationship_edges() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "metrics/revenue.md",
            "---\n\
type: metric\n\
title: Revenue\n\
executor:\n\
  resource: computations/revenue.md\n\
---\n",
        );
        write(fixture.path(), "computations/revenue.md", &concept("computation", ""));

        let result = load(fixture.path());

        assert_eq!(result.graph.node_count(), 2);
        assert_eq!(
            edges(&result),
            BTreeSet::from([(
                "okf:fixture:metrics/revenue".into(),
                "okf:fixture:computations/revenue".into(),
            )])
        );
    }

    #[test]
    fn okf_section_6_2_path_valued_fields_skip_dangling_targets() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "metrics/revenue.md",
            "---\n\
type: metric\n\
executor:\n\
  resource: ./missing-via-executor.md\n\
sources:\n\
  - resource: ./missing-via-source.md\n\
---\n",
        );
        write(fixture.path(), "sibling.md", &concept("other", ""));

        let result = load(fixture.path());

        assert_eq!(result.graph.node_count(), 2);
        // None of the §6.2 path-valued fields resolve: no edges.
        assert_eq!(result.graph.edge_count(), 0);
        // (matching the existing `sources[].resource` behavior); only body
        // links land in `unresolved`. There are no body links here, so the
        // unresolved list is empty.
    }

    #[test]
    fn reserved_files_and_unknown_versions_are_best_effort() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "index.md",
            "---\nokf_version: \"0.0.2\"\ntype: navigation\n---\n",
        );
        write(fixture.path(), "log.md", &concept("history", ""));
        write(
            fixture.path(),
            "nested/index.md",
            &concept("navigation", ""),
        );
        write(fixture.path(), "nested/log.md", &concept("history", ""));
        write(fixture.path(), "nested/only.md", &concept("concept", ""));

        let result = load(fixture.path());

        assert_eq!(result.graph.node_count(), 1);
        assert!(result.graph.nodes.contains_key("okf:fixture:nested/only"));
    }

    #[test]
    fn malformed_required_schema_fields_fail_the_import() {
        for (name, contents) in [
            ("missing-frontmatter", "# no frontmatter"),
            ("missing-type", "---\ntitle: Missing type\n---\n"),
            ("empty-type", "---\ntype: \"\"\n---\n"),
            ("numeric-type", "---\ntype: 42\n---\n"),
        ] {
            let fixture = TempDir::new().unwrap();
            write(fixture.path(), "bad.md", contents);
            let error = OkfImporter::new(fixture.path(), "fixture")
                .unwrap()
                .load_checked()
                .unwrap_err();
            assert!(
                matches!(error, OkfError::InvalidConcept { .. }),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn unknown_yaml_extensions_never_reject_an_otherwise_valid_concept() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "extended.md",
            r#"---
type: concept
extension:
  ? [compound, key]
  : value
? [unknown, top-level, key]
: ignored
---
"#,
        );

        let result = load(fixture.path());

        let extension = &result.graph.nodes["okf:fixture:extended"].meta.frontmatter["extension"];
        assert!(extension.as_str().is_some());
    }

    #[test]
    fn ordinary_yaml_anchors_and_aliases_are_bounded_and_supported() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "alias.md",
            r#"---
type: concept
base: &base [one, two]
extension: *base
---
"#,
        );

        let result = load(fixture.path());

        assert_eq!(
            result.graph.nodes["okf:fixture:alias"].meta.frontmatter["extension"],
            serde_json::json!(["one", "two"])
        );
    }

    #[test]
    fn yaml_alias_amplification_is_rejected_by_preparse_budgets() {
        let fixture = TempDir::new().unwrap();
        let mut frontmatter = String::from("---\ntype: concept\na0: &a0 [seed]\n");
        for level in 1..=8 {
            frontmatter.push_str(&format!("a{level}: &a{level} ["));
            for _ in 0..16 {
                frontmatter.push_str(&format!("*a{},", level - 1));
            }
            frontmatter.push_str("]\n");
        }
        frontmatter.push_str("---\n");
        write(fixture.path(), "bomb.md", &frontmatter);

        let error = OkfImporter::new(fixture.path(), "fixture")
            .unwrap()
            .load_checked()
            .unwrap_err();

        assert!(matches!(error, OkfError::InvalidConcept { .. }));
    }

    #[test]
    fn yaml_errors_do_not_echo_frontmatter_values() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "invalid.md",
            "---\ntype: concept\nsecret: [TOP-SECRET-VALUE\n---\n",
        );

        let error = OkfImporter::new(fixture.path(), "fixture")
            .unwrap()
            .load_checked()
            .unwrap_err()
            .to_string();

        assert!(!error.contains("TOP-SECRET-VALUE"), "{error}");
    }

    #[test]
    fn normalized_paths_cannot_escape_the_bundle() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "nested/source.md",
            r#"---
type: source
---
[valid](../target.md)
[escape](../../outside.md)
[encoded escape](%2e%2e/%2e%2e/outside.md)
[file](file:///outside.md)
[protocol relative](//example.com/outside.md)
[encoded URL](https%3A%2F%2Fexample.com%2Foutside.md)
"#,
        );
        write(fixture.path(), "target.md", &concept("target", ""));

        let result = load(fixture.path());

        assert_eq!(
            edges(&result),
            BTreeSet::from([(
                "okf:fixture:nested/source".into(),
                "okf:fixture:target".into(),
            )])
        );
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn percent_encoded_commonmark_destinations_resolve() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "source.md",
            "---\ntype: source\n---\n[space](<team%20blue.md>)\n[colon](1:metric.md)\n",
        );
        write(fixture.path(), "team blue.md", &concept("target", ""));
        write(fixture.path(), "1:metric.md", &concept("metric", ""));

        let result = load(fixture.path());

        assert_eq!(result.graph.edge_count(), 2);
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn configured_limits_are_enforced() {
        let fixture = TempDir::new().unwrap();
        write(fixture.path(), "one.md", &concept("concept", ""));
        write(fixture.path(), "two.md", &concept("concept", ""));
        let limits = Limits {
            nodes: 1,
            ..DEFAULT_LIMITS
        };

        let error = OkfImporter::with_limits(fixture.path(), "fixture", limits)
            .unwrap()
            .load_checked()
            .unwrap_err();

        assert!(matches!(
            error,
            OkfError::LimitExceeded {
                resource: "concept nodes",
                limit: 1
            }
        ));
    }

    #[test]
    fn identifier_and_edge_string_budgets_are_enforced() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "source.md",
            &concept("source", "[target](target.md)"),
        );
        write(fixture.path(), "target.md", &concept("target", ""));

        let identifier_error = OkfImporter::with_limits(
            fixture.path(),
            "fixture",
            Limits {
                identifier_bytes: 5,
                ..DEFAULT_LIMITS
            },
        )
        .unwrap()
        .load_checked()
        .unwrap_err();
        assert!(matches!(
            identifier_error,
            OkfError::LimitExceeded {
                resource: "identifier bytes",
                limit: 5
            }
        ));

        let edge_error = OkfImporter::with_limits(
            fixture.path(),
            "fixture",
            Limits {
                edge_endpoint_bytes: 1,
                ..DEFAULT_LIMITS
            },
        )
        .unwrap()
        .load_checked()
        .unwrap_err();
        assert!(matches!(
            edge_error,
            OkfError::LimitExceeded {
                resource: "edge endpoint bytes",
                limit: 1
            }
        ));
    }

    #[test]
    fn aggregate_walk_frontmatter_reference_and_diagnostic_budgets_are_enforced() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "source.md",
            "---\ntype: source\nauthors: a,b,c\n---\n[one](one.md) [two](two.md) [missing](missing.md)",
        );
        write(fixture.path(), "one.md", &concept("target", ""));
        write(fixture.path(), "two.md", &concept("target", ""));

        for (limits, resource) in [
            (
                Limits {
                    walk_entries: 1,
                    ..DEFAULT_LIMITS
                },
                "walked filesystem entries",
            ),
            (
                Limits {
                    frontmatter_nodes: 1,
                    ..DEFAULT_LIMITS
                },
                "retained frontmatter values",
            ),
            (
                Limits {
                    reference_occurrences: 1,
                    ..DEFAULT_LIMITS
                },
                "reference occurrences",
            ),
            (
                Limits {
                    reference_bytes: 1,
                    ..DEFAULT_LIMITS
                },
                "reference bytes",
            ),
            (
                Limits {
                    summary_values: 2,
                    ..DEFAULT_LIMITS
                },
                "filter-summary values",
            ),
            (
                Limits {
                    summary_value_bytes: 1,
                    ..DEFAULT_LIMITS
                },
                "filter-summary value bytes",
            ),
            (
                Limits {
                    unresolved_bytes: 1,
                    ..DEFAULT_LIMITS
                },
                "unresolved diagnostic bytes",
            ),
        ] {
            let error = OkfImporter::with_limits(fixture.path(), "fixture", limits)
                .unwrap()
                .load_checked()
                .unwrap_err();
            let OkfError::LimitExceeded {
                resource: actual, ..
            } = &error
            else {
                panic!("expected {resource}, got {error}");
            };
            assert_eq!(*actual, resource);
        }
    }

    #[test]
    fn file_reads_are_bounded_by_actual_bytes() {
        let fixture = TempDir::new().unwrap();
        write(fixture.path(), "large.md", "123456");
        let root = fixture.path().canonicalize().unwrap();

        let error = read_bounded_regular_file(&root, "large.md", 5).unwrap_err();

        assert!(matches!(
            error,
            OkfError::LimitExceeded {
                resource: "bytes in one concept file",
                limit: 5
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn file_reads_refuse_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write(outside.path(), "secret.md", "not part of the bundle");
        symlink(
            outside.path().join("secret.md"),
            fixture.path().join("alias.md"),
        )
        .unwrap();
        let root = fixture.path().canonicalize().unwrap();

        let error = read_bounded_regular_file(&root, "alias.md", 1024).unwrap_err();

        assert!(matches!(error, OkfError::Io { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn file_reads_refuse_fifos_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};

        let fixture = TempDir::new().unwrap();
        let fifo = fixture.path().join("raced.md");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a valid, NUL-terminated path for this call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let root = fixture.path().canonicalize().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(read_bounded_regular_file(&root, "raced.md", 1024))
                .unwrap();
        });

        let error = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("opening a FIFO must not block")
            .unwrap_err();

        assert!(matches!(error, OkfError::InvalidConcept { .. }));
    }

    #[test]
    fn descriptor_is_read_only_and_filesystem_scoped() {
        let fixture = TempDir::new().unwrap();
        let importer = OkfImporter::new(fixture.path(), "fixture").unwrap();
        let descriptor = importer.descriptor();
        let canonical_root = fixture.path().canonicalize().unwrap();
        let scope = canonical_root.to_string_lossy().into_owned();

        descriptor.validate().unwrap();

        assert_eq!(descriptor.id, "okf");
        assert_eq!(descriptor.version, "0.2");
        assert_eq!(
            descriptor.watch,
            WatchPlan::Filesystem {
                root: canonical_root
            }
        );
        assert!(descriptor.capabilities.contains(&Capability::new(
            Effect::Read,
            Transport::Filesystem,
            scope.clone()
        )));
        assert!(descriptor.capabilities.contains(&Capability::new(
            Effect::Watch,
            Transport::Filesystem,
            &scope
        )));
        // OKF is read-only but exposes per-node bodies via its own
        // `Importer::read_body` (the document-panel preview reads from
        // the deployment's filesystem root). `ContentRead` is scoped to
        // this importer's root; cross-source reads stay denied.
        assert!(descriptor.capabilities.contains(&Capability::new(
            Effect::ContentRead,
            Transport::Filesystem,
            scope.clone()
        )));
        assert!(!descriptor
            .capabilities
            .iter()
            .any(|capability| capability.effect == Effect::ContentWrite));
    }

    #[test]
    fn source_ids_cannot_make_ambiguous_node_namespaces() {
        for invalid in ["", "has space", "a:b", "path/segment", "unicode-λ", "UPPER"] {
            let error = OkfImporter::new(".", invalid).unwrap_err();
            assert!(matches!(error, OkfError::InvalidConfiguration(_)));
        }
        assert!(OkfImporter::new(".", "a".repeat(129)).is_err());
        assert!(OkfImporter::new(".", "team-blue_2.prod").is_ok());
    }

    #[tokio::test]
    async fn okf_importer_satisfies_the_shared_import_contract() {
        let fixture = TempDir::new().unwrap();
        write(
            fixture.path(),
            "concepts/alpha.md",
            &concept("metric", "[Beta](./beta.md)"),
        );
        write(fixture.path(), "concepts/beta.md", &concept("table", ""));

        let importer = OkfImporter::new(fixture.path(), "fixture").unwrap();
        data_loader::testing::assert_import_contract(&importer).await;
    }

    #[cfg(unix)]
    #[test]
    fn configured_root_is_canonicalized_and_pinned() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().unwrap();
        let first = parent.path().join("first");
        let second = parent.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        write(&first, "first.md", &concept("concept", ""));
        write(&second, "second.md", &concept("concept", ""));
        let configured = parent.path().join("current");
        symlink(&first, &configured).unwrap();
        let importer = OkfImporter::new(&configured, "fixture").unwrap();
        fs::remove_file(&configured).unwrap();
        symlink(&second, &configured).unwrap();

        let result = importer.load_checked().unwrap();

        assert!(result.graph.nodes.contains_key("okf:fixture:first"));
        assert!(!result.graph.nodes.contains_key("okf:fixture:second"));
    }

    #[test]
    fn checked_in_example_is_a_valid_v02_graph() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/mini-bundle");

        let result = OkfImporter::new(root, "mini")
            .unwrap()
            .load_checked()
            .unwrap();

        assert_eq!(result.graph.node_count(), 3);
        assert_eq!(result.graph.edge_count(), 3);
        assert!(result.unresolved.is_empty());
        assert_eq!(
            result.graph.nodes["okf:mini:datasets/orders"].meta.tags,
            ["commerce", "source of truth"]
        );
    }
}
