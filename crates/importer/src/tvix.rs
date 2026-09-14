//! The `tvix` engine: Nix-expression graph generators as packages.
//!
//! This engine is a **mechanism**, not a data source. The `[parser]` table of
//! a `format_version = 3` package carries `{ expr, [variables] }`: an inline
//! Nix expression that is a **function of the declared variables**, plus the
//! variables' names, types, defaults, and optional numeric bounds. An
//! administrator binds runtime values (node count, seed, cluster count, …) at
//! apply time exactly as the [`json`](crate::json) engine binds its variables —
//! so one package serves every parameterisation, and a new generator is a new
//! package, not a new `SourceKind`, a `--flag`, or a crate. See "Importers:
//! packages, not crates" in `AGENTS.md`.
//!
//! The engine renders the bound expression as `( <expr> ) { <bindings> }`,
//! where `<bindings>` binds every declared variable to a Nix literal coerced
//! from its declared type (integers/floats/booleans unquoted, strings quoted
//! and escaped), and hands it to [`tvix_wasm::eval_graph`]. The result is a
//! `toGraphJSON`-shaped attrset (`{ nodes = [...]; links = [...]; }`) projected
//! into the canonical graph. The embedded generator library (`graph.nix` with
//! `random`/`clustered`/`starGen`/…) is importable from the expression via
//! `import /jc/src/graph.nix {}`.
//!
//! Every node ID is `tvix:{source_id}:{local}`, where `source_id` names the
//! bound instance and `local` is the node id the expression emitted.
//!
//! # Feature split
//!
//! Manifest validation, variable resolution, and Nix-literal rendering are
//! wasm-clean and build on `wasm32-unknown-unknown` with `--no-default-features`.
//! Evaluation ([`tvix_wasm::eval_graph`]) and graph projection are gated behind
//! the `native` feature — the same split the json engine applies to its reqwest
//! transport. `eval_graph` is synchronous and CPU-bound, so the native importer
//! runs it on a blocking thread.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use data_loader::{
    DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, ImportError as PipelineError,
    ImporterSchema, TagHierarchySchema,
};

#[cfg(feature = "native")]
use std::collections::HashMap;
#[cfg(feature = "native")]
use data_loader::{
    identity::Namespace, Capability, Effect, ImportFuture, ImportOutcome, ImportProgress, Importer,
    ImporterDescriptor, LoadResult, SearchDocument, Transport, WatchPlan,
};
#[cfg(feature = "native")]
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

use crate::{
    EngineRuntime, ImportError, ImporterManifest, Limits, PackageMetadata, PackageSchema,
    ParserConfig, ValidatedPackage,
};

/// The `[parser] engine` tag selecting this engine.
pub const ENGINE: &str = "tvix";

/// The `source_kind` every package of this engine publishes. Packages vary the
/// `source_id`, exactly as pest and json packages do; it is load-bearing for
/// node IDs (`tvix:{source_id}:{local}`).
pub const SOURCE_KIND: &str = "tvix";

/// The declared type of a tvix variable. It decides how a supplied string is
/// validated and rendered as a Nix literal at apply time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvixVarType {
    /// A Nix integer literal (`1000`). Honours `min`/`max`.
    Integer,
    /// A Nix float literal (`0.8`). Honours `min`/`max`.
    Number,
    /// A Nix boolean literal (`true`/`false`).
    Boolean,
    /// A Nix string literal, quoted and escaped.
    String,
}

impl TvixVarType {
    /// The wire tag for this type.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::String => "string",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "string" => Some(Self::String),
            _ => None,
        }
    }
}

/// One runtime parameter of a tvix generator package. An administrator supplies
/// its value per instance; a variable with a `default` is optional. Numeric
/// bounds apply only to `integer`/`number`.
#[derive(Debug, Clone)]
pub struct TvixVariable {
    pub name: String,
    pub ty: TvixVarType,
    pub default: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub description: Option<String>,
}

impl TvixVariable {
    /// A variable with no default must be supplied at apply time.
    pub fn required(&self) -> bool {
        self.default.is_none()
    }
}

/// The tvix engine's validated `[parser]` configuration: the generator
/// expression (a function of the declared variables) and the variables it
/// binds.
#[derive(Debug, Clone)]
pub struct TvixEngineConfig {
    /// The inline Nix expression — a function from an attrset of the declared
    /// variables to a `toGraphJSON`-shaped attrset.
    pub expr: String,
    /// The runtime parameters bound into the expression at apply time.
    pub variables: Vec<TvixVariable>,
}

/// Wire shape of a tvix-engine package: the shared envelope plus the
/// engine-tagged `[parser]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TvixManifestToml {
    pub format_version: u32,
    pub metadata: PackageMetadata,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub schema: PackageSchema,
    pub parser: TvixParserToml,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TvixParserToml {
    pub engine: String,
    pub expr: String,
    #[serde(default)]
    pub variables: Vec<TvixVariableToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TvixVariableToml {
    pub name: String,
    #[serde(default = "default_var_type", rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_var_type() -> String {
    TvixVarType::String.as_str().to_owned()
}

/// Parse the full document, validate the shared envelope and the tvix engine
/// configuration, and prebuild the discovery schema.
pub(crate) fn validate(source: &str) -> Result<ValidatedPackage, ImportError> {
    let wire: TvixManifestToml = toml::from_str(source)?;
    debug_assert_eq!(wire.format_version, crate::FORMAT_VERSION);
    debug_assert_eq!(wire.parser.engine, ENGINE);

    ValidatedPackage::validate_metadata(&wire.metadata)?;
    ValidatedPackage::validate_limits(wire.limits)?;
    if source.len() > wire.limits.manifest_bytes {
        return Err(ImportError::ManifestTooLarge {
            actual: source.len(),
            max: wire.limits.manifest_bytes,
        });
    }

    let config =
        build_config(wire.parser).map_err(|error| ImportError::TvixEngine(error.to_string()))?;
    validate_config(&config).map_err(|error| ImportError::TvixEngine(error.to_string()))?;
    validate_package_schema(&wire.schema)
        .map_err(|error| ImportError::TvixEngine(error.to_string()))?;

    let schema = tvix_schema();
    schema
        .validate()
        .map_err(|error| ImportError::TvixEngine(error.to_string()))?;

    let manifest = ImporterManifest {
        format_version: wire.format_version,
        metadata: wire.metadata,
        limits: wire.limits,
        schema: wire.schema,
        parser: ParserConfig::Tvix(config),
    };
    Ok(ValidatedPackage {
        manifest,
        schema,
        runtime: EngineRuntime::Tvix,
    })
}

/// Translate the wire `[parser]` table into the validated engine config,
/// rejecting an unknown variable `type` at parse time.
fn build_config(parser: TvixParserToml) -> Result<TvixEngineConfig, PipelineError> {
    let mut variables = Vec::with_capacity(parser.variables.len());
    for wire in parser.variables {
        let ty = TvixVarType::parse(&wire.ty).ok_or_else(|| {
            invalid(format!(
                "variable {:?} has unknown type {:?}; expected integer, number, boolean, or string",
                wire.name, wire.ty
            ))
        })?;
        variables.push(TvixVariable {
            name: wire.name,
            ty,
            default: wire.default,
            min: wire.min,
            max: wire.max,
            description: wire.description,
        });
    }
    Ok(TvixEngineConfig {
        expr: parser.expr,
        variables,
    })
}

/// Validate the engine configuration beyond the shared envelope: a non-empty
/// expression, unique identifier variable names, coherent numeric bounds, and
/// declared defaults that coerce within those bounds.
fn validate_config(config: &TvixEngineConfig) -> Result<(), PipelineError> {
    if config.expr.trim().is_empty() {
        return Err(invalid("parser.expr must not be empty".to_owned()));
    }
    let mut seen = HashSet::new();
    for variable in &config.variables {
        if !is_identifier(&variable.name) {
            return Err(invalid(format!(
                "variable name {:?} must be a non-empty lowercase ASCII identifier of letters, digits, or '_'",
                variable.name
            )));
        }
        if !seen.insert(variable.name.as_str()) {
            return Err(invalid(format!("duplicate variable {:?}", variable.name)));
        }
        match variable.ty {
            TvixVarType::Integer | TvixVarType::Number => {
                if let (Some(lo), Some(hi)) = (variable.min, variable.max) {
                    if lo > hi {
                        return Err(invalid(format!(
                            "variable {:?} declares min {lo} greater than max {hi}",
                            variable.name
                        )));
                    }
                }
            }
            TvixVarType::Boolean | TvixVarType::String => {
                if variable.min.is_some() || variable.max.is_some() {
                    return Err(invalid(format!(
                        "variable {:?} of type {} must not declare min/max",
                        variable.name,
                        variable.ty.as_str()
                    )));
                }
            }
        }
        if let Some(default) = &variable.default {
            coerce(variable, default)?;
        }
    }
    Ok(())
}

/// The tvix engine publishes a fixed projection, so a package cannot add
/// discovery fields or edge types: the graph carries only node ids and one
/// `type`. Reject a declared `[schema]` body rather than silently drop it.
fn validate_package_schema(schema: &PackageSchema) -> Result<(), PipelineError> {
    if !schema.fields.is_empty() {
        return Err(invalid(
            "the tvix engine publishes a fixed discovery schema and does not accept [[schema.fields]]"
                .to_owned(),
        ));
    }
    if !schema.edge_types.is_empty() {
        return Err(invalid(
            "the tvix engine publishes a single directed edge type and does not accept [[schema.edge_types]]"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Resolve administrator-supplied values against the declared variables,
/// applying defaults and rejecting unknown, missing, or out-of-bounds values.
/// The returned map holds every declared variable's validated source string;
/// [`render_expression`] turns it into Nix literals.
pub(crate) fn resolve_variables(
    package_id: &str,
    config: &TvixEngineConfig,
    supplied: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, PipelineError> {
    let declared: HashSet<&str> = config
        .variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect();
    for key in supplied.keys() {
        if !declared.contains(key.as_str()) {
            return Err(invalid(format!(
                "package {package_id:?} declares no variable {key:?}; it accepts: {}",
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
                    "package {package_id:?} requires variable {:?}{}",
                    variable.name,
                    variable
                        .description
                        .as_deref()
                        .map(|text| format!(" ({text})"))
                        .unwrap_or_default()
                ))
            })?;
        coerce(variable, &value)?;
        resolved.insert(variable.name.clone(), value);
    }
    Ok(resolved)
}

/// Render the callable Nix expression: `( <expr> ) { name = <literal>; … }`.
/// `resolved` must contain every declared variable (as [`resolve_variables`]
/// guarantees).
#[cfg(feature = "native")]
pub(crate) fn render_expression(
    config: &TvixEngineConfig,
    resolved: &BTreeMap<String, String>,
) -> Result<String, PipelineError> {
    let mut bindings = String::new();
    for variable in &config.variables {
        let raw = resolved.get(&variable.name).ok_or_else(|| {
            invalid(format!("unresolved variable {:?}", variable.name))
        })?;
        let literal = coerce(variable, raw)?;
        bindings.push_str(&format!("{} = {}; ", variable.name, literal));
    }
    Ok(format!("( {} ) {{ {}}}", config.expr, bindings))
}

/// Validate a supplied value against a variable's declared type and bounds and
/// render it as a Nix literal.
fn coerce(variable: &TvixVariable, raw: &str) -> Result<String, PipelineError> {
    match variable.ty {
        TvixVarType::Integer => {
            let value: i64 = raw.trim().parse().map_err(|_| {
                invalid(format!(
                    "variable {:?} expects an integer, got {:?}",
                    variable.name, raw
                ))
            })?;
            check_bounds(variable, value as f64, raw)?;
            Ok(value.to_string())
        }
        TvixVarType::Number => {
            let value: f64 = raw.trim().parse().map_err(|_| {
                invalid(format!(
                    "variable {:?} expects a number, got {:?}",
                    variable.name, raw
                ))
            })?;
            if !value.is_finite() {
                return Err(invalid(format!(
                    "variable {:?} must be a finite number, got {:?}",
                    variable.name, raw
                )));
            }
            check_bounds(variable, value, raw)?;
            Ok(render_float(value))
        }
        TvixVarType::Boolean => match raw.trim() {
            "true" => Ok("true".to_owned()),
            "false" => Ok("false".to_owned()),
            other => Err(invalid(format!(
                "variable {:?} expects true or false, got {:?}",
                variable.name, other
            ))),
        },
        TvixVarType::String => Ok(render_nix_string(raw)),
    }
}

fn check_bounds(variable: &TvixVariable, value: f64, raw: &str) -> Result<(), PipelineError> {
    if let Some(min) = variable.min {
        if value < min {
            return Err(invalid(format!(
                "variable {:?} value {raw} is below the minimum {min}",
                variable.name
            )));
        }
    }
    if let Some(max) = variable.max {
        if value > max {
            return Err(invalid(format!(
                "variable {:?} value {raw} is above the maximum {max}",
                variable.name
            )));
        }
    }
    Ok(())
}

/// Render an `f64` as a Nix float literal — always with a decimal point so Nix
/// reads it as a float, never scientific notation for ordinary magnitudes.
fn render_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Escape a string as a Nix double-quoted literal, neutralising `${`
/// antiquotation and the control escapes.
fn render_nix_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Canonical discovery fields every tvix package publishes, plus the single
/// directed edge type. The projection populates exactly these fields, so the
/// schema is fixed (no package additions).
fn tvix_schema() -> ImporterSchema {
    ImporterSchema::new(
        SOURCE_KIND,
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
                .searchable(2)
                .facetable(),
        ],
        vec![EdgeTypeSchema::directed(
            "declared",
            "Directed edge declared by the evaluated Nix graph",
        )],
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["application/x-nix"])
}

fn invalid(message: String) -> PipelineError {
    PipelineError::InvalidDescriptor {
        message: format!("tvix engine package: {message}"),
    }
}

/// Project a [`tvix_wasm::GeneratedGraph`] into the canonical graph. Each node's
/// Nix `type` becomes its single tag; edges reference the namespaced node ids.
/// This is the tvix projection folded out of the retired `tvix-loader` crate,
/// parameterised by the instance namespace so `source_id` varies per instance.
#[cfg(feature = "native")]
pub fn convert_generated_graph(
    generated: &tvix_wasm::GeneratedGraph,
    namespace: &Namespace,
    source_id: &str,
) -> Result<LoadResult, PipelineError> {
    let mut graph = VaultGraph::new();
    let mut search_documents = Vec::with_capacity(generated.nodes.len());

    for node in &generated.nodes {
        let tag = node.kind.as_deref().unwrap_or("node");
        let node_id = namespace.node_id(&node.id)?;
        let meta = NodeMeta {
            source_id: source_id.to_owned(),
            title: node.id.clone(),
            tags: vec![tag.to_string()],
            frontmatter: HashMap::new(),
            mtime: 0,
            path: node.id.clone(),
            doctype: Some("generated".into()),
            folder: String::new(),
            content_type: None,
            content_readable: false,
            content_writable: false,
        };
        graph.add_node(VaultNode {
            id: node_id.clone(),
            meta,
            metrics: NodeMetrics::default(),
            x: 0.0,
            y: 0.0,
        });
        search_documents.push(
            SearchDocument::new(&node_id)
                .with("id", node_id)
                .with("title", node.id.clone())
                .with("tags", serde_json::json!([tag]))
                .with("path", node.id.clone())
                .with("type", tag),
        );
    }

    for edge in &generated.edges {
        let source = namespace.node_id(&edge.source)?;
        let target = namespace.node_id(&edge.target)?;
        if graph.nodes.contains_key(&source) && graph.nodes.contains_key(&target) {
            graph.add_edge(VaultEdge { source, target });
        }
    }

    Ok(LoadResult {
        graph,
        search_documents,
        unresolved: Vec::new(),
    })
}

/// A validated tvix generator package bound to an instance: the rendered,
/// callable expression, the instance namespace, and the fixed schema. The
/// bound values are runtime configuration the package deliberately does not
/// contain.
#[cfg(feature = "native")]
pub struct TvixImporter {
    id: String,
    name: String,
    version: String,
    source_id: String,
    namespace: Namespace,
    expr: String,
    schema: ImporterSchema,
}

/// Bind a validated tvix package to an instance. Resolves `variables` against
/// the declared variables (applying defaults, validating type and bounds),
/// renders the callable Nix expression, and derives the `source_id` namespace.
#[cfg(feature = "native")]
pub fn build_tvix_importer(
    package: ValidatedPackage,
    variables: BTreeMap<String, String>,
    source_id: String,
) -> Result<TvixImporter, ImportError> {
    let config = package.tvix_config()?;
    let resolved = resolve_variables(&package.manifest().metadata.id, config, &variables)
        .map_err(|error| ImportError::TvixEngine(error.to_string()))?;
    let expr = render_expression(config, &resolved)
        .map_err(|error| ImportError::TvixEngine(error.to_string()))?;
    let namespace = Namespace::new(SOURCE_KIND, &source_id)
        .map_err(|error| ImportError::InvalidMetadata(error.to_string()))?;
    let metadata = &package.manifest().metadata;
    Ok(TvixImporter {
        id: metadata.id.clone(),
        name: metadata.name.clone(),
        version: metadata.version.clone(),
        source_id,
        namespace,
        expr,
        schema: tvix_schema(),
    })
}

#[cfg(feature = "native")]
impl TvixImporter {
    /// The bound instance's stable source id.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The rendered, callable Nix expression handed to `tvix_wasm::eval_graph`.
    pub fn expression(&self) -> &str {
        &self.expr
    }
}

#[cfg(feature = "native")]
impl Importer for TvixImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let read = Capability::new(Effect::Read, Transport::InMemory, self.source_id.clone());
        ImporterDescriptor::new(&self.id, &self.name, &self.version, vec![read], self.schema.clone())
            .with_watch(WatchPlan::Static)
    }

    fn import<'a>(
        &'a self,
        progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<ImportOutcome, PipelineError>> {
        Box::pin(async move {
            let stage = progress.stage(&format!("Evaluating {}", self.name));
            // `tvix_wasm::eval_graph` is synchronous and CPU-bound (and uses
            // non-Send `Rc` internals), so run it on a blocking thread. Only
            // the `expr` String and the returned graph cross the boundary.
            let expr = self.expr.clone();
            let evaluated = tokio::task::spawn_blocking(move || tvix_wasm::eval_graph(&expr)).await;
            let generated = match evaluated {
                Ok(Ok(graph)) => graph,
                Ok(Err(message)) => {
                    progress.fail(stage, &message);
                    return Err(PipelineError::Decode {
                        origin: SOURCE_KIND.to_owned(),
                        message,
                    });
                }
                Err(join) => {
                    let message = format!("tvix evaluation task failed: {join}");
                    progress.fail(stage, &message);
                    return Err(PipelineError::Decode {
                        origin: SOURCE_KIND.to_owned(),
                        message,
                    });
                }
            };
            progress.finish(stage);

            let project = progress.stage("Projecting graph");
            let result = convert_generated_graph(&generated, &self.namespace, &self.source_id);
            match &result {
                Ok(_) => progress.finish(project),
                Err(error) => progress.fail(project, &error.to_string()),
            }
            result.map(ImportOutcome::Loaded)
        })
    }
}

#[cfg(test)]
mod tests;
