//! The HTTP/JSON importer package format.
//!
//! A package is one versioned TOML file describing **how to read a shape of
//! JSON API** — endpoints, pagination, and the projection of JSON documents
//! into the canonical graph. Like the Pest package format, it deliberately
//! carries **no data-source binding**: the base URL, credentials, and the
//! values of declared variables (tenant, bank, repository, namespace, …) are
//! runtime configuration an administrator binds to a validated package.
//!
//! One package therefore serves every instance of that API shape, and a new
//! API is a new package — not a new crate, not a new `SourceKind`.

use std::collections::{BTreeMap, HashSet};

use data_loader::{
    DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, ImportError, ImporterSchema,
    TagHierarchySchema,
};
use serde::Deserialize;

/// Package format understood by this engine.
pub const FORMAT_VERSION: u32 = 1;

/// The `source_kind` every package of this engine publishes. Packages vary the
/// `source_id`, exactly as Pest packages do.
pub const SOURCE_KIND: &str = "httpjson";

/// Absolute ceilings. A package may lower any of these; validation rejects
/// zero values and values above the ceiling.
pub const HARD_LIMITS: Limits = Limits {
    manifest_bytes: 1024 * 1024,
    max_records: 1_000_000,
    max_response_bytes: 256 * 1024 * 1024,
    request_timeout_seconds: 600,
    page_size: 1000,
};

/// Deserialized, not-yet-validated package manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpJsonManifest {
    /// Package-format version. Only [`FORMAT_VERSION`] is accepted.
    pub format_version: u32,
    pub metadata: PackageMetadata,
    /// Path/query variables an administrator supplies per instance.
    #[serde(default)]
    pub variables: Vec<VariableSpec>,
    /// Optional existence check run before any collection is read, so a
    /// mistyped instance variable fails with the values that do exist.
    #[serde(default)]
    pub preflight: Option<Preflight>,
    pub collections: Vec<Collection>,
    pub schema: PackageSchema,
    #[serde(default)]
    pub limits: Limits,
}

/// Identity and release metadata for a package.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMetadata {
    /// Stable machine-readable identifier, such as `hindsight.memory-bank`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One administrator-supplied value substituted into paths and queries as
/// `{name}`. Values are validated as single URL path segments by the engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableSpec {
    pub name: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl VariableSpec {
    pub fn required(&self) -> bool {
        self.default.is_none()
    }
}

/// Existence check for the selected instance.
///
/// For a memory service this is "does this bank exist"; for a ticket system
/// "does this project exist". The message names what the caller asked for and
/// what the API actually offers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preflight {
    /// Path template listing the available instances.
    pub path: String,
    /// JSON pointer to the array of available instances.
    #[serde(default = "default_items_pointer")]
    pub items_pointer: String,
    /// Pointer, relative to one array element, holding its identifier.
    pub id_pointer: String,
    /// The variable whose value must appear among those identifiers.
    pub variable: String,
    /// Noun used in the error message ("bank", "project", …).
    #[serde(default = "default_subject")]
    pub subject: String,
}

/// Package-declared discovery fields and edge semantics. The canonical
/// `id`/`title`/`tags`/`path`/`type`/`folder` fields are supplied by the
/// engine; a package declares only what it adds.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSchema {
    #[serde(default)]
    pub fields: Vec<DiscoveryField>,
    pub edge_types: Vec<EdgeTypeSchema>,
}

/// Per-package limits, bounded by [`HARD_LIMITS`].
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_manifest_bytes")]
    pub manifest_bytes: usize,
    /// Hard bound on records imported per collection. Exceeding it fails the
    /// import; it never truncates silently.
    #[serde(default = "default_max_records")]
    pub max_records: usize,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        HARD_LIMITS
    }
}

/// One API endpoint and what its documents become.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    /// Unique name, referenced by edge rules.
    pub name: String,
    /// Path template, e.g. `/v1/{tenant}/banks/{bank}/memories/list`.
    pub path: String,
    /// Static query parameters. Values may reference `{variables}`.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    /// JSON pointer to the array of documents in the response.
    #[serde(default = "default_items_pointer")]
    pub items_pointer: String,
    #[serde(default)]
    pub paginate: Pagination,
    /// Optional pointer to a server-reported total, checked against
    /// `limits.max_records` so an over-bound source fails loudly.
    #[serde(default)]
    pub total_pointer: Option<String>,
    #[serde(flatten)]
    pub produces: Produces,
}

/// How a collection is paged.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum Pagination {
    /// Single request; the endpoint returns everything it will return.
    #[default]
    None,
    /// `?limit=&offset=` walked until a short page.
    LimitOffset,
}

/// What a collection's documents become: graph nodes, or edges between nodes
/// another collection already produced.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Produces {
    Nodes(NodeRules),
    Edges(EdgeListRules),
}

/// Projection of one JSON document into a node.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRules {
    /// Pointer to the document's stable identifier.
    pub id_pointer: String,
    /// Prefix keeping local IDs distinct across collections (`entity:`).
    #[serde(default)]
    pub local_prefix: String,
    /// Value of the canonical faceted `type` field, e.g. `memory`.
    pub node_type: String,
    /// Folder facet; defaults to the collection name.
    #[serde(default)]
    pub folder: Option<String>,
    pub title: TitleRule,
    /// Pointer to an array of tag strings.
    #[serde(default)]
    pub tags_pointer: Option<String>,
    /// Import the document only when this predicate holds.
    #[serde(default)]
    pub skip_unless: Option<Predicate>,
    /// Package-declared discovery fields populated from the document.
    #[serde(default)]
    pub fields: Vec<FieldRule>,
    /// Edges derived from values inside the document.
    #[serde(default)]
    pub edges: Vec<EdgeRule>,
}

/// How a node's title is derived. Titles must be non-empty, so a fallback is
/// always available.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleRule {
    pub pointer: String,
    /// Truncate at a word boundary beyond this many characters.
    #[serde(default = "default_title_max_chars")]
    pub max_chars: usize,
    /// Prefix for the `{prefix} {short-id}` fallback when the pointer is empty.
    #[serde(default = "default_subject")]
    pub fallback_prefix: String,
}

/// Equality predicate over one pointer, case-insensitive.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub pointer: String,
    pub equals: String,
    /// Treat a missing value as matching (fields absent on older records).
    #[serde(default)]
    pub missing_matches: bool,
}

/// One package-declared discovery field populated from the document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldRule {
    /// Must name a field declared in `schema.fields`.
    pub key: String,
    pub pointer: String,
    #[serde(default)]
    pub transform: Transform,
}

/// Value transforms applied between the JSON document and the graph.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    #[default]
    None,
    /// `"tofu, Hydra"` becomes `["tofu", "Hydra"]`.
    SplitCsv,
}

/// An edge derived from a value inside a node document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeRule {
    /// Declared edge-type key; documentation only, but must be declared.
    pub kind: String,
    /// Pointer to the value naming the target(s).
    pub value_pointer: String,
    #[serde(default)]
    pub transform: Transform,
    /// Collection whose nodes the value resolves against.
    pub target_collection: String,
    #[serde(default)]
    pub match_on: MatchOn,
}

/// How an edge value resolves to a target node.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MatchOn {
    /// The value is the target's `id_pointer` value.
    #[default]
    Id,
    /// The value is the target's title, matched exactly then case-insensitively.
    Title,
}

/// Projection of a document that *is* an edge (an API-provided link list).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeListRules {
    pub source_pointer: String,
    pub target_pointer: String,
    /// Pointer to the link's kind, used with `include_kinds`.
    #[serde(default)]
    pub kind_pointer: Option<String>,
    /// Only these link kinds import. Empty imports every kind.
    #[serde(default)]
    pub include_kinds: Vec<String>,
    /// Collection whose nodes both endpoints must resolve to.
    pub endpoints_collection: String,
    #[serde(default = "default_true")]
    pub drop_self_loops: bool,
    #[serde(default)]
    pub dedupe: Dedupe,
}

/// Duplicate-edge policy.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Dedupe {
    /// `a -> b` and `b -> a` are the same edge (untyped canvas edges).
    #[default]
    Unordered,
    Ordered,
    None,
}

fn default_items_pointer() -> String {
    "/items".to_string()
}
fn default_subject() -> String {
    "record".to_string()
}
fn default_title_max_chars() -> usize {
    72
}
fn default_true() -> bool {
    true
}
const fn default_manifest_bytes() -> usize {
    HARD_LIMITS.manifest_bytes
}
const fn default_max_records() -> usize {
    50_000
}
const fn default_max_response_bytes() -> usize {
    64 * 1024 * 1024
}
const fn default_request_timeout_seconds() -> u64 {
    120
}
const fn default_page_size() -> usize {
    500
}

/// A manifest whose metadata, limits, variables, collections, and schema have
/// been validated. Construction is the only way to obtain one.
#[derive(Debug, Clone)]
pub struct ValidatedPackage {
    manifest: HttpJsonManifest,
    schema: ImporterSchema,
}

impl ValidatedPackage {
    /// Parse and validate a package from TOML bytes.
    pub fn from_toml_bytes(bytes: &[u8]) -> Result<Self, ImportError> {
        if bytes.len() > HARD_LIMITS.manifest_bytes {
            return Err(invalid(format!(
                "package is {} bytes; hard limit is {} bytes",
                bytes.len(),
                HARD_LIMITS.manifest_bytes
            )));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|error| invalid(format!("package is not UTF-8: {error}")))?;
        let manifest: HttpJsonManifest = toml::from_str(text)
            .map_err(|error| invalid(format!("invalid package TOML: {error}")))?;
        Self::validate(manifest)
    }

    fn validate(manifest: HttpJsonManifest) -> Result<Self, ImportError> {
        if manifest.format_version != FORMAT_VERSION {
            return Err(invalid(format!(
                "unsupported package format_version {}; this engine accepts {FORMAT_VERSION}",
                manifest.format_version
            )));
        }
        for (field, value) in [
            ("metadata.id", &manifest.metadata.id),
            ("metadata.name", &manifest.metadata.name),
            ("metadata.version", &manifest.metadata.version),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(format!("{field} must be non-empty")));
            }
        }
        validate_limits(manifest.limits)?;

        let mut variables = HashSet::new();
        for variable in &manifest.variables {
            if !is_identifier(&variable.name) {
                return Err(invalid(format!(
                    "variable name {:?} must be [a-z0-9_]{{1,64}}",
                    variable.name
                )));
            }
            if !variables.insert(variable.name.as_str()) {
                return Err(invalid(format!("duplicate variable {:?}", variable.name)));
            }
        }

        if manifest.collections.is_empty() {
            return Err(invalid("a package must declare at least one collection".into()));
        }
        let mut names = HashSet::new();
        for collection in &manifest.collections {
            if !is_identifier(&collection.name) {
                return Err(invalid(format!(
                    "collection name {:?} must be [a-z0-9_]{{1,64}}",
                    collection.name
                )));
            }
            if !names.insert(collection.name.as_str()) {
                return Err(invalid(format!(
                    "duplicate collection {:?}",
                    collection.name
                )));
            }
        }

        let node_collections: HashSet<&str> = manifest
            .collections
            .iter()
            .filter(|collection| matches!(collection.produces, Produces::Nodes(_)))
            .map(|collection| collection.name.as_str())
            .collect();
        let declared_fields: HashSet<&str> = manifest
            .schema
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect();
        let declared_edges: HashSet<&str> = manifest
            .schema
            .edge_types
            .iter()
            .map(|edge| edge.key.as_str())
            .collect();

        if let Some(preflight) = &manifest.preflight {
            validate_template("preflight.path", &preflight.path, &variables)?;
            validate_pointer("preflight.items_pointer", &preflight.items_pointer)?;
            validate_pointer("preflight.id_pointer", &preflight.id_pointer)?;
            if !variables.contains(preflight.variable.as_str()) {
                return Err(invalid(format!(
                    "preflight.variable {:?} is not a declared variable",
                    preflight.variable
                )));
            }
        }

        for collection in &manifest.collections {
            let name = &collection.name;
            validate_template(&format!("{name}.path"), &collection.path, &variables)?;
            for (key, value) in &collection.query {
                validate_template(&format!("{name}.query.{key}"), value, &variables)?;
            }
            validate_pointer(&format!("{name}.items_pointer"), &collection.items_pointer)?;
            if let Some(pointer) = &collection.total_pointer {
                validate_pointer(&format!("{name}.total_pointer"), pointer)?;
            }
            match &collection.produces {
                Produces::Nodes(rules) => {
                    validate_pointer(&format!("{name}.id_pointer"), &rules.id_pointer)?;
                    validate_pointer(&format!("{name}.title.pointer"), &rules.title.pointer)?;
                    if rules.node_type.trim().is_empty() {
                        return Err(invalid(format!("{name}.node_type must be non-empty")));
                    }
                    if rules.title.max_chars == 0 {
                        return Err(invalid(format!("{name}.title.max_chars must be non-zero")));
                    }
                    if let Some(pointer) = &rules.tags_pointer {
                        validate_pointer(&format!("{name}.tags_pointer"), pointer)?;
                    }
                    if let Some(predicate) = &rules.skip_unless {
                        validate_pointer(&format!("{name}.skip_unless.pointer"), &predicate.pointer)?;
                    }
                    for rule in &rules.fields {
                        validate_pointer(&format!("{name}.fields.{}", rule.key), &rule.pointer)?;
                        if !declared_fields.contains(rule.key.as_str()) {
                            return Err(invalid(format!(
                                "{name} populates field {:?}, which schema.fields does not declare",
                                rule.key
                            )));
                        }
                    }
                    for rule in &rules.edges {
                        validate_pointer(
                            &format!("{name}.edges.{}", rule.kind),
                            &rule.value_pointer,
                        )?;
                        if !declared_edges.contains(rule.kind.as_str()) {
                            return Err(invalid(format!(
                                "{name} emits edge kind {:?}, which schema.edge_types does not declare",
                                rule.kind
                            )));
                        }
                        if !node_collections.contains(rule.target_collection.as_str()) {
                            return Err(invalid(format!(
                                "{name} edge {:?} targets {:?}, which is not a node collection",
                                rule.kind, rule.target_collection
                            )));
                        }
                    }
                }
                Produces::Edges(rules) => {
                    validate_pointer(&format!("{name}.source_pointer"), &rules.source_pointer)?;
                    validate_pointer(&format!("{name}.target_pointer"), &rules.target_pointer)?;
                    if let Some(pointer) = &rules.kind_pointer {
                        validate_pointer(&format!("{name}.kind_pointer"), pointer)?;
                    }
                    if rules.kind_pointer.is_none() && !rules.include_kinds.is_empty() {
                        return Err(invalid(format!(
                            "{name} filters include_kinds without a kind_pointer"
                        )));
                    }
                    if !node_collections.contains(rules.endpoints_collection.as_str()) {
                        return Err(invalid(format!(
                            "{name} resolves endpoints against {:?}, which is not a node collection",
                            rules.endpoints_collection
                        )));
                    }
                }
            }
        }

        let schema = build_schema(&manifest);
        schema.validate()?;
        Ok(Self { manifest, schema })
    }

    pub fn manifest(&self) -> &HttpJsonManifest {
        &self.manifest
    }

    /// The discovery schema this package publishes.
    pub fn schema(&self) -> &ImporterSchema {
        &self.schema
    }

    pub fn limits(&self) -> Limits {
        self.manifest.limits
    }

    pub fn collections(&self) -> &[Collection] {
        &self.manifest.collections
    }

    pub fn preflight(&self) -> Option<&Preflight> {
        self.manifest.preflight.as_ref()
    }

    /// Resolve administrator-supplied values against the declared variables,
    /// applying defaults and rejecting unknown or unusable values.
    pub fn resolve_variables(
        &self,
        supplied: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, ImportError> {
        let declared: HashSet<&str> = self
            .manifest
            .variables
            .iter()
            .map(|variable| variable.name.as_str())
            .collect();
        for key in supplied.keys() {
            if !declared.contains(key.as_str()) {
                return Err(invalid(format!(
                    "package {:?} declares no variable {key:?}; it accepts: {}",
                    self.manifest.metadata.id,
                    declared.iter().copied().collect::<Vec<_>>().join(", ")
                )));
            }
        }
        let mut resolved = BTreeMap::new();
        for variable in &self.manifest.variables {
            let value = supplied
                .get(&variable.name)
                .cloned()
                .or_else(|| variable.default.clone())
                .ok_or_else(|| {
                    invalid(format!(
                        "package {:?} requires variable {:?}{}",
                        self.manifest.metadata.id,
                        variable.name,
                        variable
                            .description
                            .as_deref()
                            .map(|text| format!(" ({text})"))
                            .unwrap_or_default()
                    ))
                })?;
            if value.is_empty()
                || value.len() > 128
                || value
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '%' | '{' | '}'))
            {
                return Err(invalid(format!(
                    "variable {:?} must be a URL path segment without whitespace, /, ?, #, %, or braces, got {value:?}",
                    variable.name
                )));
            }
            resolved.insert(variable.name.clone(), value);
        }
        Ok(resolved)
    }
}

/// Canonical fields every package publishes, plus its declared additions.
fn build_schema(manifest: &HttpJsonManifest) -> ImporterSchema {
    let mut fields = vec![
        DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
        DiscoveryField::new("title", DiscoveryFieldType::Text, true)
            .searchable(4)
            .snippet(),
        DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
            .searchable(3)
            .facetable(),
        DiscoveryField::new("path", DiscoveryFieldType::Keyword, false).searchable(2),
        DiscoveryField::new("type", DiscoveryFieldType::Keyword, false)
            .searchable(2)
            .facetable(),
        DiscoveryField::new("folder", DiscoveryFieldType::Keyword, false)
            .searchable(1)
            .facetable(),
    ];
    fields.extend(manifest.schema.fields.iter().cloned());
    ImporterSchema::new(
        SOURCE_KIND,
        fields,
        manifest.schema.edge_types.clone(),
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["application/json"])
}

fn validate_limits(limits: Limits) -> Result<(), ImportError> {
    for (field, value, ceiling) in [
        ("manifest_bytes", limits.manifest_bytes, HARD_LIMITS.manifest_bytes),
        ("max_records", limits.max_records, HARD_LIMITS.max_records),
        (
            "max_response_bytes",
            limits.max_response_bytes,
            HARD_LIMITS.max_response_bytes,
        ),
        ("page_size", limits.page_size, HARD_LIMITS.page_size),
    ] {
        if value == 0 || value > ceiling {
            return Err(invalid(format!(
                "limits.{field} must be between 1 and {ceiling}, got {value}"
            )));
        }
    }
    if limits.request_timeout_seconds == 0
        || limits.request_timeout_seconds > HARD_LIMITS.request_timeout_seconds
    {
        return Err(invalid(format!(
            "limits.request_timeout_seconds must be between 1 and {}",
            HARD_LIMITS.request_timeout_seconds
        )));
    }
    Ok(())
}

/// Every `{placeholder}` in a template must name a declared variable.
fn validate_template(
    field: &str,
    template: &str,
    variables: &HashSet<&str>,
) -> Result<(), ImportError> {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(invalid(format!("{field} has an unterminated {{ placeholder")));
        };
        let name = &after[..end];
        if !variables.contains(name) {
            return Err(invalid(format!(
                "{field} references undeclared variable {name:?}"
            )));
        }
        rest = &after[end + 1..];
    }
    if !template.starts_with('/') {
        // Query values are templates too; only paths must be rooted.
        if field.ends_with(".path") {
            return Err(invalid(format!("{field} must start with '/'")));
        }
    }
    Ok(())
}

fn validate_pointer(field: &str, pointer: &str) -> Result<(), ImportError> {
    if pointer.is_empty() || !pointer.starts_with('/') {
        return Err(invalid(format!(
            "{field} must be a JSON pointer starting with '/', got {pointer:?}"
        )));
    }
    Ok(())
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn invalid(message: String) -> ImportError {
    ImportError::InvalidDescriptor {
        message: format!("http-json package: {message}"),
    }
}
