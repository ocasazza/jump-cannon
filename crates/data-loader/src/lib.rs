//! Source-neutral importer contracts.
//!
//! [`Importer`] is the asynchronous implementation contract and
//! [`HostedImporter`] pairs it with host-owned authority. [`SourceConnector`],
//! [`Decoder`], and [`GraphMapper`] keep effects separate from pure parsing and
//! graph projection, while [`Loader`] remains as a compatibility contract for
//! the original Obsidian, tvix, and generated graph adapters.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vault_data::VaultGraph;

pub mod identity;
pub mod testing;

/// Discovery-schema version understood by this release.
///
/// Version 2 introduces the unified identity contract: every importer
/// declares [`ImporterSchema::source_kind`] and every node ID is exactly
/// `{source_kind}:{source_id}:{local}` (see [`identity`]). Version 1 schemas
/// are rejected.
pub const DISCOVERY_SCHEMA_VERSION: u32 = 2;
/// Descriptor fields are intentionally bounded so a future package-control
/// plane cannot turn schema validation into an allocation attack.
pub const MAX_DISCOVERY_FIELDS: usize = 128;
/// One imported snapshot may retain at most this many bytes of indexed values.
pub const MAX_SEARCH_DOCUMENT_BYTES: usize = 512 * 1024 * 1024;

/// Logical value type emitted into a discovery document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryFieldType {
    Text,
    Keyword,
    KeywordList,
    Number,
    Boolean,
    Date,
    Url,
}

/// One named field in an importer's source-neutral discovery contract.
///
/// Unknown source attributes may still be retained in `NodeMeta.frontmatter`,
/// but they are neither indexed nor faceted until the importer explicitly
/// declares them here and emits a matching [`SearchDocument`] value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryField {
    pub key: String,
    pub field_type: DiscoveryFieldType,
    pub required: bool,
    pub searchable: bool,
    pub facetable: bool,
    #[serde(default)]
    pub snippet: bool,
    #[serde(default = "default_search_boost")]
    pub boost: u16,
    #[serde(default)]
    pub default_value: Option<serde_json::Value>,
    #[serde(default)]
    pub sensitive: bool,
}

const fn default_search_boost() -> u16 {
    1
}

impl DiscoveryField {
    pub fn new(key: impl Into<String>, field_type: DiscoveryFieldType, required: bool) -> Self {
        Self {
            key: key.into(),
            field_type,
            required,
            searchable: false,
            facetable: false,
            snippet: false,
            boost: 1,
            default_value: None,
            sensitive: false,
        }
    }

    pub fn searchable(mut self, boost: u16) -> Self {
        self.searchable = true;
        self.boost = boost;
        self
    }

    pub fn facetable(mut self) -> Self {
        self.facetable = true;
        self
    }

    pub fn snippet(mut self) -> Self {
        self.snippet = true;
        self
    }

    pub fn with_default(mut self, value: impl Into<serde_json::Value>) -> Self {
        self.default_value = Some(value.into());
        self
    }
}

/// Semantics of the canonical edges produced by one importer.
///
/// The `key` is declaration-only metadata — nothing consumes it at runtime.
/// Importers converge on this conventional vocabulary:
///
/// | key | meaning |
/// |---|---|
/// | `wikilink` | Obsidian `[[wikilink]]` from source note to target note |
/// | `declared` | edge declared by the source artifact itself (tvix Nix graph, Pest capture map) |
/// | `generated` | edge produced algorithmically by the graph generator |
/// | `relationship` | typed relationship between concepts (OKF links, provenance references) |
/// | `owner_reference` | Kubernetes `ownerReference` from owner to dependent |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeTypeSchema {
    pub key: String,
    pub directed: bool,
    #[serde(default)]
    pub description: Option<String>,
}

impl EdgeTypeSchema {
    pub fn directed(key: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            directed: true,
            description: Some(description.into()),
        }
    }
}

/// Source-content operations available after graph materialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ContentSchema {
    pub readable: bool,
    pub writable: bool,
    #[serde(default)]
    pub media_types: Vec<String>,
}

/// Application-wide tag path contract. Every importer must publish this
/// alongside its required `tags` field so clients can build one consistent
/// hierarchical navigator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagHierarchySchema {
    pub separator: char,
}

impl TagHierarchySchema {
    pub const fn slash() -> Self {
        Self { separator: '/' }
    }
}

/// One rule in the declarative link-extraction grammar: a YAML frontmatter
/// path whose string values are candidate references to other concepts in
/// the bundle (or relative paths that resolve against the source concept's
/// id). The path is dot-delimited; segments ending in `[*]` iterate over
/// the field as a YAML sequence; non-array segments recurse into YAML
/// mappings by key. Every leaf string is collected and routed through the
/// importer's link resolver (which decides whether the value resolves to
/// another concept in the bundle and, if so, emits an edge of `kind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRule {
    /// Dotted YAML path. Sequence iteration uses `[*]`; nested maps
    /// recurse by key. Examples:
    /// - `sources[*].resource` — every entry's `resource` field under `sources:`
    /// - `executor.resource` — single value at `executor.resource`
    /// - `resource` — single scalar at the top level
    pub path: String,
    /// Declared edge kind; must match a key in [`ImporterSchema::edge_types`].
    pub kind: String,
}

/// Versioned, mandatory output/discovery schema for an importer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImporterSchema {
    pub schema_version: u32,
    /// The lowercase [`SourceKind`] identifier of the importer that owns this
    /// schema. Every node ID in the import must be exactly
    /// `{source_kind}:{source_id}:{local}` (see [`identity`]).
    pub source_kind: String,
    #[serde(default)]
    pub input_media_types: Vec<String>,
    pub tag_hierarchy: TagHierarchySchema,
    pub fields: Vec<DiscoveryField>,
    pub edge_types: Vec<EdgeTypeSchema>,
    pub content: ContentSchema,
    /// Declarative link-extraction grammar. Each rule names a YAML
    /// frontmatter path whose string values are candidate references to
    /// other concepts in this bundle (or relative paths that resolve to
    /// them). The grammar is consumed by importers that expose frontmatter
    /// (e.g. OKF's §6.2 path-valued fields); importers that don't use
    /// frontmatter leave this empty and emit edges through their own
    /// dedicated paths.
    #[serde(default)]
    pub link_rules: Vec<LinkRule>,
 }

impl ImporterSchema {
    pub fn new(
        source_kind: impl Into<String>,
        fields: Vec<DiscoveryField>,
        edge_types: Vec<EdgeTypeSchema>,
        tag_hierarchy: TagHierarchySchema,
    ) -> Self {
        Self {
            schema_version: DISCOVERY_SCHEMA_VERSION,
            source_kind: source_kind.into(),
            input_media_types: Vec::new(),
            tag_hierarchy,
            fields,
            edge_types,
            content: ContentSchema::default(),
            link_rules: Vec::new(),
        }
    }

    pub fn with_input_media_types(
        mut self,
        media_types: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.input_media_types = media_types.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_content(mut self, content: ContentSchema) -> Self {
        self.content = content;
        self
    }

    pub fn with_link_rules(
        mut self,
        rules: impl IntoIterator<Item = LinkRule>,
    ) -> Self {
        self.link_rules = rules.into_iter().collect();
        self
    }

    pub fn field(&self, key: &str) -> Option<&DiscoveryField> {
        self.fields.iter().find(|field| field.key == key)
    }

    pub fn searchable_fields(&self) -> impl Iterator<Item = &DiscoveryField> {
        self.fields.iter().filter(|field| field.searchable)
    }

    pub fn facetable_fields(&self) -> impl Iterator<Item = &DiscoveryField> {
        self.fields.iter().filter(|field| field.facetable)
    }

    pub fn validate(&self) -> Result<(), ImportError> {
        if self.schema_version != DISCOVERY_SCHEMA_VERSION {
            return Err(invalid_descriptor(format!(
                "unsupported discovery schema version {}; supported version is {}",
                self.schema_version, DISCOVERY_SCHEMA_VERSION
            )));
        }
        if let Err(message) = identity::validate_source_kind(&self.source_kind) {
            return Err(invalid_descriptor(message));
        }
        if !SourceKind::all().contains(&self.source_kind.as_str()) {
            return Err(invalid_descriptor(format!(
                "source_kind {:?} must be a SourceKind lowercase identifier (one of {})",
                self.source_kind,
                SourceKind::all().join(", ")
            )));
        }
        if self.fields.is_empty() || self.fields.len() > MAX_DISCOVERY_FIELDS {
            return Err(invalid_descriptor(format!(
                "discovery schema must declare between 1 and {MAX_DISCOVERY_FIELDS} fields"
            )));
        }
        if self.tag_hierarchy.separator != '/' {
            return Err(invalid_descriptor(format!(
                "tag hierarchy separator must be the application-wide '/' delimiter, got {:?}",
                self.tag_hierarchy.separator
            )));
        }

        let mut keys = HashSet::with_capacity(self.fields.len());
        for field in &self.fields {
            if !valid_schema_key(&field.key) {
                return Err(invalid_descriptor(format!(
                    "invalid discovery field key {:?}",
                    field.key
                )));
            }
            if !keys.insert(field.key.as_str()) {
                return Err(invalid_descriptor(format!(
                    "duplicate discovery field key {:?}",
                    field.key
                )));
            }
            if field.searchable && field.boost == 0 {
                return Err(invalid_descriptor(format!(
                    "searchable field {:?} must have a non-zero boost",
                    field.key
                )));
            }
            if field.boost > 100 {
                return Err(invalid_descriptor(format!(
                    "field {:?} boost {} exceeds 100",
                    field.key, field.boost
                )));
            }
            if field.snippet && (!field.searchable || field.field_type != DiscoveryFieldType::Text)
            {
                return Err(invalid_descriptor(format!(
                    "snippet field {:?} must be searchable text",
                    field.key
                )));
            }
            if field.facetable && field.field_type == DiscoveryFieldType::Text {
                return Err(invalid_descriptor(format!(
                    "free-text field {:?} cannot be facetable",
                    field.key
                )));
            }
            if field.sensitive
                && (field.required
                    || field.searchable
                    || field.facetable
                    || field.snippet
                    || field.default_value.is_some())
            {
                return Err(invalid_descriptor(format!(
                    "sensitive field {:?} cannot be required, defaulted, indexed, faceted, or snippet-capable",
                    field.key
                )));
            }
            if let Some(value) = &field.default_value {
                validate_field_value(field, value).map_err(|message| {
                    invalid_descriptor(format!(
                        "default for field {:?} is invalid: {message}",
                        field.key
                    ))
                })?;
            }
        }

        // These fields are the source-neutral discovery surface every host
        // may rely on. Tags are also a mandatory facet so clients can build
        // bulk navigation and filters without issuing one request per node.
        for (key, expected_type) in [
            ("id", DiscoveryFieldType::Keyword),
            ("title", DiscoveryFieldType::Text),
            ("tags", DiscoveryFieldType::KeywordList),
        ] {
            let Some(field) = self.field(key) else {
                return Err(invalid_descriptor(format!(
                    "discovery schema must declare required field {key:?}"
                )));
            };
            let missing_required_facet = key == "tags" && !field.facetable;
            if !field.required
                || !field.searchable
                || field.field_type != expected_type
                || field.default_value.is_some()
                || missing_required_facet
            {
                let facet_requirement = if key == "tags" { ", facetable" } else { "" };
                return Err(invalid_descriptor(format!(
                    "field {key:?} must be required, searchable{facet_requirement}, typed {expected_type:?}, and have no default"
                )));
            }
        }

        if self.edge_types.is_empty() {
            return Err(invalid_descriptor(
                "discovery schema must declare at least one edge type".into(),
            ));
        }
        let mut edge_keys = HashSet::with_capacity(self.edge_types.len());
        for edge in &self.edge_types {
            if !valid_schema_key(&edge.key) || !edge_keys.insert(edge.key.as_str()) {
                return Err(invalid_descriptor(format!(
                    "edge type keys must be valid and unique: {:?}",
                    edge.key
                )));
            }
        }
        if !self.link_rules.is_empty() {
            for rule in &self.link_rules {
                if rule.path.trim().is_empty() {
                    return Err(invalid_descriptor(
                        "link rule path must be a non-empty dotted YAML path".to_string(),
                    ));
                }
                if !edge_keys.contains(rule.kind.as_str()) {
                    return Err(invalid_descriptor(format!(
                        "link rule {:?} references undeclared edge type {:?}; declare it in edge_types",
                        rule.path, rule.kind
                    )));
                }
            }
        }
        if self.content.writable && !self.content.readable {
            return Err(invalid_descriptor(
                "writable content must also be readable".into(),
            ));
        }
        if (self.content.readable || self.content.writable) && self.content.media_types.is_empty() {
            return Err(invalid_descriptor(
                "readable or writable content must declare at least one media type".into(),
            ));
        }
        validate_media_types(&self.input_media_types)?;
        validate_media_types(&self.content.media_types)?;
        Ok(())
    }

    /// Validate the searchable projection emitted for one completed import.
    pub fn validate_result(&self, result: &LoadResult) -> Result<(), ImportError> {
        self.validate_output(&result.graph, &result.search_documents)
    }

    /// Validate a graph and its searchable projection without cloning either.
    pub fn validate_output(
        &self,
        graph: &VaultGraph,
        search_documents: &[SearchDocument],
    ) -> Result<(), ImportError> {
        self.validate()?;
        if search_documents.len() != graph.nodes.len() {
            return Err(ImportError::Map {
                message: format!(
                    "importer emitted {} search documents for {} graph nodes",
                    search_documents.len(),
                    graph.nodes.len()
                ),
            });
        }

        let fields: HashMap<&str, &DiscoveryField> = self
            .fields
            .iter()
            .map(|field| (field.key.as_str(), field))
            .collect();
        let mut seen = HashSet::with_capacity(search_documents.len());
        let mut total_bytes = 0usize;
        for document in search_documents {
            let Some(node) = graph.nodes.get(&document.node_id) else {
                return Err(ImportError::Map {
                    message: format!(
                        "search document references unknown node {:?}",
                        document.node_id
                    ),
                });
            };
            if !seen.insert(document.node_id.as_str()) {
                return Err(ImportError::Map {
                    message: format!("duplicate search document for node {:?}", document.node_id),
                });
            }
            for key in document.fields.keys() {
                let Some(field) = fields.get(key.as_str()) else {
                    return Err(ImportError::Map {
                        message: format!(
                            "search document for {:?} emitted undeclared field {:?}",
                            document.node_id, key
                        ),
                    });
                };
                if field.sensitive {
                    return Err(ImportError::Map {
                        message: format!(
                            "search document for {:?} emitted sensitive field {:?}",
                            document.node_id, key
                        ),
                    });
                }
            }
            for field in &self.fields {
                let value = document
                    .fields
                    .get(&field.key)
                    .or(field.default_value.as_ref());
                if field.required && value.is_none() {
                    return Err(ImportError::Map {
                        message: format!(
                            "search document for {:?} is missing required field {:?}",
                            document.node_id, field.key
                        ),
                    });
                }
                if let Some(value) = value {
                    validate_field_value(field, value).map_err(|message| ImportError::Map {
                        message: format!(
                            "search document for {:?} has invalid field {:?}: {message}",
                            document.node_id, field.key
                        ),
                    })?;
                    total_bytes = total_bytes.saturating_add(json_value_bytes(value));
                    if total_bytes > MAX_SEARCH_DOCUMENT_BYTES {
                        return Err(ImportError::Map {
                            message: format!(
                                "search document values exceed {MAX_SEARCH_DOCUMENT_BYTES} bytes"
                            ),
                        });
                    }
                }
            }
            let indexed_id = document
                .fields
                .get("id")
                .and_then(serde_json::Value::as_str)
                .expect("validated core id field");
            if indexed_id != document.node_id {
                return Err(ImportError::Map {
                    message: format!(
                        "search document for {:?} indexes mismatched id {:?}",
                        document.node_id, indexed_id
                    ),
                });
            }
            let indexed_title = document
                .fields
                .get("title")
                .and_then(serde_json::Value::as_str)
                .expect("validated core title field");
            if indexed_title.trim().is_empty() || indexed_title != node.meta.title {
                return Err(ImportError::Map {
                    message: format!(
                        "search document for {:?} must index its canonical non-empty title",
                        document.node_id
                    ),
                });
            }
            let indexed_tags = document
                .fields
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .expect("validated core tags field");
            for tag in indexed_tags.iter().filter_map(serde_json::Value::as_str) {
                validate_tag_path(tag, self.tag_hierarchy.separator).map_err(|message| {
                    ImportError::Map {
                        message: format!(
                            "search document for {:?} has invalid tag {tag:?}: {message}",
                            document.node_id
                        ),
                    }
                })?;
            }
            if indexed_tags.len() != node.meta.tags.len()
                || indexed_tags
                    .iter()
                    .zip(&node.meta.tags)
                    .any(|(indexed, canonical)| indexed.as_str() != Some(canonical.as_str()))
            {
                return Err(ImportError::Map {
                    message: format!(
                        "search document for {:?} must index its canonical tags",
                        document.node_id
                    ),
                });
            }
        }

        for node in graph.nodes.values() {
            // The shared namespace-ambiguity rule: every source_id must be a
            // safe single namespace segment (`[a-z0-9._-]{1,128}`, no `:`).
            if let Err(message) = identity::validate_source_id(&node.meta.source_id) {
                return Err(ImportError::Map {
                    message: format!("node {:?} has an invalid source_id: {message}", node.id),
                });
            }
            // Namespace conformance: every node ID is exactly
            // `{source_kind}:{source_id}:{local}` with a valid local part.
            let prefix = format!("{}:{}:", self.source_kind, node.meta.source_id);
            let Some(local) = node.id.strip_prefix(&prefix) else {
                return Err(ImportError::Map {
                    message: format!(
                        "node {:?} does not start with its namespace prefix {prefix:?}",
                        node.id
                    ),
                });
            };
            if let Err(message) = identity::validate_local_id(local) {
                return Err(ImportError::Map {
                    message: format!("node {:?} has an invalid local id: {message}", node.id),
                });
            }
            if node.meta.content_readable && !self.content.readable {
                return Err(ImportError::Map {
                    message: format!(
                        "node {:?} advertises readable content outside its schema",
                        node.id
                    ),
                });
            }
            if node.meta.content_writable && !self.content.writable {
                return Err(ImportError::Map {
                    message: format!(
                        "node {:?} advertises writable content outside its schema",
                        node.id
                    ),
                });
            }
        }

        // Every edge endpoint must resolve to a node in this import: dangling
        // wikilink/reference resolution can never reach publication.
        for (index, edge) in graph.edges.iter().enumerate() {
            if !graph.nodes.contains_key(&edge.source) || !graph.nodes.contains_key(&edge.target)
            {
                return Err(ImportError::Map {
                    message: format!(
                        "edge {index} has a missing endpoint: {:?} -> {:?}",
                        edge.source, edge.target
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Searchable/facetable values emitted for one graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchDocument {
    pub node_id: String,
    pub fields: BTreeMap<String, serde_json::Value>,
}

impl SearchDocument {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            fields: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.fields.insert(key.into(), value.into());
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.insert(key, value);
        self
    }
}

fn valid_schema_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && key.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
}

fn validate_tag_path(tag: &str, separator: char) -> Result<(), &'static str> {
    if tag
        .split(separator)
        .any(|segment| segment.trim().is_empty())
    {
        Err("hierarchical tags must contain a non-empty segment on both sides of every separator")
    } else {
        Ok(())
    }
}

fn validate_media_types(media_types: &[String]) -> Result<(), ImportError> {
    let mut seen = HashSet::with_capacity(media_types.len());
    for media_type in media_types {
        if media_type.trim().is_empty()
            || media_type.chars().any(char::is_whitespace)
            || !media_type.contains('/')
        {
            return Err(invalid_descriptor(format!(
                "invalid media type {media_type:?}"
            )));
        }
        if !seen.insert(media_type) {
            return Err(invalid_descriptor(format!(
                "duplicate media type {media_type:?}"
            )));
        }
    }
    Ok(())
}

fn validate_field_value(field: &DiscoveryField, value: &serde_json::Value) -> Result<(), String> {
    let valid = match field.field_type {
        DiscoveryFieldType::Text => value.is_string(),
        DiscoveryFieldType::Keyword | DiscoveryFieldType::Date | DiscoveryFieldType::Url => {
            value.as_str().is_some_and(|value| !value.trim().is_empty())
        }
        DiscoveryFieldType::KeywordList => value.as_array().is_some_and(|items| {
            let mut seen = HashSet::with_capacity(items.len());
            items.iter().all(|item| {
                item.as_str()
                    .filter(|value| !value.trim().is_empty())
                    .is_some_and(|value| seen.insert(value))
            })
        }),
        DiscoveryFieldType::Number => value.is_number(),
        DiscoveryFieldType::Boolean => value.is_boolean(),
    };
    if valid {
        Ok(())
    } else {
        let expectation = match field.field_type {
            DiscoveryFieldType::Keyword
            | DiscoveryFieldType::KeywordList
            | DiscoveryFieldType::Date
            | DiscoveryFieldType::Url => match field.field_type {
                DiscoveryFieldType::KeywordList => {
                    "expected KeywordList with non-empty unique values".into()
                }
                _ => format!("expected {:?} with a non-empty value", field.field_type),
            },
            _ => format!("expected {:?}", field.field_type),
        };
        Err(expectation)
    }
}

fn json_value_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(number) => number.to_string().len(),
        serde_json::Value::String(string) => string.len(),
        serde_json::Value::Array(values) => values.iter().map(json_value_bytes).sum(),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.len() + json_value_bytes(value))
            .sum(),
    }
}

fn invalid_descriptor(message: String) -> ImportError {
    ImportError::InvalidDescriptor { message }
}


/// The result of a single load pass.
#[derive(Debug)]
pub struct LoadResult {
    /// The populated graph (nodes + resolved edges).
    pub graph: VaultGraph,
    /// Typed, bounded discovery projection. There must be exactly one document
    /// per graph node, and every field must be declared by the importer schema.
    pub search_documents: Vec<SearchDocument>,
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
/// Loaders that back a live filesystem (Obsidian vault) can optionally expose
/// their root path. Loaders for static / generated data (tvix, CSV) return
/// `None`.
pub trait Loader: Send + Sync {
    /// Human-readable name for progress / UI (e.g. "obsidian", "tvix").
    fn name(&self) -> &str;

    /// Mandatory output/discovery contract for this loader.
    fn schema(&self) -> ImporterSchema;

    /// Produce a fresh graph from the source.
    fn load(&self) -> LoadResult;

    /// Produce a fresh graph through the mandatory importer boundary.
    ///
    /// Compatibility loaders default to their infallible [`Loader::load`]
    /// implementation. Loaders whose source can fail must override this method
    /// so hosts retain the previous snapshot instead of publishing a synthetic
    /// empty result. `load` may still preserve legacy diagnostic behavior for
    /// direct callers.
    fn try_load(&self) -> Result<LoadResult, ImportError> {
        Ok(self.load())
    }

    /// The root path this loader reads from, if any. Used by the watcher to
    /// know *what* to watch. Returns `None` for sources that have no
    /// filesystem root (tvix, in-memory generators).
    fn root_path(&self) -> Option<&PathBuf> {
        None
    }

    /// Additional explicit effects implemented by a compatibility loader.
    /// Read and filesystem watch effects are inferred by the blanket importer
    /// adapter; content/search/write effects must be opted into here.
    fn additional_effects(&self) -> &'static [Effect] {
        &[]
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
    /// List allowlisted Kubernetes dynamic resources through kube-rs.
    Kubernetes,
    /// Import an Open Knowledge Format v0.2 bundle from the filesystem.
    Okf,
    /// Parse a bounded filesystem input with an administrator-installed,
    /// runtime-validated Pest grammar package.
    Pest,
    /// Import a vault corpus from a GitHub repository tarball (codeload,
    /// ETag-revalidated polling).
    GitHub,
    /// Read a paged JSON API through the declarative package engine
    /// (`crates/http-json-importer`). One kind serves every JSON API: the
    /// package supplies the endpoints and mapping, and instances vary by
    /// `source_id`, exactly as Pest packages do.
    HttpJson,
    /// A versioned shared world served by the session manager
    /// (`crates/session-manager`). Not a CLI-selectable source: worlds are
    /// hosted per-world by the session-manager server.
    World,
}

impl SourceKind {
    /// Parse from a CLI string. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "obsidian" | "vault" => Some(Self::Obsidian),
            "tvix" | "nix" => Some(Self::Tvix),
            "generate" | "gen" | "random" => Some(Self::Generate),
            "kubernetes" | "k8s" => Some(Self::Kubernetes),
            "okf" | "open-knowledge-format" => Some(Self::Okf),
            "pest" | "grammar" => Some(Self::Pest),
            "github" => Some(Self::GitHub),
            "httpjson" | "http-json" => Some(Self::HttpJson),
            "world" => Some(Self::World),
            _ => None,
        }
    }

    /// All known source kinds (for help text).
    pub fn all() -> &'static [&'static str] {
        &[
            "obsidian",
            "tvix",
            "generate",
            "kubernetes",
            "okf",
            "pest",
            "github",
            "httpjson",
            "world",
        ]
    }
}

/// Heap-allocated future used by object-safe asynchronous importer traits.
pub type ImportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Effects an importer may request from its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Watch,
    Write,
    Search,
    ContentRead,
    ContentWrite,
}

/// Mechanism through which an importer reaches its data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Filesystem,
    Http,
    Kubernetes,
    Grpc,
    Udp,
    InMemory,
    WasmComponent,
}

/// One exact effect grant. Scope is deliberately opaque to the core: callers
/// can use paths, URLs, cluster names, namespaces, or component identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability {
    pub effect: Effect,
    pub transport: Transport,
    pub scope: String,
}

impl Capability {
    pub fn new(effect: Effect, transport: Transport, scope: impl Into<String>) -> Self {
        Self {
            effect,
            transport,
            scope: scope.into(),
        }
    }
}

/// How the host should learn that a source may have changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchPlan {
    #[default]
    Static,
    Filesystem {
        root: PathBuf,
    },
    Poll {
        interval_ms: u64,
    },
    Push,
}

/// Stable metadata advertised by an importer implementation or manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImporterDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub watch: WatchPlan,
    pub schema: ImporterSchema,
}

impl ImporterDescriptor {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        capabilities: Vec<Capability>,
        schema: ImporterSchema,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            capabilities,
            watch: WatchPlan::Static,
            schema,
        }
    }

    pub fn with_watch(mut self, watch: WatchPlan) -> Self {
        self.watch = watch;
        self
    }

    /// Validate identity, capabilities, and the mandatory discovery schema.
    pub fn validate(&self) -> Result<(), ImportError> {
        if self.id.trim().is_empty()
            || self.name.trim().is_empty()
            || self.version.trim().is_empty()
        {
            return Err(invalid_descriptor(
                "id, name, and version must be non-empty".into(),
            ));
        }
        if !self
            .capabilities
            .iter()
            .any(|capability| capability.effect == Effect::Read)
        {
            return Err(invalid_descriptor(
                "an importer must request at least one read capability".into(),
            ));
        }
        if self.schema.content.readable
            != self
                .capabilities
                .iter()
                .any(|capability| capability.effect == Effect::ContentRead)
        {
            return Err(invalid_descriptor(
                "content.readable must agree with ContentRead capability".into(),
            ));
        }
        if self.schema.content.writable
            != self
                .capabilities
                .iter()
                .any(|capability| capability.effect == Effect::ContentWrite)
        {
            return Err(invalid_descriptor(
                "content.writable must agree with ContentWrite capability".into(),
            ));
        }
        self.schema.validate()
    }
}

/// A fallible asynchronous graph importer.
///
/// The boxed-future method keeps the trait object-safe without requiring an
/// `async-trait` transformation.
pub trait Importer: Send + Sync {
    fn descriptor(&self) -> ImporterDescriptor;
    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>>;

    /// Optional body reader: return the markdown body for a node whose
    /// `meta.path` is `path` (relative to the importer's own filesystem root),
    /// or `None` when this importer doesn't surface bodies. Default is `None`.
    /// Hosts that serve per-source bodies (see [`Effect::ContentRead`]) prefer
    /// this over a shared filesystem read so each importer can project its
    /// own body semantics (e.g. OKF falls back to the `description`
    /// frontmatter when the markdown body is empty). The host already holds
    /// the matching capability; importers don't re-authorize.
    fn read_body(&self, _path: &str) -> Option<String> {
        None
    }
}

/// Compatibility bridge: every existing synchronous [`Loader`] is also an
/// asynchronous [`Importer`]. The synchronous work runs when the future is
/// polled; hosts should use a blocking executor for loaders that perform I/O.
impl<T> Importer for T
where
    T: Loader + ?Sized,
{
    fn descriptor(&self) -> ImporterDescriptor {
        let (transport, scope, watch) = match self.root_path() {
            Some(root) => (
                Transport::Filesystem,
                root.to_string_lossy().into_owned(),
                WatchPlan::Filesystem { root: root.clone() },
            ),
            None => (
                Transport::InMemory,
                self.name().to_string(),
                WatchPlan::Static,
            ),
        };
        let mut capabilities = vec![Capability::new(Effect::Read, transport, scope.clone())];
        if !matches!(watch, WatchPlan::Static) {
            capabilities.push(Capability::new(Effect::Watch, transport, scope.clone()));
        }
        capabilities.extend(
            self.additional_effects()
                .iter()
                .copied()
                .map(|effect| Capability::new(effect, transport, scope.clone())),
        );
        ImporterDescriptor::new(
            self.name(),
            self.name(),
            env!("CARGO_PKG_VERSION"),
            capabilities,
            self.schema(),
        )
        .with_watch(watch)
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move { self.try_load() })
    }
}

/// An importer paired with authority selected by its host.
///
/// Importer descriptors only request capabilities. They never grant their own
/// effects. The host constructs this wrapper from independently selected exact
/// grants; every import preflights all requested read effects before importer
/// code can touch its source.
pub struct HostedImporter {
    importer: Box<dyn Importer>,
    grants: HashSet<Capability>,
}

impl HostedImporter {
    pub fn new(
        importer: Box<dyn Importer>,
        grants: impl IntoIterator<Item = Capability>,
    ) -> Result<Self, ImportError> {
        let descriptor = importer.descriptor();
        descriptor.validate()?;

        let mut exact_grants = HashSet::new();
        for grant in grants {
            if !descriptor.capabilities.contains(&grant) {
                return Err(ImportError::CapabilityDenied { capability: grant });
            }
            exact_grants.insert(grant);
        }

        Ok(Self {
            importer,
            grants: exact_grants,
        })
    }

    pub fn descriptor(&self) -> ImporterDescriptor {
        self.importer.descriptor()
    }

    /// Return whether the host granted this exact declared capability.
    pub fn is_authorized(&self, capability: &Capability) -> bool {
        self.importer.descriptor().capabilities.contains(capability)
            && self.grants.contains(capability)
    }

    pub fn authorize(&self, capability: &Capability) -> Result<(), ImportError> {
        if self.is_authorized(capability) {
            Ok(())
        } else {
            Err(ImportError::CapabilityDenied {
                capability: capability.clone(),
            })
        }
    }
}

impl Importer for HostedImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        self.importer.descriptor()
    }

    fn read_body(&self, path: &str) -> Option<String> {
        self.importer.read_body(path)
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move {
            let descriptor = self.importer.descriptor();
            descriptor.validate()?;
            for capability in descriptor
                .capabilities
                .iter()
                .filter(|capability| capability.effect == Effect::Read)
            {
                self.authorize(capability)?;
            }
            let result = self.importer.import().await?;
            descriptor.schema.validate_result(&result)?;
            Ok(result)
        })
    }
}

/// Raw payload acquired from one source object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    pub origin: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Source payload after a pure decoder has interpreted its wire format.
///
/// The connector's acquisition [`metadata`](Self::metadata) rides through
/// decoding unchanged: a decoder interprets bytes, it does not get to decide
/// which query produced them. Multi-endpoint connectors depend on this to tell
/// a mapper which collection a document came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedRecord {
    pub origin: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// A write requested against a source connector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteRequest {
    pub origin: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Connector acknowledgement for a completed write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteReceipt {
    pub origin: String,
    pub bytes_written: u64,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Errors crossing the importer boundary. Variants retain the stage and origin
/// so callers can report failures without parsing display strings.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImportError {
    #[error("invalid importer descriptor: {message}")]
    InvalidDescriptor { message: String },
    #[error("capability denied: {capability:?}")]
    CapabilityDenied { capability: Capability },
    #[error("source read failed at {origin}: {message}")]
    SourceRead { origin: String, message: String },
    #[error("source write failed at {origin}: {message}")]
    SourceWrite { origin: String, message: String },
    #[error("decode failed at {origin}: {message}")]
    Decode { origin: String, message: String },
    #[error(
        "unsupported input media type {content_type:?} at {origin}; importer accepts {accepted:?}"
    )]
    UnsupportedMediaType {
        origin: String,
        content_type: String,
        accepted: Vec<String>,
    },
    #[error("graph mapping failed: {message}")]
    Map { message: String },
    #[error("effect {effect:?} is unsupported")]
    UnsupportedEffect { effect: Effect },
}

/// Effectful source boundary. Implementations may read from files, remote APIs,
/// streams, or component hosts; parsing is intentionally delegated to
/// [`Decoder`].
pub trait SourceConnector: Send + Sync {
    /// Exact capabilities required for `effect` on this configured connector.
    /// Connectors that aggregate multiple independently scoped queries return
    /// one entry per query.
    fn capabilities(&self, effect: Effect) -> Vec<Capability>;

    fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>>;

    fn write<'a>(
        &'a self,
        _request: WriteRequest,
    ) -> ImportFuture<'a, Result<WriteReceipt, ImportError>> {
        Box::pin(async move {
            Err(ImportError::UnsupportedEffect {
                effect: Effect::Write,
            })
        })
    }
}

/// Pure wire-format decoder. It has no authority to perform I/O.
pub trait Decoder: Send + Sync {
    fn decode(&self, record: SourceRecord) -> Result<DecodedRecord, ImportError>;
}

/// Pure mapping from decoded source records into the application's graph IR.
pub trait GraphMapper: Send + Sync {
    fn map(&self, records: Vec<DecodedRecord>) -> Result<LoadResult, ImportError>;
}

/// Composes acquisition, decoding, and graph mapping while verifying that all
/// connector requirements are declared. [`HostedImporter`] independently owns
/// and enforces the exact grant set before this pipeline runs.
pub struct ImportPipeline {
    descriptor: ImporterDescriptor,
    connector: Box<dyn SourceConnector>,
    decoder: Box<dyn Decoder>,
    mapper: Box<dyn GraphMapper>,
}

impl ImportPipeline {
    pub fn new(
        descriptor: ImporterDescriptor,
        connector: Box<dyn SourceConnector>,
        decoder: Box<dyn Decoder>,
        mapper: Box<dyn GraphMapper>,
    ) -> Result<Self, ImportError> {
        descriptor.validate()?;
        if descriptor.schema.input_media_types.is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "connector-backed importer must declare at least one input media type"
                    .into(),
            });
        }

        let reads = connector.capabilities(Effect::Read);
        if reads.is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "connector must require at least one read capability".into(),
            });
        }
        for read in &reads {
            if !descriptor.capabilities.contains(read) {
                return Err(ImportError::InvalidDescriptor {
                    message: format!("connector read capability is not declared: {read:?}"),
                });
            }
        }

        if !matches!(descriptor.watch, WatchPlan::Static) {
            let watches = connector.capabilities(Effect::Watch);
            if watches.is_empty() {
                return Err(ImportError::InvalidDescriptor {
                    message: "watch plan must require at least one watch capability".into(),
                });
            }
            for watch in &watches {
                if !descriptor.capabilities.contains(watch) {
                    return Err(ImportError::InvalidDescriptor {
                        message: format!("watch plan lacks its exact capability: {watch:?}"),
                    });
                }
            }
        }

        Ok(Self {
            descriptor,
            connector,
            decoder,
            mapper,
        })
    }

    pub fn descriptor(&self) -> &ImporterDescriptor {
        &self.descriptor
    }

    pub fn watch_plan(&self) -> &WatchPlan {
        &self.descriptor.watch
    }

    /// Execute connector -> decoder -> mapper in that order.
    pub async fn run(&self) -> Result<LoadResult, ImportError> {
        let source_records = self.connector.read().await?;
        for record in &source_records {
            let actual_media_type = record
                .content_type
                .split_once(';')
                .map_or(record.content_type.as_str(), |(media_type, _)| media_type)
                .trim();
            if !self
                .descriptor
                .schema
                .input_media_types
                .iter()
                .any(|accepted| accepted.eq_ignore_ascii_case(actual_media_type))
            {
                return Err(ImportError::UnsupportedMediaType {
                    origin: record.origin.clone(),
                    content_type: record.content_type.clone(),
                    accepted: self.descriptor.schema.input_media_types.clone(),
                });
            }
        }
        let decoded = source_records
            .into_iter()
            .map(|record| self.decoder.decode(record))
            .collect::<Result<Vec<_>, _>>()?;
        let result = self.mapper.map(decoded)?;
        self.descriptor.schema.validate_result(&result)?;
        Ok(result)
    }
}

impl Importer for ImportPipeline {
    fn descriptor(&self) -> ImporterDescriptor {
        self.descriptor.clone()
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move { self.run().await })
    }
}

#[cfg(test)]
mod importer_tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use vault_data::VaultNode;

    use super::*;

    type Trace = Arc<Mutex<Vec<String>>>;

    fn capability(effect: Effect, scope: &str) -> Capability {
        Capability::new(effect, Transport::InMemory, scope)
    }

    fn test_schema() -> ImporterSchema {
        ImporterSchema::new(
            "generate",
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                    .searchable(3)
                    .snippet(),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                    .searchable(2)
                    .facetable(),
            ],
            vec![EdgeTypeSchema::directed(
                "relationship",
                "Test relationship",
            )],
            TagHierarchySchema::slash(),
        )
        .with_input_media_types(["text/plain"])
    }

    fn descriptor(capabilities: Vec<Capability>) -> ImporterDescriptor {
        ImporterDescriptor::new("fake", "Fake", "1", capabilities, test_schema())
    }

    /// Namespaced test node ID: `{source_kind}:{source_id}:{local}` for the
    /// `generate`/`fixture` test namespace.
    const N1: &str = "generate:fixture:n1";

    fn one_node_result(document: SearchDocument) -> LoadResult {
        let mut graph = VaultGraph::new();
        graph.add_node(VaultNode {
            id: N1.into(),
            meta: vault_data::NodeMeta {
                source_id: "fixture".into(),
                title: "Node one".into(),
                tags: vec!["fixture".into()],
                ..Default::default()
            },
            ..Default::default()
        });
        LoadResult {
            graph,
            search_documents: vec![document],
            unresolved: Vec::new(),
        }
    }

    fn valid_document() -> SearchDocument {
        SearchDocument::new(N1)
            .with("id", N1)
            .with("title", "Node one")
            .with("tags", json!(["fixture"]))
    }

    #[test]
    fn discovery_schema_rejects_missing_and_duplicate_core_fields() {
        for missing in ["id", "title", "tags"] {
            let mut schema = test_schema();
            schema.fields.retain(|field| field.key != missing);
            let error = schema.validate().unwrap_err().to_string();
            assert!(
                error.contains(&format!("required field \"{missing}\"")),
                "{missing}: {error}"
            );
        }

        let mut schema = test_schema();
        schema.fields.push(schema.fields[0].clone());
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("duplicate discovery field key \"id\""),
            "{error}"
        );

        let mut schema = test_schema();
        schema.fields.push(DiscoveryField::new(
            "invalid key",
            DiscoveryFieldType::Keyword,
            false,
        ));
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("invalid discovery field key \"invalid key\""),
            "{error}"
        );

        let mut schema = test_schema();
        schema.fields.push(DiscoveryField::new(
            "invalid-key",
            DiscoveryFieldType::Keyword,
            false,
        ));
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("invalid discovery field key \"invalid-key\""),
            "{error}"
        );
    }

    #[test]
    fn discovery_schema_rejects_invalid_core_field_contracts() {
        let mut schema = test_schema();
        schema.field_mut("title").field_type = DiscoveryFieldType::Keyword;
        schema.field_mut("title").snippet = false;
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("field \"title\" must be required, searchable"),
            "{error}"
        );

        let mut schema = test_schema();
        schema.field_mut("tags").searchable = false;
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("field \"tags\" must be required, searchable, facetable"),
            "{error}"
        );

        let mut schema = test_schema();
        schema.field_mut("tags").facetable = false;
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("field \"tags\" must be required, searchable, facetable"),
            "{error}"
        );

        let mut schema = test_schema();
        schema.field_mut("id").default_value = Some(json!("default-id"));
        let error = schema.validate().unwrap_err().to_string();
        assert!(error.contains("and have no default"), "{error}");
    }

    #[test]
    fn discovery_schema_requires_the_application_tag_hierarchy() {
        test_schema().validate().unwrap();

        let mut invalid_separator = test_schema();
        invalid_separator.tag_hierarchy.separator = ':';
        let error = invalid_separator.validate().unwrap_err().to_string();
        assert!(error.contains("application-wide '/' delimiter"), "{error}");

        let mut serialized = serde_json::to_value(test_schema()).unwrap();
        serialized.as_object_mut().unwrap().remove("tag_hierarchy");
        let error = serde_json::from_value::<ImporterSchema>(serialized).unwrap_err();
        assert!(error.to_string().contains("missing field `tag_hierarchy`"));
    }

    #[test]
    fn discovery_documents_reject_missing_undeclared_and_wrong_typed_fields() {
        let schema = test_schema();

        let missing = SearchDocument::new(N1)
            .with("id", N1)
            .with("tags", json!([]));
        let error = schema
            .validate_result(&one_node_result(missing))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("missing required field \"title\""),
            "{error}"
        );

        let undeclared = valid_document().with("secret", "must not be indexed");
        let error = schema
            .validate_result(&one_node_result(undeclared))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("emitted undeclared field \"secret\""),
            "{error}"
        );

        let wrong_type = SearchDocument::new(N1)
            .with("id", N1)
            .with("title", "Node one")
            .with("tags", "fixture");
        let error = schema
            .validate_result(&one_node_result(wrong_type))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("invalid field \"tags\": expected KeywordList"),
            "{error}"
        );

        let mut schema = test_schema();
        let mut sensitive = DiscoveryField::new("secret", DiscoveryFieldType::Keyword, false);
        sensitive.sensitive = true;
        schema.fields.push(sensitive);
        let error = schema
            .validate_result(&one_node_result(
                valid_document().with("secret", "must not enter discovery"),
            ))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("emitted sensitive field \"secret\""),
            "{error}"
        );
    }

    #[test]
    fn discovery_documents_must_be_one_to_one_with_graph_nodes() {
        let schema = test_schema();
        let mut result = one_node_result(valid_document());
        result.search_documents.push(valid_document());
        let error = schema.validate_result(&result).unwrap_err().to_string();
        assert!(
            error.contains("2 search documents for 1 graph nodes"),
            "{error}"
        );

        let unknown = SearchDocument::new("unknown")
            .with("id", "unknown")
            .with("title", "Unknown")
            .with("tags", json!([]));
        let error = schema
            .validate_result(&one_node_result(unknown))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("references unknown node \"unknown\""),
            "{error}"
        );
    }

    #[test]
    fn discovery_documents_match_canonical_identity_title_and_tags() {
        let schema = test_schema();

        let mismatched_id = SearchDocument::new(N1)
            .with("id", "another-id")
            .with("title", "Node one")
            .with("tags", json!(["fixture"]));
        let error = schema
            .validate_result(&one_node_result(mismatched_id))
            .unwrap_err()
            .to_string();
        assert!(error.contains("indexes mismatched id"), "{error}");

        let mismatched_title = SearchDocument::new(N1)
            .with("id", N1)
            .with("title", "Different title")
            .with("tags", json!(["fixture"]));
        let error = schema
            .validate_result(&one_node_result(mismatched_title))
            .unwrap_err()
            .to_string();
        assert!(error.contains("canonical non-empty title"), "{error}");

        let mismatched_tags = SearchDocument::new(N1)
            .with("id", N1)
            .with("title", "Node one")
            .with("tags", json!(["different"]));
        let error = schema
            .validate_result(&one_node_result(mismatched_tags))
            .unwrap_err()
            .to_string();
        assert!(error.contains("canonical tags"), "{error}");

        let mut malformed_result = one_node_result(
            SearchDocument::new(N1)
                .with("id", N1)
                .with("title", "Node one")
                .with("tags", json!(["foo//bar"])),
        );
        malformed_result
            .graph
            .nodes
            .get_mut(N1)
            .unwrap()
            .meta
            .tags = vec!["foo//bar".into()];
        let error = schema
            .validate_result(&malformed_result)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid tag \"foo//bar\""), "{error}");
    }

    trait FieldMut {
        fn field_mut(&mut self, key: &str) -> &mut DiscoveryField;
    }

    impl FieldMut for ImporterSchema {
        fn field_mut(&mut self, key: &str) -> &mut DiscoveryField {
            self.fields
                .iter_mut()
                .find(|field| field.key == key)
                .expect("test field")
        }
    }

    #[test]
    fn source_kind_accepts_okf_names() {
        assert_eq!(SourceKind::parse("okf"), Some(SourceKind::Okf));
        assert_eq!(
            SourceKind::parse("OPEN-KNOWLEDGE-FORMAT"),
            Some(SourceKind::Okf)
        );
        assert!(SourceKind::all().contains(&"okf"));
    }

    struct FakeConnector {
        trace: Trace,
        scope: String,
        records: Vec<SourceRecord>,
        error: Option<ImportError>,
    }

    impl SourceConnector for FakeConnector {
        fn capabilities(&self, effect: Effect) -> Vec<Capability> {
            vec![capability(effect, &self.scope)]
        }

        fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
            Box::pin(async move {
                self.trace.lock().unwrap().push("read".into());
                if let Some(error) = &self.error {
                    return Err(error.clone());
                }
                Ok(self.records.clone())
            })
        }

        fn write<'a>(
            &'a self,
            request: WriteRequest,
        ) -> ImportFuture<'a, Result<WriteReceipt, ImportError>> {
            Box::pin(async move {
                self.trace.lock().unwrap().push("write".into());
                Ok(WriteReceipt {
                    origin: request.origin,
                    bytes_written: request.bytes.len() as u64,
                    metadata: BTreeMap::new(),
                })
            })
        }
    }

    struct FakeDecoder {
        trace: Trace,
        fail_at: Option<String>,
    }

    impl Decoder for FakeDecoder {
        fn decode(&self, record: SourceRecord) -> Result<DecodedRecord, ImportError> {
            self.trace
                .lock()
                .unwrap()
                .push(format!("decode:{}", record.origin));
            if self.fail_at.as_deref() == Some(&record.origin) {
                return Err(ImportError::Decode {
                    origin: record.origin,
                    message: "bad record".into(),
                });
            }
            let text = String::from_utf8(record.bytes).map_err(|error| ImportError::Decode {
                origin: record.origin.clone(),
                message: error.to_string(),
            })?;
            Ok(DecodedRecord {
                origin: record.origin,
                value: json!({ "text": text }),
                metadata: record.metadata,
            })
        }
    }

    struct FakeMapper {
        trace: Trace,
    }

    impl GraphMapper for FakeMapper {
        fn map(&self, records: Vec<DecodedRecord>) -> Result<LoadResult, ImportError> {
            self.trace.lock().unwrap().push("map".into());
            let mut graph = VaultGraph::new();
            let mut search_documents = Vec::new();
            for record in records {
                let id = format!("generate:fake:{}", record.origin);
                graph.add_node(VaultNode {
                    id: id.clone(),
                    meta: vault_data::NodeMeta {
                        source_id: "fake".into(),
                        title: id.clone(),
                        tags: Vec::new(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                search_documents.push(
                    SearchDocument::new(&id)
                        .with("id", id.clone())
                        .with("title", id)
                        .with("tags", serde_json::json!([])),
                );
            }
            Ok(LoadResult {
                graph,
                search_documents,
                unresolved: Vec::new(),
            })
        }
    }

    fn record(origin: &str, body: &str) -> SourceRecord {
        SourceRecord {
            origin: origin.into(),
            content_type: "text/plain".into(),
            bytes: body.as_bytes().to_vec(),
            metadata: BTreeMap::new(),
        }
    }

    fn pipeline(
        trace: &Trace,
        records: Vec<SourceRecord>,
        fail_at: Option<&str>,
    ) -> Result<ImportPipeline, ImportError> {
        let read = capability(Effect::Read, "fixture");
        ImportPipeline::new(
            descriptor(vec![read]),
            Box::new(FakeConnector {
                trace: trace.clone(),
                scope: "fixture".into(),
                records,
                error: None,
            }),
            Box::new(FakeDecoder {
                trace: trace.clone(),
                fail_at: fail_at.map(str::to_string),
            }),
            Box::new(FakeMapper {
                trace: trace.clone(),
            }),
        )
    }

    #[tokio::test]
    async fn pipeline_runs_connector_then_decoder_then_mapper() {
        let trace = Trace::default();
        let read = capability(Effect::Read, "fixture");
        let pipeline =
            pipeline(&trace, vec![record("a", "one"), record("b", "two")], None).unwrap();
        let importer: Box<dyn Importer> =
            Box::new(HostedImporter::new(Box::new(pipeline), [read]).unwrap());

        assert_eq!(importer.descriptor().id, "fake");
        let loaded = importer.import().await.unwrap();

        assert_eq!(loaded.graph.node_count(), 2);
        assert_eq!(
            *trace.lock().unwrap(),
            ["read", "decode:a", "decode:b", "map"]
        );
    }

    #[tokio::test]
    async fn decoder_error_propagates_and_stops_mapping() {
        let trace = Trace::default();
        let pipeline = pipeline(
            &trace,
            vec![record("a", "one"), record("bad", "two")],
            Some("bad"),
        )
        .unwrap();

        let error = pipeline.run().await.unwrap_err();

        assert_eq!(
            error,
            ImportError::Decode {
                origin: "bad".into(),
                message: "bad record".into(),
            }
        );
        assert_eq!(*trace.lock().unwrap(), ["read", "decode:a", "decode:bad"]);
    }

    #[tokio::test]
    async fn pipeline_rejects_undeclared_media_type_before_decoding() {
        let trace = Trace::default();
        let mut unsupported = record("payload.json", "{}");
        unsupported.content_type = "application/json".into();
        let pipeline = pipeline(&trace, vec![unsupported], None).unwrap();

        let error = pipeline.run().await.unwrap_err();

        assert_eq!(
            error,
            ImportError::UnsupportedMediaType {
                origin: "payload.json".into(),
                content_type: "application/json".into(),
                accepted: vec!["text/plain".into()],
            }
        );
        assert_eq!(*trace.lock().unwrap(), ["read"]);
    }

    #[test]
    fn pipeline_requires_an_input_media_type_declaration() {
        let trace = Trace::default();
        let read = capability(Effect::Read, "fixture");
        let mut descriptor = descriptor(vec![read]);
        descriptor.schema.input_media_types.clear();

        let result = ImportPipeline::new(
            descriptor,
            Box::new(FakeConnector {
                trace: trace.clone(),
                scope: "fixture".into(),
                records: Vec::new(),
                error: None,
            }),
            Box::new(FakeDecoder {
                trace: trace.clone(),
                fail_at: None,
            }),
            Box::new(FakeMapper { trace }),
        );

        let Err(error) = result else {
            panic!("pipeline without an input media type must be rejected")
        };
        assert_eq!(
            error,
            ImportError::InvalidDescriptor {
                message: "connector-backed importer must declare at least one input media type"
                    .into(),
            }
        );
    }

    #[tokio::test]
    async fn pipeline_accepts_case_insensitive_media_type_with_parameters() {
        let trace = Trace::default();
        let mut parameterized = record("payload.txt", "one");
        parameterized.content_type = "Text/Plain; charset=utf-8".into();
        let pipeline = pipeline(&trace, vec![parameterized], None).unwrap();

        let loaded = pipeline.run().await.unwrap();

        assert_eq!(loaded.graph.node_count(), 1);
        assert_eq!(
            *trace.lock().unwrap(),
            ["read", "decode:payload.txt", "map"]
        );
    }

    #[tokio::test]
    async fn missing_exact_grant_denies_before_source_effect() {
        let trace = Trace::default();
        let pipeline = pipeline(&trace, vec![record("a", "one")], None).unwrap();
        let importer = HostedImporter::new(Box::new(pipeline), []).unwrap();

        let error = importer.import().await.unwrap_err();

        assert_eq!(
            error,
            ImportError::CapabilityDenied {
                capability: capability(Effect::Read, "fixture"),
            }
        );
        assert!(trace.lock().unwrap().is_empty());
    }

    #[test]
    fn declarations_do_not_grant_write_or_watch_authority() {
        let trace = Trace::default();
        let read = capability(Effect::Read, "fixture");
        let write = capability(Effect::Write, "fixture");
        let watch = capability(Effect::Watch, "fixture");
        let pipeline = ImportPipeline::new(
            descriptor(vec![read.clone(), write.clone(), watch.clone()])
                .with_watch(WatchPlan::Poll { interval_ms: 100 }),
            Box::new(FakeConnector {
                trace,
                scope: "fixture".into(),
                records: Vec::new(),
                error: None,
            }),
            Box::new(FakeDecoder {
                trace: Trace::default(),
                fail_at: None,
            }),
            Box::new(FakeMapper {
                trace: Trace::default(),
            }),
        )
        .unwrap();
        let importer = HostedImporter::new(Box::new(pipeline), [read.clone()]).unwrap();

        assert!(importer.is_authorized(&read));
        assert!(!importer.is_authorized(&write));
        assert!(!importer.is_authorized(&watch));
    }

    #[test]
    fn scope_mismatch_is_not_a_grant() {
        let trace = Trace::default();
        let pipeline = pipeline(&trace, vec![record("a", "one")], None).unwrap();
        let result = HostedImporter::new(
            Box::new(pipeline),
            [capability(Effect::Read, "other-scope")],
        );

        let Err(error) = result else {
            panic!("scope-mismatched grant must be rejected")
        };
        assert_eq!(
            error,
            ImportError::CapabilityDenied {
                capability: capability(Effect::Read, "other-scope"),
            }
        );
        assert!(trace.lock().unwrap().is_empty());
    }

    struct LegacyLoader;    impl Loader for LegacyLoader {
        fn name(&self) -> &str {
            "legacy"
        }

        fn schema(&self) -> ImporterSchema {
            test_schema()
        }

        fn load(&self) -> LoadResult {
            LoadResult {
                graph: VaultGraph::new(),
                search_documents: Vec::new(),
                unresolved: vec!["legacy diagnostic".into()],
            }
        }
    }

    #[tokio::test]
    async fn every_loader_has_async_importer_compatibility() {
        let loader = LegacyLoader;
        let descriptor = Importer::descriptor(&loader);
        let read = descriptor.capabilities[0].clone();
        let importer = HostedImporter::new(Box::new(loader), [read.clone()]).unwrap();
        let loaded = importer.import().await.unwrap();

        assert_eq!(loaded.unresolved, ["legacy diagnostic"]);
        assert_eq!(descriptor.id, "legacy");
        assert_eq!(descriptor.capabilities.len(), 1);
        assert_eq!(descriptor.capabilities[0].effect, Effect::Read);
        assert_eq!(descriptor.capabilities[0].transport, Transport::InMemory);
        assert!(importer.is_authorized(&read));
        assert!(!importer.is_authorized(&Capability::new(
            Effect::Write,
            Transport::InMemory,
            "legacy"
        )));
    }

    #[tokio::test]
    async fn shared_import_contract_harness_accepts_a_conformant_importer() {
        crate::testing::assert_import_contract(&LegacyLoader).await;
    }

    #[test]
    fn discovery_schema_v1_is_rejected() {
        let mut schema = test_schema();
        schema.schema_version = 1;
        let error = schema.validate().unwrap_err().to_string();
        assert!(
            error.contains("unsupported discovery schema version 1"),
            "{error}"
        );
    }

    #[test]
    fn discovery_schema_rejects_unknown_source_kinds() {
        for kind in ["", "Unknown", "github-enterprise"] {
            let mut schema = test_schema();
            schema.source_kind = kind.into();
            let error = schema.validate().unwrap_err().to_string();
            assert!(
                error.contains("source_kind"),
                "{kind}: {error}"
            );
        }
    }

    #[test]
    fn node_ids_must_match_the_declared_namespace() {
        let schema = test_schema();

        let namespaced_result = |node_id: &str, source_id: &str| {
            let mut graph = VaultGraph::new();
            graph.add_node(VaultNode {
                id: node_id.into(),
                meta: vault_data::NodeMeta {
                    source_id: source_id.into(),
                    title: "Node one".into(),
                    tags: vec!["fixture".into()],
                    ..Default::default()
                },
                ..Default::default()
            });
            LoadResult {
                graph,
                search_documents: vec![SearchDocument::new(node_id)
                    .with("id", node_id)
                    .with("title", "Node one")
                    .with("tags", json!(["fixture"]))],
                unresolved: Vec::new(),
            }
        };

        // Wrong kind prefix.
        let error = schema
            .validate_result(&namespaced_result("tvix:fixture:n1", "fixture"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not start with its namespace prefix"),
            "{error}"
        );

        // Empty local part.
        let error = schema
            .validate_result(&namespaced_result("generate:fixture:", "fixture"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid local id"), "{error}");

        // ':' in source_id can never produce an unambiguous namespace.
        let error = schema
            .validate_result(&namespaced_result("generate:a:b:n1", "a:b"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid source_id"), "{error}");
    }

    #[test]
    fn edge_endpoints_must_resolve_within_the_graph() {
        let schema = test_schema();
        let mut result = one_node_result(valid_document());
        result.graph.add_edge(vault_data::VaultEdge {
            source: N1.into(),
            target: "generate:fixture:missing".into(),
        });
        let error = schema.validate_result(&result).unwrap_err().to_string();
        assert!(
            error.contains("missing endpoint")
                && error.contains("generate:fixture:missing"),
            "{error}"
        );
    }
}
