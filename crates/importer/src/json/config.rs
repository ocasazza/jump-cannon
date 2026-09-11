//! The json engine's package configuration: endpoints, pagination, and the
//! projection of JSON documents into the canonical graph.
//!
//! Like the pest engine, the configuration deliberately carries **no
//! data-source binding**: the base URL, credentials, and the values of
//! declared variables (tenant, bank, repository, namespace, …) are runtime
//! configuration an administrator binds to a validated package. One package
//! therefore serves every instance of that API shape, and a new API is a new
//! package — not a new crate, not a new `SourceKind`.

use std::collections::{BTreeMap, HashSet};

use data_loader::{
    DiscoveryField, DiscoveryFieldType, ImportError, ImporterSchema, TagHierarchySchema,
};
use serde::Deserialize;

use crate::{Limits, PackageMetadata, PackageSchema};

/// The `[parser] engine` tag selecting this engine.
pub const ENGINE: &str = "json";

/// The `source_kind` every package of this engine publishes. Packages vary
/// the `source_id`, exactly as pest packages do. The wire value predates the
/// unified crate and is load-bearing for existing node IDs
/// (`httpjson:{source_id}:{local}`).
pub const SOURCE_KIND: &str = "httpjson";

/// Largest `page_size` a package may declare.
pub const MAX_PAGE_SIZE: usize = 1000;
/// Longest per-request timeout a package may declare.
pub const MAX_REQUEST_TIMEOUT_SECONDS: u64 = 600;

const fn default_page_size() -> usize {
    500
}

const fn default_request_timeout_seconds() -> u64 {
    120
}

/// The json engine's `[parser]` configuration.
#[derive(Debug, Clone)]
pub struct JsonEngineConfig {
    /// Path/query variables an administrator supplies per instance.
    pub variables: Vec<VariableSpec>,
    /// Optional existence check run before any collection is read.
    pub preflight: Option<Preflight>,
    /// API endpoints and what their documents become.
    pub collections: Vec<Collection>,
    /// Page size for `limit_offset` pagination.
    pub page_size: usize,
    /// Per-request timeout for the production HTTP transport.
    pub request_timeout_seconds: u64,
}

/// Wire shape of a json-engine package: the shared envelope plus the
/// engine-tagged `[parser]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonManifestToml {
    pub format_version: u32,
    pub metadata: PackageMetadata,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub schema: PackageSchema,
    pub parser: JsonParserToml,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonParserToml {
    pub engine: String,
    #[serde(default)]
    pub variables: Vec<VariableSpec>,
    #[serde(default)]
    pub preflight: Option<Preflight>,
    pub collections: Vec<Collection>,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

/// One administrator-supplied value substituted into paths and queries as
/// `{name}`. The engine percent-encodes values on interpolation, so values
/// may carry spaces, slashes, and query syntax (`publisher:Zenodo`).
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
    /// `limits.nodes` so an over-bound source fails loudly.
    #[serde(default)]
    pub total_pointer: Option<String>,
    #[serde(flatten)]
    pub produces: Produces,
}

/// How a collection is paged.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum Pagination {
    /// Single request; the endpoint returns everything it will return.
    #[default]
    None,
    /// `?limit=&offset=` walked until a short page.
    LimitOffset,
    /// `?page=0&size=` walked until a short page, with configurable
    /// parameter names: PRIDE uses `page`/`pageSize`, DataCite uses
    /// `page[number]`/`page[size]`.
    PageNumber(PageNumberParams),
}

/// Parameter names and origin for page-number pagination.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageNumberParams {
    /// Query parameter carrying the page index (`page`, `page[number]`).
    #[serde(default = "default_page_param")]
    pub page_param: String,
    /// Query parameter carrying the page size (`size`, `pageSize`).
    #[serde(default = "default_size_param")]
    pub size_param: String,
    /// Index of the first page: 0 for PRIDE, 1 for DataCite.
    #[serde(default)]
    pub first_page: usize,
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
    /// Collection-wide node type: the canonical faceted `type` field and the
    /// node's `doctype` for every record the `doctype` rule below does not
    /// resolve, e.g. `memory`.
    pub node_type: String,
    /// Per-record type derived from the document itself, e.g. Hindsight's
    /// `fact_type` (`world` / `experience` / `observation`). Resolved values
    /// drive color-by/shape-by/filter downstream exactly like a vault
    /// frontmatter `doctype`.
    #[serde(default)]
    pub doctype: Option<DoctypeRule>,
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

/// Per-record node type. The value at `pointer` (a JSON string) is looked up
/// in `map` when the map is non-empty — a bounded, package-declared
/// vocabulary so the `type` facet cannot balloon on hostile or drifting
/// data — and used verbatim otherwise. A missing value, or an unmapped one
/// when `map` is declared, falls back to the collection's `node_type`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctypeRule {
    pub pointer: String,
    /// Raw document value → display type (`world` → `World Fact`).
    #[serde(default)]
    pub map: BTreeMap<String, String>,
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
fn default_page_param() -> String {
    "page".to_string()
}
fn default_size_param() -> String {
    "size".to_string()
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

/// Validate the engine configuration beyond the shared envelope checks:
/// variables, preflight, collections, pointers, and the json-only knobs.
pub(crate) fn validate_config(
    config: &JsonEngineConfig,
    schema: &PackageSchema,
) -> Result<(), ImportError> {
    if config.page_size == 0 || config.page_size > MAX_PAGE_SIZE {
        return Err(invalid(format!(
            "parser.page_size must be between 1 and {MAX_PAGE_SIZE}, got {}",
            config.page_size
        )));
    }
    if config.request_timeout_seconds == 0
        || config.request_timeout_seconds > MAX_REQUEST_TIMEOUT_SECONDS
    {
        return Err(invalid(format!(
            "parser.request_timeout_seconds must be between 1 and {MAX_REQUEST_TIMEOUT_SECONDS}, got {}",
            config.request_timeout_seconds
        )));
    }

    let mut variables = HashSet::new();
    for variable in &config.variables {
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

    if config.collections.is_empty() {
        return Err(invalid(
            "a package must declare at least one collection".into(),
        ));
    }
    let mut names = HashSet::new();
    for collection in &config.collections {
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

    let node_collections: HashSet<&str> = config
        .collections
        .iter()
        .filter(|collection| matches!(collection.produces, Produces::Nodes(_)))
        .map(|collection| collection.name.as_str())
        .collect();
    let declared_fields: HashSet<&str> = schema
        .fields
        .iter()
        .map(|field| field.key.as_str())
        .collect();
    let declared_edges: HashSet<&str> = schema
        .edge_types
        .iter()
        .map(|edge| edge.key.as_str())
        .collect();

    if let Some(preflight) = &config.preflight {
        validate_template("preflight.path", &preflight.path, &variables)?;
        validate_items_pointer("preflight.items_pointer", &preflight.items_pointer)?;
        validate_pointer("preflight.id_pointer", &preflight.id_pointer)?;
        if !variables.contains(preflight.variable.as_str()) {
            return Err(invalid(format!(
                "preflight.variable {:?} is not a declared variable",
                preflight.variable
            )));
        }
    }

    for collection in &config.collections {
        let name = &collection.name;
        validate_template(&format!("{name}.path"), &collection.path, &variables)?;
        for (key, value) in &collection.query {
            validate_template(&format!("{name}.query.{key}"), value, &variables)?;
        }
        validate_items_pointer(&format!("{name}.items_pointer"), &collection.items_pointer)?;
        if let Pagination::PageNumber(params) = &collection.paginate {
            for (what, param) in [
                ("page_param", params.page_param.as_str()),
                ("size_param", params.size_param.as_str()),
            ] {
                let invalid_char = |c: char| {
                    c.is_whitespace() || matches!(c, '=' | '&' | '?' | '#')
                };
                if param.is_empty() || param.chars().any(invalid_char) {
                    return Err(invalid(format!(
                        "{name}.paginate.{what} must be a query parameter name without whitespace or '=' '&' '?' '#', got {param:?}"
                    )));
                }
            }
        }
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
                if let Some(rule) = &rules.doctype {
                    validate_pointer(&format!("{name}.doctype.pointer"), &rule.pointer)?;
                    for (raw, label) in &rule.map {
                        if raw.trim().is_empty() || label.trim().is_empty() {
                            return Err(invalid(format!(
                                "{name}.doctype.map entries must be non-empty, got {raw:?} = {label:?}"
                            )));
                        }
                    }
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
    Ok(())
}

/// Resolve administrator-supplied values against the declared variables,
/// applying defaults and rejecting unknown or unusable values.
pub(crate) fn resolve_variables(
    package_id: &str,
    config: &JsonEngineConfig,
    supplied: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ImportError> {
    let declared: HashSet<&str> = config
        .variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect();
    for key in supplied.keys() {
        if !declared.contains(key.as_str()) {
            return Err(invalid(format!(
                "package {:?} declares no variable {key:?}; it accepts: {}",
                package_id,
                declared.iter().copied().collect::<Vec<_>>().join(", ")
            )));
        }
    }
    let mut resolved = BTreeMap::new();
    for variable in &config.variables {
        let value = supplied
            .get(&variable.name)
            .cloned()
            .or_else(|| variable.default.clone())
            .ok_or_else(|| {
                invalid(format!(
                    "package {:?} requires variable {:?}{}",
                    package_id,
                    variable.name,
                    variable
                        .description
                        .as_deref()
                        .map(|text| format!(" ({text})"))
                        .unwrap_or_default()
                ))
            })?;
        if value.is_empty()
            || value.len() > 512
            || value.chars().any(|ch| ch.is_control() || matches!(ch, '{' | '}'))
        {
            return Err(invalid(format!(
                "variable {:?} must be non-empty, at most 512 characters, and free of braces and control characters (values are percent-encoded when interpolated into URLs), got {value:?}",
                variable.name
            )));
        }
        resolved.insert(variable.name.clone(), value);
    }
    Ok(resolved)
}

/// Canonical fields every package publishes, plus its declared additions.
pub(crate) fn build_schema(schema: &PackageSchema) -> ImporterSchema {
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
    fields.extend(schema.fields.iter().cloned());
    ImporterSchema::new(
        SOURCE_KIND,
        fields,
        schema.edge_types.clone(),
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["application/json"])
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

/// `items_pointer` variant: the empty string selects the whole response
/// document, for APIs (PRIDE Archive WS, …) that return a bare JSON array
/// at the root. `serde_json::Value::pointer("")` already resolves to the
/// root, so this is a validation relaxation, not new traversal code.
fn validate_items_pointer(field: &str, pointer: &str) -> Result<(), ImportError> {
    if pointer.is_empty() {
        return Ok(());
    }
    validate_pointer(field, pointer)
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn invalid(message: String) -> ImportError {
    ImportError::InvalidDescriptor {
        message: format!("json engine package: {message}"),
    }
}
