//! The `pest` engine: runtime Pest grammar packages.
//!
//! The `[parser]` table of a `format_version = 3` package carries
//! `{ root_rule, grammar, [captures] }` — an inline Pest grammar, its root
//! rule, and the semantic capture-rule bindings. The semantic capture
//! contract maps Pest spans into the canonical graph: node, id, title, kind,
//! tag, property, key, value, edge, source, target, and the optional
//! `edge_kind` and `content`. Capture text is used exactly as matched by the
//! grammar. Nodes retain source order, tags retain capture order, properties
//! become string-valued frontmatter, and edges retain source order. An edge
//! whose `edge_kind` capture names a kind the package's `[[schema.edge_types]]`
//! does not declare fails the import. Edges with missing endpoints are
//! reported through [data_loader::LoadResult::unresolved] and are not added to
//! the graph.
//!
//! The grammar package format is deliberately free of any data-source
//! binding: [`ValidatedPackage::parse_input`](crate::ValidatedPackage::parse_input)
//! accepts caller-supplied bytes on any target (including wasm32), and the
//! `native`-gated [`FilesystemLoader`]/[`FilesystemImporter`] bind a package
//! to an explicit administrator-configured path.

use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "native")]
use std::fs::File;
#[cfg(feature = "native")]
use std::io::Read;
#[cfg(feature = "native")]
use std::path::{Path, PathBuf};

use data_loader::{
    identity::Namespace, DiscoveryFieldType, ImporterSchema, LoadResult, SearchDocument,
};
#[cfg(feature = "native")]
use data_loader::{
    Capability, Effect, ImportError as PipelineError, ImportFuture, ImportOutcome, ImportProgress,
    Importer,
    ImporterDescriptor, Loader, Transport, WatchPlan,
};
use pest::iterators::Pair;
use pest_meta::ast::RuleType;
use pest_meta::optimizer::OptimizedRule;
use pest_vm::Vm;
use serde::Deserialize;
use serde_json::Value;
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

use crate::{
    EngineRuntime, ImportError, ImporterManifest, Limits, PackageSchema, ParserConfig,
    ValidatedPackage,
};

/// The `[parser] engine` tag selecting this engine.
pub const ENGINE: &str = "pest";

/// Runtime Pest parser configuration: the root rule, the inline grammar, and
/// the capture-rule bindings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PestEngineConfig {
    /// Pest rule invoked for the complete input.
    pub root_rule: String,
    /// Inline .pest grammar source.
    pub grammar: String,
    /// Semantic names in the Pest parse tree.
    pub captures: CaptureRules,
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
    /// Optional per-edge kind capture. When bound and matched, the captured
    /// text becomes the edge's `kind` and must be one of the package's
    /// declared `[[schema.edge_types]]` keys when any are declared. Edges
    /// whose match carries no capture stay untyped.
    #[serde(default)]
    pub edge_kind: Option<String>,
    /// Optional per-node body capture. When bound, matched nodes carry a
    /// readable markdown body: the schema advertises
    /// `content.readable = true`, the importer gains a scoped `ContentRead`
    /// capability, and the captured text is served through
    /// `Importer::read_body`. Omit it for metadata-only packages.
    #[serde(default)]
    pub content: Option<String>,
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

    /// The optional rules join the fixed eleven in the distinct-rule
    /// validation.
    fn optional_named(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.edge_kind
            .iter()
            .map(|rule| ("edge_kind", rule.as_str()))
            .chain(self.content.iter().map(|rule| ("content", rule.as_str())))
    }

    /// Whether this package captures per-node bodies.
    fn captures_content(&self) -> bool {
        self.content.is_some()
    }
}

/// Wire shape of a pest-engine package: the shared envelope plus the
/// engine-tagged `[parser]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PestManifestToml {
    pub format_version: u32,
    pub metadata: crate::PackageMetadata,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub schema: PackageSchema,
    pub parser: PestParserToml,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PestParserToml {
    pub engine: String,
    pub root_rule: String,
    pub grammar: String,
    pub captures: CaptureRules,
}

/// Prebuilt pest execution state: the optimized grammar VM and the node-ID
/// namespace derived from the package id.
#[derive(Clone)]
pub(crate) struct PestRuntime {
    vm: Arc<Vm>,
    namespace: Namespace,
}

/// Parse the full document, validate the shared envelope and the pest engine
/// configuration, and prebuild the runtime VM.
pub(crate) fn validate(source: &str) -> Result<ValidatedPackage, ImportError> {
    let wire: PestManifestToml = toml::from_str(source)?;
    debug_assert_eq!(wire.format_version, crate::FORMAT_VERSION);
    debug_assert_eq!(wire.parser.engine, ENGINE);

    ValidatedPackage::validate_metadata(&wire.metadata)?;
    ValidatedPackage::validate_limits(wire.limits)?;
    validate_package_schema(&wire.schema)?;
    let namespace = Namespace::new(ENGINE, &wire.metadata.id)
        .map_err(|error| ImportError::InvalidMetadata(error.to_string()))?;

    if source.len() > wire.limits.manifest_bytes {
        return Err(ImportError::ManifestTooLarge {
            actual: source.len(),
            max: wire.limits.manifest_bytes,
        });
    }
    if wire.parser.grammar.len() > wire.limits.grammar_bytes {
        return Err(ImportError::GrammarTooLarge {
            actual: wire.parser.grammar.len(),
            max: wire.limits.grammar_bytes,
        });
    }
    if wire.parser.root_rule.is_empty() {
        return Err(ImportError::InvalidMetadata(
            "parser.root_rule must not be empty".to_owned(),
        ));
    }

    let config = PestEngineConfig {
        root_rule: wire.parser.root_rule,
        grammar: wire.parser.grammar,
        captures: wire.parser.captures,
    };

    let (_, optimized) = pest_meta::parse_and_optimize(&config.grammar)
        .map_err(|errors| ImportError::Grammar(join_errors(errors)))?;
    validate_rule_bindings(&config, &optimized)?;

    let manifest = ImporterManifest {
        format_version: wire.format_version,
        metadata: wire.metadata,
        limits: wire.limits,
        schema: wire.schema,
        parser: ParserConfig::Pest(config),
    };
    let schema = pest_schema(&manifest);
    Ok(ValidatedPackage {
        manifest,
        schema,
        runtime: EngineRuntime::Pest(PestRuntime {
            vm: Arc::new(Vm::new(optimized)),
            namespace,
        }),
    })
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
    Ok(())
}

/// Canonical pest fields plus the package's declared additions. A package
/// that declares no edge types publishes the built-in directed `declared`
/// edge type.
fn pest_schema(manifest: &ImporterManifest) -> ImporterSchema {
    let mut fields = vec![
        data_loader::DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
        data_loader::DiscoveryField::new("title", DiscoveryFieldType::Text, true)
            .searchable(4)
            .snippet(),
        data_loader::DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
            .searchable(3)
            .facetable(),
        data_loader::DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(2),
        data_loader::DiscoveryField::new("type", DiscoveryFieldType::Keyword, false)
            .searchable(2)
            .facetable(),
    ];
    fields.extend(manifest.schema.fields.iter().cloned());
    let edge_types = if manifest.schema.edge_types.is_empty() {
        vec![data_loader::EdgeTypeSchema::directed(
            "declared",
            "Directed edge emitted by the package capture map",
        )]
    } else {
        manifest.schema.edge_types.clone()
    };
    ImporterSchema::new(
        ENGINE,
        fields,
        edge_types,
        data_loader::TagHierarchySchema::slash(),
    )
    .with_input_media_types(["text/plain"])
    .with_content(if captures_content(manifest) {
        data_loader::ContentSchema {
            readable: true,
            writable: false,
            media_types: vec!["text/markdown".to_owned()],
        }
    } else {
        data_loader::ContentSchema {
            readable: false,
            writable: false,
            media_types: Vec::new(),
        }
    })
}

/// Whether the package's parser binds the optional `content` capture.
fn captures_content(manifest: &ImporterManifest) -> bool {
    match &manifest.parser {
        ParserConfig::Pest(config) => config.captures.captures_content(),
        _ => false,
    }
}

fn validate_rule_bindings(
    config: &PestEngineConfig,
    optimized: &[OptimizedRule],
) -> Result<(), ImportError> {
    let mut rules = HashMap::new();
    for rule in optimized {
        rules.insert(rule.name.as_str(), rule.ty);
    }

    validate_bound_rule("root", &config.root_rule, &rules)?;

    let mut assigned = HashMap::new();
    for (role, name) in config
        .captures
        .named()
        .into_iter()
        .chain(config.captures.optional_named())
    {
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

/// Parse one UTF-8 input and deterministically map its semantic captures to a
/// fresh graph. Engine dispatch happens in
/// [`ValidatedPackage::parse_input`](crate::ValidatedPackage::parse_input).
pub(crate) fn parse_input(
    package: &ValidatedPackage,
    runtime: &PestRuntime,
    input: &str,
) -> Result<LoadResult, ImportError> {
    let (result, _bodies) = parse_input_with_bodies(package, runtime, input)?;
    Ok(result)
}

/// Same mapping as [`parse_input`], additionally returning the node bodies
/// captured by the optional `content` rule, keyed by local capture id.
pub(crate) fn parse_input_with_bodies(
    package: &ValidatedPackage,
    runtime: &PestRuntime,
    input: &str,
) -> Result<(LoadResult, HashMap<String, String>), ImportError> {
    if input.len() > package.manifest.limits.input_bytes {
        return Err(ImportError::InputTooLarge {
            actual: input.len(),
            max: package.manifest.limits.input_bytes,
        });
    }

    let engine = PestEngine {
        package,
        runtime,
        config: package
            .pest_config()
            .expect("parse_input is only reached for pest packages"),
    };

    let root_rule = engine.config.root_rule.as_str();
    let mut pairs = runtime
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
    engine.collect_records(root, &mut mapped)?;

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
        .map(|node| engine.search_document(node))
        .collect();

    let bodies = mapped.bodies;
    let result = LoadResult {
        graph: mapped.graph,
        search_documents,
        unresolved,
    };
    Ok((result, bodies))
}

/// Borrowed mapping context: the validated package, its pest configuration,
/// and the prebuilt runtime.
struct PestEngine<'a> {
    package: &'a ValidatedPackage,
    runtime: &'a PestRuntime,
    config: &'a PestEngineConfig,
}

impl PestEngine<'_> {
    fn search_document(&self, node: &VaultNode) -> SearchDocument {
        let mut document = SearchDocument::new(&node.id)
            .with("id", node.id.clone())
            .with("title", node.meta.title.clone())
            .with("tags", serde_json::json!(node.meta.tags))
            .with("path", node.meta.path.clone());
        if let Some(kind) = &node.meta.doctype {
            document.insert("type", kind.clone());
        }
        for field in &self.package.manifest.schema.fields {
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
        let captures = &self.config.captures;

        if rule == captures.node {
            let actual = mapped.graph.node_count() + 1;
            if actual > self.package.manifest.limits.nodes {
                return Err(ImportError::RecordLimit {
                    kind: "nodes",
                    actual,
                    max: self.package.manifest.limits.nodes,
                });
            }

            let (node, body) = self.map_node(pair)?;
            if let Some((path, body)) = body {
                mapped.bodies.insert(path, body);
            }
            if mapped.graph.nodes.contains_key(&node.id) {
                // Report the local capture id, not the namespaced node ID.
                return Err(ImportError::DuplicateNodeId(node.meta.path.clone()));
            }
            mapped.graph.add_node(node);
            return Ok(());
        }

        if rule == captures.edge {
            let actual = mapped.edges.len() + 1;
            if actual > self.package.manifest.limits.edges {
                return Err(ImportError::RecordLimit {
                    kind: "edges",
                    actual,
                    max: self.package.manifest.limits.edges,
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

    fn map_node<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
    ) -> Result<(VaultNode, Option<(String, String)>), ImportError> {
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
        let node_id = self
            .runtime
            .namespace
            .node_id(&id)
            .map_err(|error| invalid_record("node", error.to_string()))?;

        let title = fields.title.clone().unwrap_or_else(|| id.clone());
        fields.tags.sort();
        fields.tags.dedup();
        let content = fields
            .content
            .filter(|body| !body.trim().is_empty());
        let meta = NodeMeta {
            source_id: self.package.manifest.metadata.id.clone(),
            title,
            tags: fields.tags,
            frontmatter: fields.properties,
            mtime: 0,
            path: id.clone(),
            doctype: fields.kind,
            folder: String::new(),
            content_type: content.as_ref().map(|_| "text/markdown".to_owned()),
            content_readable: content.is_some(),
            content_writable: false,
        };

        let body = content.map(|body| (id, body));
        Ok((
            VaultNode {
                id: node_id,
                meta,
                metrics: NodeMetrics::default(),
                x: 0.0,
                y: 0.0,
            },
            body,
        ))
    }

    fn collect_node_fields<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        fields: &mut NodeFields,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.config.captures;

        if rule == captures.id {
            return set_scalar(&mut fields.id, pair.as_str(), "node", "id");
        }
        if let Some(content_rule) = &captures.content {
            if rule == content_rule {
                return set_scalar(
                    &mut fields.content,
                    pair.as_str(),
                    "node",
                    "content",
                );
            }
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
        let captures = &self.config.captures;
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
        let mut fields = EdgeFields::default();
        for child in pair.into_inner() {
            self.collect_edge_fields(child, &mut fields)?;
        }

        let source = fields
            .source
            .ok_or_else(|| invalid_record("edge", "missing source capture"))?;
        let target = fields
            .target
            .ok_or_else(|| invalid_record("edge", "missing target capture"))?;
        if source.is_empty() || target.is_empty() {
            return Err(invalid_record(
                "edge",
                "source and target captures must be non-empty",
            ));
        }
        if let Some(kind) = &fields.kind {
            if kind.is_empty() {
                return Err(invalid_record("edge", "edge_kind capture is empty"));
            }
            // A package that declares its vocabulary is held to it; one that
            // declares nothing publishes the built-in `declared` type and
            // accepts any captured kind.
            let declared = &self.package.manifest.schema.edge_types;
            if !declared.is_empty() && !declared.iter().any(|edge_type| edge_type.key == *kind) {
                return Err(invalid_record(
                    "edge",
                    format!("emits edge kind {kind:?}, which schema.edge_types does not declare"),
                ));
            }
        }
        Ok(VaultEdge {
            source: self
                .runtime
                .namespace
                .node_id(&source)
                .map_err(|error| invalid_record("edge", error.to_string()))?,
            target: self
                .runtime
                .namespace
                .node_id(&target)
                .map_err(|error| invalid_record("edge", error.to_string()))?,
            kind: fields.kind,
        })
    }

    fn collect_edge_fields<'i, 'r>(
        &self,
        pair: Pair<'i, &'r str>,
        fields: &mut EdgeFields,
    ) -> Result<(), ImportError> {
        let rule = pair.as_rule();
        let captures = &self.config.captures;
        if rule == captures.source {
            return set_scalar(&mut fields.source, pair.as_str(), "edge", "source");
        }
        if rule == captures.target {
            return set_scalar(&mut fields.target, pair.as_str(), "edge", "target");
        }
        if let Some(kind_rule) = &captures.edge_kind {
            if rule == kind_rule {
                return set_scalar(&mut fields.kind, pair.as_str(), "edge", "edge_kind");
            }
        }
        if rule == captures.node || rule == captures.edge {
            return Err(invalid_record(
                "edge",
                format!("nested graph record '{rule}' is not allowed"),
            ));
        }

        for child in pair.into_inner() {
            self.collect_edge_fields(child, fields)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct MappedGraph {
    graph: VaultGraph,
    edges: Vec<VaultEdge>,
    /// Node bodies from the optional `content` capture, keyed by local id.
    bodies: HashMap<String, String>,
}

#[derive(Default)]
struct NodeFields {
    id: Option<String>,
    title: Option<String>,
    kind: Option<String>,
    tags: Vec<String>,
    properties: HashMap<String, Value>,
    content: Option<String>,
}

#[derive(Default)]
struct EdgeFields {
    source: Option<String>,
    target: Option<String>,
    kind: Option<String>,
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

/// A trusted, validated pest package bound by the administrator to one
/// filesystem input. The input path is runtime configuration and is never
/// read from the package itself.
#[cfg(feature = "native")]
pub struct FilesystemLoader {
    package: ValidatedPackage,
    input_path: PathBuf,
    /// Node bodies from the package's optional `content` capture, keyed by
    /// local capture id; refreshed on every load.
    bodies: std::sync::Mutex<HashMap<String, String>>,
}

#[cfg(feature = "native")]
impl FilesystemLoader {
    /// Bind a validated pest package to an explicit input path. Json-engine
    /// packages are rejected: they are bound through
    /// [`crate::build_importer`] instead.
    pub fn new(package: ValidatedPackage, input_path: impl Into<PathBuf>) -> Result<Self, ImportError> {
        package.pest_config()?;
        Ok(Self {
            package,
            input_path: input_path.into(),
            bodies: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Read the bounded UTF-8 file and return parse/mapping errors to callers
    /// that can handle a fallible load operation.
    pub fn load_checked(&self) -> Result<LoadResult, ImportError> {
        let bytes = read_bounded(&self.input_path, self.package.manifest.limits.input_bytes)?;
        let input = String::from_utf8(bytes).map_err(|source| ImportError::InputUtf8 {
            path: self.input_path.clone(),
            source,
        })?;
        let (result, bodies) = self.package.parse_input_with_bodies(&input)?;
        if let Ok(mut store) = self.bodies.lock() {
            *store = bodies;
        }
        Ok(result)
    }

    /// Validated package bound to this loader.
    pub fn package(&self) -> &ValidatedPackage {
        &self.package
    }

    /// Explicit administrator-configured input path.
    pub fn input_path(&self) -> &Path {
        &self.input_path
    }

    /// Whether the bound package captures per-node bodies.
    pub fn captures_content(&self) -> bool {
        self.package
            .pest_config()
            .ok()
            .is_some_and(|config| config.captures.captures_content())
    }
}

#[cfg(feature = "native")]
impl Loader for FilesystemLoader {
    fn name(&self) -> &str {
        &self.package.manifest.metadata.name
    }

    fn schema(&self) -> ImporterSchema {
        self.package.schema().clone()
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
#[cfg(feature = "native")]
pub struct FilesystemImporter {
    loader: FilesystemLoader,
}

#[cfg(feature = "native")]
impl FilesystemImporter {
    pub fn new(package: ValidatedPackage, input_path: impl Into<PathBuf>) -> Result<Self, ImportError> {
        Ok(Self {
            loader: FilesystemLoader::new(package, input_path)?,
        })
    }

    pub fn package(&self) -> &ValidatedPackage {
        self.loader.package()
    }

    pub fn input_path(&self) -> &Path {
        self.loader.input_path()
    }
}

#[cfg(feature = "native")]
impl Importer for FilesystemImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let metadata = &self.loader.package.manifest.metadata;
        let scope = self.loader.input_path.to_string_lossy().into_owned();
        let read = Capability::new(Effect::Read, Transport::Filesystem, scope.clone());
        let watch = Capability::new(Effect::Watch, Transport::Filesystem, scope.clone());
        let mut capabilities = vec![read, watch];
        if self.loader.captures_content() {
            // Bodies are served from this importer's capture store, not
            // from disk. The capability is scoped to the package's own
            // root (the input's directory) — the same contract OKF uses —
            // so hosts gate reads on `--vault-root` while cross-source
            // reads stay denied.
            let root = self
                .loader
                .input_path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or(scope);
            capabilities.push(Capability::new(
                Effect::ContentRead,
                Transport::Filesystem,
                root,
            ));
        }
        ImporterDescriptor::new(
            &metadata.id,
            &metadata.name,
            &metadata.version,
            capabilities,
            self.loader.package.schema().clone(),
        )
        .with_watch(WatchPlan::Filesystem {
            root: self.loader.input_path.clone(),
        })
    }

    fn import<'a>(
        &'a self,
        progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<ImportOutcome, PipelineError>> {
        Box::pin(async move {
            let stage = progress.stage(&format!(
                "Parsing {}",
                self.loader.package.manifest.metadata.name
            ));
            let result = self
                .loader
                .load_checked()
                .map(ImportOutcome::Loaded)
                .map_err(|error| PipelineError::Decode {
                    origin: self.loader.input_path.display().to_string(),
                    message: error.to_string(),
                });
            match &result {
                Ok(_) => progress.finish(stage),
                Err(error) => progress.fail(stage, &error.to_string()),
            }
            result
        })
    }

    /// Node bodies captured by the package's optional `content` rule,
    /// keyed by the node's local capture id (`NodeMeta::path`).
    fn read_body(&self, path: &str) -> Option<String> {
        self.loader.bodies.lock().ok()?.get(path).cloned()
    }
}

#[cfg(feature = "native")]
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
mod tests;
