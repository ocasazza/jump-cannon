//! Runtime-defined Pest importers for jump-cannon.
//!
//! An importer package is one versioned TOML file containing package metadata,
//! an inline Pest grammar, the grammar's root rule, semantic capture-rule
//! bindings, and resource limits. The package deliberately contains **no data
//! source binding**. An administrator binds a validated package to an explicit
//! file with [FilesystemLoader::new]; future HTTP, Kubernetes, protobuf, or
//! streaming adapters can feed bytes to [ValidatedPackage::parse_input]
//! without changing the grammar package format.
//!
//! The semantic capture contract maps Pest spans into the canonical graph:
//! node, id, title, kind, tag, property, key, value, edge, source, and target.
//! Capture text is used exactly as matched by the grammar. Nodes retain source
//! order, tags retain capture order, properties become string-valued
//! frontmatter, and edges retain source order. Edges with missing endpoints are
//! reported through [data_loader::LoadResult::unresolved] and are not added to
//! the graph.
//!
//! # Security boundary
//!
//! [pest_vm] has no parser fuel counter, deadline, or preemption mechanism.
//! Byte, node, and edge limits bound storage, but they cannot stop a hostile
//! grammar from consuming excessive CPU while parsing. Native execution is
//! therefore only appropriate for trusted, administrator-installed packages.
//! A UI that accepts untrusted package uploads must run validation and parsing
//! inside a separately resource-limited Wasm sandbox; it must not pass uploaded
//! grammars directly to this native loader.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use data_loader::{
    Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect,
    ImportError as PipelineError, ImportFuture, Importer, ImporterDescriptor, ImporterSchema,
    LoadResult, Loader, SearchDocument, Transport, WatchPlan,
};
use pest::iterators::Pair;
use pest_meta::ast::RuleType;
use pest_meta::optimizer::OptimizedRule;
use pest_vm::Vm;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

/// Importer package format understood by this crate.
pub const FORMAT_VERSION: u32 = 2;

/// Maximum limits accepted from an importer package.
///
/// A package may lower any of these values, but validation rejects zero values
/// and values above this ceiling.
pub const HARD_LIMITS: Limits = Limits {
    manifest_bytes: 128 * 1024,
    grammar_bytes: 64 * 1024,
    input_bytes: 32 * 1024 * 1024,
    nodes: 100_000,
    edges: 500_000,
};

/// Deserialized, not-yet-validated importer package manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImporterManifest {
    /// Package-format version. Currently only [FORMAT_VERSION] is accepted.
    pub format_version: u32,
    /// Human-facing identity and release metadata.
    pub metadata: PackageMetadata,
    /// Runtime parser configuration and inline grammar.
    pub parser: ParserConfig,
    /// Semantic names in the Pest parse tree.
    pub captures: CaptureRules,
    /// Explicit allowlist of package-specific discovery fields. Canonical
    /// id/title/tags/path/type fields are supplied by the host automatically.
    pub schema: PackageSchema,
    /// Per-package limits, bounded by [HARD_LIMITS].
    #[serde(default)]
    pub limits: Limits,
}

/// Package-specific searchable/facetable string properties.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSchema {
    #[serde(default)]
    pub fields: Vec<DiscoveryField>,
}

/// Identity and release metadata for an importer package.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    /// Stable machine-readable identifier, such as example.line-graph.
    pub id: String,
    /// Human-readable importer name.
    pub name: String,
    /// Semantic package version.
    pub version: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
}

/// Runtime Pest parser configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParserConfig {
    /// Pest rule invoked for the complete input.
    pub root_rule: String,
    /// Inline .pest grammar source.
    pub grammar: String,
}

/// Pest rule names that carry canonical graph semantics.
///
/// All names must refer to distinct, non-silent rules in the supplied grammar.
/// The metadata-related rules may be absent from a particular node match, but
/// every matched node must contain exactly one id; every matched edge must
/// contain exactly one source and one target.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRules {
    pub node: String,
    pub id: String,
    pub title: String,
    pub kind: String,
    pub tag: String,
    pub property: String,
    pub key: String,
    pub value: String,
    pub edge: String,
    pub source: String,
    pub target: String,
}

impl CaptureRules {
    fn named(&self) -> [(&'static str, &str); 11] {
        [
            ("node", &self.node),
            ("id", &self.id),
            ("title", &self.title),
            ("kind", &self.kind),
            ("tag", &self.tag),
            ("property", &self.property),
            ("key", &self.key),
            ("value", &self.value),
            ("edge", &self.edge),
            ("source", &self.source),
            ("target", &self.target),
        ]
    }
}

/// Storage limits declared by an importer package.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum UTF-8 TOML package size.
    #[serde(default = "default_manifest_bytes")]
    pub manifest_bytes: usize,
    /// Maximum inline grammar size.
    #[serde(default = "default_grammar_bytes")]
    pub grammar_bytes: usize,
    /// Maximum UTF-8 data input size.
    #[serde(default = "default_input_bytes")]
    pub input_bytes: usize,
    /// Maximum matched node records.
    #[serde(default = "default_nodes")]
    pub nodes: usize,
    /// Maximum matched edge records, including dangling edges.
    #[serde(default = "default_edges")]
    pub edges: usize,
}

const fn default_manifest_bytes() -> usize {
    HARD_LIMITS.manifest_bytes
}

const fn default_grammar_bytes() -> usize {
    HARD_LIMITS.grammar_bytes
}

const fn default_input_bytes() -> usize {
    HARD_LIMITS.input_bytes
}

const fn default_nodes() -> usize {
    HARD_LIMITS.nodes
}

const fn default_edges() -> usize {
    HARD_LIMITS.edges
}

impl Default for Limits {
    fn default() -> Self {
        HARD_LIMITS
    }
}

/// Package validation, input, parsing, and mapping failures.
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("importer manifest is {actual} bytes; limit is {max} bytes")]
    ManifestTooLarge { actual: usize, max: usize },

    #[error("importer manifest is not UTF-8: {0}")]
    ManifestUtf8(#[source] std::str::Utf8Error),

    #[error("invalid importer TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("unsupported importer format version {found}; supported version is {supported}")]
    UnsupportedFormatVersion { found: u32, supported: u32 },

    #[error("invalid importer metadata: {0}")]
    InvalidMetadata(String),

    #[error("invalid '{name}' limit {value}; value must be between 1 and {hard_max}")]
    InvalidLimit {
        name: &'static str,
        value: usize,
        hard_max: usize,
    },

    #[error("inline Pest grammar is {actual} bytes; package limit is {max} bytes")]
    GrammarTooLarge { actual: usize, max: usize },

    #[error("invalid Pest grammar: {0}")]
    Grammar(String),

    #[error("configured {role} rule '{rule}' does not exist in the grammar")]
    MissingRule { role: &'static str, rule: String },

    #[error("configured {role} rule '{rule}' is silent and cannot produce a capture")]
    SilentRule { role: &'static str, rule: String },

    #[error("capture rule '{rule}' is assigned to both {first_role} and {second_role}")]
    AmbiguousCaptureRule {
        rule: String,
        first_role: &'static str,
        second_role: &'static str,
    },

    #[error("input is {actual} bytes; package limit is {max} bytes")]
    InputTooLarge { actual: usize, max: usize },

    #[error("could not read importer input '{path}': {source}")]
    InputIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("importer input '{path}' is not UTF-8: {source}")]
    InputUtf8 {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },

    #[error("input did not match root rule '{rule}': {message}")]
    Parse { rule: String, message: String },

    #[error(
        "root rule '{rule}' matched bytes {matched} of {total}; the root must consume all input"
    )]
    PartialParse {
        rule: String,
        matched: usize,
        total: usize,
    },

    #[error("invalid {record} record: {detail}")]
    InvalidRecord {
        record: &'static str,
        detail: String,
    },

    #[error("duplicate node id '{0}'")]
    DuplicateNodeId(String),

    #[error("matched {actual} {kind}; package limit is {max}")]
    RecordLimit {
        kind: &'static str,
        actual: usize,
        max: usize,
    },
}

/// A TOML package whose metadata, limits, capture bindings, and Pest grammar
/// have been validated.
#[derive(Clone)]
pub struct ValidatedPackage {
    manifest: ImporterManifest,
    vm: Arc<Vm>,
}

impl ValidatedPackage {
    /// Parse and validate a single-file TOML importer package.
    pub fn from_toml(source: &str) -> Result<Self, ImportError> {
        Self::from_toml_bytes(source.as_bytes())
    }

    /// Parse and validate UTF-8 TOML bytes without allocating beyond the hard
    /// manifest-size ceiling.
    pub fn from_toml_bytes(source: &[u8]) -> Result<Self, ImportError> {
        if source.len() > HARD_LIMITS.manifest_bytes {
            return Err(ImportError::ManifestTooLarge {
                actual: source.len(),
                max: HARD_LIMITS.manifest_bytes,
            });
        }

        let source = std::str::from_utf8(source).map_err(ImportError::ManifestUtf8)?;
        let manifest: ImporterManifest = toml::from_str(source)?;
        validate_manifest(source.len(), manifest)
    }

    /// The validated manifest.
    pub fn manifest(&self) -> &ImporterManifest {
        &self.manifest
    }

    /// Parse one UTF-8 input and deterministically map its semantic captures to
    /// a fresh graph.
    pub fn parse_input(&self, input: &str) -> Result<LoadResult, ImportError> {
        if input.len() > self.manifest.limits.input_bytes {
            return Err(ImportError::InputTooLarge {
                actual: input.len(),
                max: self.manifest.limits.input_bytes,
            });
        }

        let root_rule = self.manifest.parser.root_rule.as_str();
        let mut pairs = self
            .vm
            .parse(root_rule, input)
            .map_err(|error| ImportError::Parse {
                rule: root_rule.to_owned(),
                message: error.to_string(),
            })?;

        let root = pairs.next().ok_or_else(|| ImportError::Parse {
            rule: root_rule.to_owned(),
            message: "parser returned no root capture".to_owned(),
        })?;

        if root.as_rule() != root_rule {
            return Err(ImportError::Parse {
                rule: root_rule.to_owned(),
                message: format!(
                    "parser returned unexpected top-level rule '{}'",
                    root.as_rule()
                ),
            });
        }

        let span = root.as_span();
        let matched = span.end().saturating_sub(span.start());
        if span.start() != 0 || span.end() != input.len() {
            return Err(ImportError::PartialParse {
                rule: root_rule.to_owned(),
                matched,
                total: input.len(),
            });
        }

        let mut mapped = MappedGraph::default();
        self.collect_records(root, &mut mapped)?;

        let mut unresolved = Vec::new();
        for edge in mapped.edges {
            let source_exists = mapped.graph.nodes.contains_key(&edge.source);
            let target_exists = mapped.graph.nodes.contains_key(&edge.target);
            if source_exists && target_exists {
                mapped.graph.add_edge(edge);
            } else {
                let missing = match (source_exists, target_exists) {
                    (false, false) => "source and target",
                    (false, true) => "source",
                    (true, false) => "target",
                    (true, true) => unreachable!(),
                };
                unresolved.push(format!(
                    "edge '{}' -> '{}' has missing {missing}",
                    edge.source, edge.target
                ));
            }
        }

        let search_documents = mapped
            .graph
            .nodes
            .values()
            .map(|node| self.search_document(node))
            .collect();

        Ok(LoadResult {
            graph: mapped.graph,
            search_documents,
            unresolved,
        })
    }

    fn search_document(&self, node: &VaultNode) -> SearchDocument {
        let mut document = SearchDocument::new(&node.id)
            .with("id", node.id.clone())
            .with("title", node.meta.title.clone())
            .with("tags", serde_json::json!(node.meta.tags))
            .with("path", node.meta.path.clone());
        if let Some(kind) = &node.meta.doctype {
            document.insert("type", kind.clone());
        }
        for field in &self.manifest.schema.fields {
            if let Some(value) = node.meta.frontmatter.get(&field.key) {
                let keep = field.field_type == DiscoveryFieldType::Text
                    || value.as_str().is_some_and(|value| !value.trim().is_empty());
                if keep {
                    document.insert(&field.key, value.clone());
                }
            }
        }
        document
    }

    fn collect_records<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        mapped: &mut MappedGraph,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.manifest.captures;

        if rule == captures.node {
            let actual = mapped.graph.node_count() + 1;
            if actual > self.manifest.limits.nodes {
                return Err(ImportError::RecordLimit {
                    kind: "nodes",
                    actual,
                    max: self.manifest.limits.nodes,
                });
            }

            let node = self.map_node(pair)?;
            if mapped.graph.nodes.contains_key(&node.id) {
                return Err(ImportError::DuplicateNodeId(node.id));
            }
            mapped.graph.add_node(node);
            return Ok(());
        }

        if rule == captures.edge {
            let actual = mapped.edges.len() + 1;
            if actual > self.manifest.limits.edges {
                return Err(ImportError::RecordLimit {
                    kind: "edges",
                    actual,
                    max: self.manifest.limits.edges,
                });
            }

            mapped.edges.push(self.map_edge(pair)?);
            return Ok(());
        }

        for child in pair.into_inner() {
            self.collect_records(child, mapped)?;
        }
        Ok(())
    }

    fn map_node<'i, 'r>(&self, pair: Pair<'i, &'r str>) -> Result<VaultNode, ImportError> {
        let mut fields = NodeFields::default();
        for child in pair.into_inner() {
            self.collect_node_fields(child, &mut fields)?;
        }

        let id = fields
            .id
            .ok_or_else(|| invalid_record("node", "missing id capture"))?;
        if id.is_empty() {
            return Err(invalid_record("node", "id capture is empty"));
        }

        let title = fields.title.unwrap_or_else(|| id.clone());
        fields.tags.sort();
        fields.tags.dedup();
        let meta = NodeMeta {
            source_id: self.manifest.metadata.id.clone(),
            title,
            tags: fields.tags,
            frontmatter: fields.properties,
            mtime: 0,
            path: id.clone(),
            doctype: fields.kind,
            folder: String::new(),
            content_type: None,
            content_readable: false,
            content_writable: false,
        };

        Ok(VaultNode {
            id,
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        })
    }

    fn collect_node_fields<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        fields: &mut NodeFields,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.manifest.captures;

        if rule == captures.id {
            return set_scalar(&mut fields.id, pair.as_str(), "node", "id");
        }
        if rule == captures.title {
            return set_scalar(&mut fields.title, pair.as_str(), "node", "title");
        }
        if rule == captures.kind {
            return set_scalar(&mut fields.kind, pair.as_str(), "node", "kind");
        }
        if rule == captures.tag {
            if pair.as_str().is_empty() {
                return Err(invalid_record("node", "tag capture is empty"));
            }
            fields.tags.push(pair.as_str().to_owned());
            return Ok(());
        }
        if rule == captures.property {
            let (key, value) = self.map_property(pair)?;
            if fields
                .properties
                .insert(key.clone(), Value::String(value))
                .is_some()
            {
                return Err(invalid_record(
                    "node",
                    format!("duplicate property key '{key}'"),
                ));
            }
            return Ok(());
        }
        if rule == captures.node || rule == captures.edge {
            return Err(invalid_record(
                "node",
                format!("nested graph record '{rule}' is not allowed"),
            ));
        }

        for child in pair.into_inner() {
            self.collect_node_fields(child, fields)?;
        }
        Ok(())
    }

    fn map_property<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
    ) -> Result<(String, String), ImportError> {
        let mut key = None;
        let mut value = None;
        self.collect_property_fields(pair, &mut key, &mut value)?;

        let key = key.ok_or_else(|| invalid_record("property", "missing key capture"))?;
        let value = value.ok_or_else(|| invalid_record("property", "missing value capture"))?;
        if key.is_empty() {
            return Err(invalid_record("property", "key capture is empty"));
        }
        Ok((key, value))
    }

    fn collect_property_fields<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        key: &mut Option<String>,
        value: &mut Option<String>,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.manifest.captures;
        if rule == captures.key {
            return set_scalar(key, pair.as_str(), "property", "key");
        }
        if rule == captures.value {
            return set_scalar(value, pair.as_str(), "property", "value");
        }

        for child in pair.into_inner() {
            self.collect_property_fields(child, key, value)?;
        }
        Ok(())
    }

    fn map_edge<'i, 'r>(&self, pair: Pair<'i, &'r str>) -> Result<VaultEdge, ImportError> {
        let mut source = None;
        let mut target = None;
        for child in pair.into_inner() {
            self.collect_edge_fields(child, &mut source, &mut target)?;
        }

        let source = source.ok_or_else(|| invalid_record("edge", "missing source capture"))?;
        let target = target.ok_or_else(|| invalid_record("edge", "missing target capture"))?;
        if source.is_empty() || target.is_empty() {
            return Err(invalid_record(
                "edge",
                "source and target captures must be non-empty",
            ));
        }
        Ok(VaultEdge { source, target })
    }

    fn collect_edge_fields<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        source: &mut Option<String>,
        target: &mut Option<String>,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.manifest.captures;
        if rule == captures.source {
            return set_scalar(source, pair.as_str(), "edge", "source");
        }
        if rule == captures.target {
            return set_scalar(target, pair.as_str(), "edge", "target");
        }
        if rule == captures.node || rule == captures.edge {
            return Err(invalid_record(
                "edge",
                format!("nested graph record '{rule}' is not allowed"),
            ));
        }

        for child in pair.into_inner() {
            self.collect_edge_fields(child, source, target)?;
        }
        Ok(())
    }
}

/// A trusted, validated package bound by the administrator to one filesystem
/// input. The input path is runtime configuration and is never read from the
/// package itself.
pub struct FilesystemLoader {
    package: ValidatedPackage,
    input_path: PathBuf,
}

impl FilesystemLoader {
    /// Bind a validated package to an explicit input path.
    pub fn new(package: ValidatedPackage, input_path: impl Into<PathBuf>) -> Self {
        Self {
            package,
            input_path: input_path.into(),
        }
    }

    /// Read the bounded UTF-8 file and return parse/mapping errors to callers
    /// that can handle a fallible load operation.
    pub fn load_checked(&self) -> Result<LoadResult, ImportError> {
        let bytes = read_bounded(&self.input_path, self.package.manifest.limits.input_bytes)?;
        let input = String::from_utf8(bytes).map_err(|source| ImportError::InputUtf8 {
            path: self.input_path.clone(),
            source,
        })?;
        self.package.parse_input(&input)
    }

    /// Validated package bound to this loader.
    pub fn package(&self) -> &ValidatedPackage {
        &self.package
    }

    /// Explicit administrator-configured input path.
    pub fn input_path(&self) -> &Path {
        &self.input_path
    }
}

impl Loader for FilesystemLoader {
    fn name(&self) -> &str {
        &self.package.manifest.metadata.name
    }

    fn schema(&self) -> ImporterSchema {
        pest_schema(self.package.manifest.schema.fields.clone())
    }

    fn load(&self) -> LoadResult {
        self.load_checked().unwrap_or_else(|error| LoadResult {
            graph: VaultGraph::new(),
            search_documents: Vec::new(),
            unresolved: vec![format!(
                "importer '{}' failed: {error}",
                self.package.manifest.metadata.id
            )],
        })
    }

    fn root_path(&self) -> Option<&PathBuf> {
        Some(&self.input_path)
    }
}

/// Fallible async importer wrapper used by graph-api.
///
/// [`FilesystemLoader`] preserves the original synchronous `Loader` contract,
/// whose only diagnostic channel is `LoadResult::unresolved`. This wrapper
/// keeps package/I/O/parse failures on the typed importer error path so a
/// failed reload cannot publish an empty graph.
pub struct FilesystemImporter {
    loader: FilesystemLoader,
}

impl FilesystemImporter {
    pub fn new(package: ValidatedPackage, input_path: impl Into<PathBuf>) -> Self {
        Self {
            loader: FilesystemLoader::new(package, input_path),
        }
    }

    pub fn package(&self) -> &ValidatedPackage {
        self.loader.package()
    }

    pub fn input_path(&self) -> &Path {
        self.loader.input_path()
    }
}

impl Importer for FilesystemImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let metadata = &self.loader.package.manifest.metadata;
        let scope = self.loader.input_path.to_string_lossy().into_owned();
        let read = Capability::new(Effect::Read, Transport::Filesystem, scope.clone());
        let watch = Capability::new(Effect::Watch, Transport::Filesystem, scope);
        ImporterDescriptor::new(
            &metadata.id,
            &metadata.name,
            &metadata.version,
            vec![read, watch],
            pest_schema(self.loader.package.manifest.schema.fields.clone()),
        )
        .with_watch(WatchPlan::Filesystem {
            root: self.loader.input_path.clone(),
        })
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, PipelineError>> {
        Box::pin(async move {
            self.loader
                .load_checked()
                .map_err(|error| PipelineError::Decode {
                    origin: self.loader.input_path.display().to_string(),
                    message: error.to_string(),
                })
        })
    }
}

#[derive(Default)]
struct MappedGraph {
    graph: VaultGraph,
    edges: Vec<VaultEdge>,
}

#[derive(Default)]
struct NodeFields {
    id: Option<String>,
    title: Option<String>,
    kind: Option<String>,
    tags: Vec<String>,
    properties: HashMap<String, Value>,
}

fn validate_manifest(
    manifest_source_bytes: usize,
    manifest: ImporterManifest,
) -> Result<ValidatedPackage, ImportError> {
    if manifest.format_version != FORMAT_VERSION {
        return Err(ImportError::UnsupportedFormatVersion {
            found: manifest.format_version,
            supported: FORMAT_VERSION,
        });
    }

    validate_metadata(&manifest.metadata)?;
    validate_limits(manifest.limits)?;
    validate_package_schema(&manifest.schema)?;

    if manifest_source_bytes > manifest.limits.manifest_bytes {
        return Err(ImportError::ManifestTooLarge {
            actual: manifest_source_bytes,
            max: manifest.limits.manifest_bytes,
        });
    }
    if manifest.parser.grammar.len() > manifest.limits.grammar_bytes {
        return Err(ImportError::GrammarTooLarge {
            actual: manifest.parser.grammar.len(),
            max: manifest.limits.grammar_bytes,
        });
    }
    if manifest.parser.root_rule.is_empty() {
        return Err(ImportError::InvalidMetadata(
            "parser.root_rule must not be empty".to_owned(),
        ));
    }

    let (_, optimized) = pest_meta::parse_and_optimize(&manifest.parser.grammar)
        .map_err(|errors| ImportError::Grammar(join_errors(errors)))?;
    validate_rule_bindings(&manifest, &optimized)?;

    Ok(ValidatedPackage {
        manifest,
        vm: Arc::new(Vm::new(optimized)),
    })
}

fn validate_metadata(metadata: &PackageMetadata) -> Result<(), ImportError> {
    if metadata.id.is_empty()
        || !metadata
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        return Err(ImportError::InvalidMetadata(
            "metadata.id must be a non-empty ASCII identifier using letters, digits, '.', '-', or '_'"
                .to_owned(),
        ));
    }
    if metadata.name.trim().is_empty() {
        return Err(ImportError::InvalidMetadata(
            "metadata.name must not be empty".to_owned(),
        ));
    }
    semver::Version::parse(&metadata.version).map_err(|error| {
        ImportError::InvalidMetadata(format!(
            "metadata.version '{}' is not semantic versioning: {error}",
            metadata.version
        ))
    })?;
    Ok(())
}

fn validate_limits(limits: Limits) -> Result<(), ImportError> {
    for (name, value, hard_max) in [
        (
            "manifest_bytes",
            limits.manifest_bytes,
            HARD_LIMITS.manifest_bytes,
        ),
        (
            "grammar_bytes",
            limits.grammar_bytes,
            HARD_LIMITS.grammar_bytes,
        ),
        ("input_bytes", limits.input_bytes, HARD_LIMITS.input_bytes),
        ("nodes", limits.nodes, HARD_LIMITS.nodes),
        ("edges", limits.edges, HARD_LIMITS.edges),
    ] {
        if value == 0 || value > hard_max {
            return Err(ImportError::InvalidLimit {
                name,
                value,
                hard_max,
            });
        }
    }
    Ok(())
}

fn validate_package_schema(schema: &PackageSchema) -> Result<(), ImportError> {
    const CORE_KEYS: &[&str] = &["id", "title", "tags", "path", "type"];
    for field in &schema.fields {
        if CORE_KEYS.contains(&field.key.as_str()) {
            return Err(ImportError::InvalidMetadata(format!(
                "schema field {:?} collides with a canonical field",
                field.key
            )));
        }
        if !matches!(
            field.field_type,
            DiscoveryFieldType::Text
                | DiscoveryFieldType::Keyword
                | DiscoveryFieldType::Date
                | DiscoveryFieldType::Url
        ) {
            return Err(ImportError::InvalidMetadata(format!(
                "Pest property field {:?} must be text, keyword, date, or url",
                field.key
            )));
        }
        if field.sensitive {
            return Err(ImportError::InvalidMetadata(format!(
                "sensitive property {:?} must not enter the discovery schema",
                field.key
            )));
        }
    }
    pest_schema(schema.fields.clone())
        .validate()
        .map_err(|error| ImportError::InvalidMetadata(error.to_string()))
}

fn pest_schema(package_fields: Vec<DiscoveryField>) -> ImporterSchema {
    let mut fields = vec![
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
    ];
    fields.extend(package_fields);
    ImporterSchema::new(
        fields,
        vec![EdgeTypeSchema::directed(
            "declared",
            "Directed edge emitted by the package capture map",
        )],
    )
    .with_input_media_types(["text/plain"])
}

fn validate_rule_bindings(
    manifest: &ImporterManifest,
    optimized: &[OptimizedRule],
) -> Result<(), ImportError> {
    let mut rules = HashMap::new();
    for rule in optimized {
        rules.insert(rule.name.as_str(), rule.ty);
    }

    validate_bound_rule("root", &manifest.parser.root_rule, &rules)?;

    let mut assigned = HashMap::new();
    for (role, name) in manifest.captures.named() {
        validate_bound_rule(role, name, &rules)?;
        if let Some(first_role) = assigned.insert(name, role) {
            return Err(ImportError::AmbiguousCaptureRule {
                rule: name.to_owned(),
                first_role,
                second_role: role,
            });
        }
    }
    Ok(())
}

fn validate_bound_rule(
    role: &'static str,
    name: &str,
    rules: &HashMap<&str, RuleType>,
) -> Result<(), ImportError> {
    let rule_type = rules.get(name).ok_or_else(|| ImportError::MissingRule {
        role,
        rule: name.to_owned(),
    })?;
    if *rule_type == RuleType::Silent {
        return Err(ImportError::SilentRule {
            role,
            rule: name.to_owned(),
        });
    }
    Ok(())
}

fn join_errors<E: std::fmt::Display>(errors: Vec<E>) -> String {
    errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn set_scalar(
    slot: &mut Option<String>,
    value: &str,
    record: &'static str,
    role: &'static str,
) -> Result<(), ImportError> {
    if slot.replace(value.to_owned()).is_some() {
        return Err(invalid_record(record, format!("multiple {role} captures")));
    }
    Ok(())
}

fn invalid_record(record: &'static str, detail: impl Into<String>) -> ImportError {
    ImportError::InvalidRecord {
        record,
        detail: detail.into(),
    }
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, ImportError> {
    let mut file = File::open(path).map_err(|source| ImportError::InputIo {
        path: path.to_owned(),
        source,
    })?;
    let read_limit = max.saturating_add(1);
    let mut bytes = Vec::with_capacity(read_limit.min(64 * 1024));
    file.by_ref()
        .take(read_limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ImportError::InputIo {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() > max {
        return Err(ImportError::InputTooLarge {
            actual: bytes.len(),
            max,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const GRAMMAR: &str = r#"
document = { SOI ~ (record ~ NEWLINE?)* ~ EOI }
record = _{ node | edge }
node = { "N|" ~ node_id ~ "|" ~ title ~ "|" ~ kind ~ "|" ~ tags ~ "|" ~ properties }
node_id = @{ field }
title = @{ field }
kind = @{ field }
tags = _{ (tag ~ ("," ~ tag)*)? }
tag = @{ atom }
properties = _{ (property ~ (";" ~ property)*)? }
property = { key ~ "=" ~ value }
key = @{ atom }
value = @{ atom }
edge = { "E|" ~ source ~ "|" ~ target }
source = @{ field }
target = @{ field }
field = _{ (!("|" | NEWLINE) ~ ANY)+ }
atom = _{ (!("," | ";" | "=" | "|" | NEWLINE) ~ ANY)+ }
"#;

    fn manifest(extra_limits: &str) -> String {
        format!(
            r#"format_version = 2

[metadata]
id = "example.line-graph"
name = "Line graph"
version = "1.2.3"
description = "Test line-oriented graph"

[parser]
root_rule = "document"
grammar = '''{GRAMMAR}'''

[captures]
node = "node"
id = "node_id"
title = "title"
kind = "kind"
tag = "tag"
property = "property"
key = "key"
value = "value"
edge = "edge"
source = "source"
target = "target"

[[schema.fields]]
key = "owner"
field_type = "keyword"
required = false
searchable = true
facetable = true

[[schema.fields]]
key = "zone"
field_type = "keyword"
required = false
searchable = true
facetable = true

[[schema.fields]]
key = "state"
field_type = "keyword"
required = false
searchable = true
facetable = true
{extra_limits}
"#
        )
    }

    fn package() -> ValidatedPackage {
        ValidatedPackage::from_toml(&manifest("")).expect("valid test package")
    }

    #[test]
    fn validates_inline_runtime_grammar_and_metadata() {
        let package = package();
        assert_eq!(package.manifest().format_version, FORMAT_VERSION);
        assert_eq!(package.manifest().metadata.id, "example.line-graph");
        assert_eq!(package.manifest().metadata.version, "1.2.3");
        assert_eq!(package.manifest().parser.root_rule, "document");
    }

    #[test]
    fn rejects_unknown_format_and_bad_grammar() {
        let unknown = manifest("").replacen("format_version = 2", "format_version = 3", 1);
        assert!(matches!(
            ValidatedPackage::from_toml(&unknown),
            Err(ImportError::UnsupportedFormatVersion { .. })
        ));

        let bad = manifest("").replace("target = @{ field }", "target = @{ missing_rule }");
        assert!(matches!(
            ValidatedPackage::from_toml(&bad),
            Err(ImportError::Grammar(_))
        ));
    }

    #[test]
    fn rejects_v1_packages_that_do_not_declare_search_schema() {
        let v1 = manifest("").replacen("format_version = 2", "format_version = 1", 1);
        assert!(matches!(
            ValidatedPackage::from_toml(&v1),
            Err(ImportError::UnsupportedFormatVersion {
                found: 1,
                supported: FORMAT_VERSION
            })
        ));
    }

    #[test]
    fn maps_good_graph_and_metadata_deterministically() {
        let input = concat!(
            "N|n1|Alpha|service|red,prod|owner=platform;zone=west\n",
            "N|n2|Beta|database||state=ready\n",
            "E|n1|n2"
        );
        let result = package().parse_input(input).expect("input parses");

        assert!(result.unresolved.is_empty());
        assert_eq!(result.graph.node_count(), 2);
        assert_eq!(result.graph.edge_count(), 1);
        assert_eq!(
            result
                .graph
                .nodes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["n1", "n2"]
        );

        let n1 = result.graph.nodes.get("n1").expect("n1");
        assert_eq!(n1.meta.title, "Alpha");
        assert_eq!(n1.meta.doctype.as_deref(), Some("service"));
        assert_eq!(n1.meta.tags, vec!["prod".to_owned(), "red".to_owned()]);
        assert_eq!(n1.meta.path, "n1");
        assert_eq!(
            n1.meta.frontmatter["owner"],
            Value::String("platform".into())
        );
        assert_eq!(n1.meta.frontmatter["zone"], Value::String("west".into()));

        let edge = &result.graph.edges[0];
        assert_eq!(edge.source, "n1");
        assert_eq!(edge.target, "n2");

        let schema = pest_schema(package().manifest().schema.fields.clone());
        schema.validate_result(&result).unwrap();
    }

    #[test]
    fn undeclared_properties_remain_metadata_but_never_enter_search_documents() {
        let package = package();
        let result = package
            .parse_input("N|n1|Alpha|service|prod|owner=platform;private=secret")
            .unwrap();
        let node = &result.graph.nodes["n1"];
        let document = &result.search_documents[0];

        assert_eq!(node.meta.frontmatter["private"], "secret");
        assert_eq!(document.fields["owner"], "platform");
        assert!(!document.fields.contains_key("private"));
        assert!(!document
            .fields
            .values()
            .any(|value| value.as_str() == Some("secret")));
        pest_schema(package.manifest().schema.fields.clone())
            .validate_result(&result)
            .unwrap();
    }

    #[test]
    fn duplicate_node_ids_are_rejected() {
        let error = package()
            .parse_input("N|same|One|kind||\nN|same|Two|kind||")
            .expect_err("duplicate must fail");
        assert!(matches!(error, ImportError::DuplicateNodeId(id) if id == "same"));
    }

    #[test]
    fn dangling_edges_are_unresolved_and_not_added() {
        let result = package()
            .parse_input("N|n1|One|kind||\nE|n1|missing")
            .expect("dangling edges do not fail the load");
        assert_eq!(result.graph.node_count(), 1);
        assert_eq!(result.graph.edge_count(), 0);
        assert_eq!(
            result.unresolved,
            ["edge 'n1' -> 'missing' has missing target"]
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        let error = package()
            .parse_input("this is not a graph record")
            .expect_err("malformed input must fail");
        assert!(matches!(error, ImportError::Parse { .. }));
    }

    #[test]
    fn input_node_and_edge_limits_are_enforced() {
        let limited = ValidatedPackage::from_toml(&manifest(
            "\n[limits]\ninput_bytes = 80\nnodes = 1\nedges = 1\n",
        ))
        .expect("limited package validates");

        assert!(matches!(
            limited.parse_input("N|n1|One|kind||\nN|n2|Two|kind||"),
            Err(ImportError::RecordLimit {
                kind: "nodes",
                actual: 2,
                max: 1
            })
        ));
        assert!(matches!(
            limited.parse_input("N|n1|One|kind||\nE|n1|n1\nE|n1|n1"),
            Err(ImportError::RecordLimit {
                kind: "edges",
                actual: 2,
                max: 1
            })
        ));
        assert!(matches!(
            limited.parse_input(&"x".repeat(81)),
            Err(ImportError::InputTooLarge {
                actual: 81,
                max: 80
            })
        ));
    }

    #[test]
    fn manifest_and_grammar_limits_are_enforced() {
        let oversized = vec![b'x'; HARD_LIMITS.manifest_bytes + 1];
        assert!(matches!(
            ValidatedPackage::from_toml_bytes(&oversized),
            Err(ImportError::ManifestTooLarge { .. })
        ));

        let grammar_limited = manifest("\n[limits]\ngrammar_bytes = 16\n");
        assert!(matches!(
            ValidatedPackage::from_toml(&grammar_limited),
            Err(ImportError::GrammarTooLarge { max: 16, .. })
        ));
    }

    #[test]
    fn filesystem_loader_binds_explicit_path_and_implements_loader() {
        let mut file = tempfile::NamedTempFile::new().expect("temp input");
        write!(file, "N|n1|One|kind||").expect("write input");
        let path = file.path().to_owned();
        let loader = FilesystemLoader::new(package(), &path);

        assert_eq!(loader.name(), "Line graph");
        assert_eq!(loader.root_path(), Some(&path));
        let schema = loader.schema();
        assert!(schema.field("owner").unwrap().searchable);
        assert!(schema.field("zone").unwrap().facetable);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 1);
        schema.validate_result(&result).unwrap();

        let importer = FilesystemImporter::new(package(), &path);
        let descriptor = importer.descriptor();
        descriptor.validate().unwrap();
        assert_eq!(descriptor.schema, schema);
    }

    #[test]
    fn loader_reports_parse_failures_through_load_result() {
        let mut file = tempfile::NamedTempFile::new().expect("temp input");
        write!(file, "not valid").expect("write input");
        let loader = FilesystemLoader::new(package(), file.path());
        let result = loader.load();

        assert_eq!(result.graph.node_count(), 0);
        assert_eq!(result.unresolved.len(), 1);
        assert!(result.unresolved[0].contains("example.line-graph"));
    }
}
