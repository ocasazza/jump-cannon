//! Unified runtime importer engine for jump-cannon.
//!
//! An importer package is one versioned TOML file (`format_version = 3`)
//! containing a shared envelope — `[metadata]`, `[limits]`, and
//! `[[schema.fields]]` — plus a `[parser]` table selecting the engine:
//!
//! - `engine = "pest"` carries `{ root_rule, grammar, [captures] }`: an inline
//!   Pest grammar whose semantic capture rules are projected into the
//!   canonical graph. See [`pest`].
//! - `engine = "json"` carries the declarative paged-JSON manifest body
//!   (variables, preflight, collections, mapping). See [`json`].
//! - `engine = "tvix"` carries an inline Nix generator expression that is a
//!   function of typed, bounded `[[parser.variables]]`; the engine binds those
//!   variables at apply time and evaluates the expression into a graph. See
//!   [`tvix`].
//!
//! The package deliberately contains **no data source binding and no
//! credentials**. An administrator binds a validated package to an explicit
//! input at runtime: a filesystem path ([`pest::FilesystemImporter`]), an
//! HTTP/JSON instance ([`InstanceConfig`] + [`build_importer`]), or a set of
//! generator parameters ([`build_tvix_importer`]). Tokens are
//! injected through instance configuration, redacted from `Debug`, capability
//! scopes, and error messages.
//!
//! Packages from the retired per-engine crates (`format_version = 1` or `2`)
//! are rejected with an explicit upgrade error; there is no silent
//! compatibility path.
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
//!
//! # Feature split
//!
//! The default `native` feature gates filesystem loading and the reqwest
//! HTTP transport. The package core — manifest validation, [`pest`]-engine
//! `parse_input` on caller-supplied bytes, and the [`json`] decode/map
//! pipeline over an injected transport — builds on `wasm32-unknown-unknown`
//! with `--no-default-features`.

pub mod json;
pub mod pest;
pub mod tvix;

use serde::Deserialize;

use data_loader::{DiscoveryField, EdgeTypeSchema, ImportError as PipelineError, ImporterSchema};

pub use json::{
    build_importer_with_transport, InstanceConfig, JsonDecoder, ManifestMapper,
    RECORD_COLLECTION_KEY, RECORD_PAGE_KEY,
};
#[cfg(feature = "native")]
pub use json::build_importer;
#[cfg(feature = "native")]
pub use pest::{FilesystemImporter, FilesystemLoader};
#[cfg(feature = "native")]
pub use tvix::{build_tvix_importer, TvixImporter};

/// Importer package format understood by this crate.
pub const FORMAT_VERSION: u32 = 3;

/// Maximum limits accepted from an importer package.
///
/// A package may lower any of these values, but validation rejects zero values
/// and values above this ceiling. The ceiling is the union of the two legacy
/// per-engine formats; packages should declare tight per-package values.
pub const HARD_LIMITS: Limits = Limits {
    manifest_bytes: 1024 * 1024,
    grammar_bytes: 64 * 1024,
    input_bytes: 256 * 1024 * 1024,
    nodes: 1_000_000,
    edges: 500_000,
};

/// Storage limits declared by an importer package.
///
/// Field meaning is engine-specific: `grammar_bytes` bounds the pest inline
/// grammar; `input_bytes` bounds one pest input file or one json response
/// body; `nodes` bounds pest node records or json records per collection;
/// `edges` bounds pest edge records (json edges are bounded by `nodes`).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum UTF-8 TOML package size.
    #[serde(default = "default_manifest_bytes")]
    pub manifest_bytes: usize,
    /// Maximum inline grammar size (pest engine).
    #[serde(default = "default_grammar_bytes")]
    pub grammar_bytes: usize,
    /// Maximum input size: one pest input document, or one json response body.
    #[serde(default = "default_input_bytes")]
    pub input_bytes: usize,
    /// Maximum node records (json: records per collection).
    #[serde(default = "default_nodes")]
    pub nodes: usize,
    /// Maximum edge records, including dangling edges (pest engine).
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

/// Package-declared discovery fields and edge semantics. The canonical
/// `id`/`title`/`tags`/`path`/`type` fields are supplied by the engine; a
/// package declares only what it adds. A pest package that declares no edge
/// types publishes the built-in directed `declared` edge type.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSchema {
    #[serde(default)]
    pub fields: Vec<DiscoveryField>,
    #[serde(default)]
    pub edge_types: Vec<EdgeTypeSchema>,
}

/// Engine-specific `[parser]` configuration of a validated package.
#[derive(Debug, Clone)]
pub enum ParserConfig {
    Pest(pest::PestEngineConfig),
    Json(json::JsonEngineConfig),
    Tvix(tvix::TvixEngineConfig),
}

/// The engine a validated package selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Pest,
    Json,
    Tvix,
}

impl EngineKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pest => pest::ENGINE,
            Self::Json => json::ENGINE,
            Self::Tvix => tvix::ENGINE,
        }
    }
}

/// Unified, validated view of a package manifest. Construction is only
/// possible through [`ValidatedPackage`], so every field is already checked.
#[derive(Debug, Clone)]
pub struct ImporterManifest {
    /// Package format version; always [`FORMAT_VERSION`].
    pub format_version: u32,
    /// Human-facing identity and release metadata.
    pub metadata: PackageMetadata,
    /// Per-package limits, bounded by [`HARD_LIMITS`].
    pub limits: Limits,
    /// Package-declared discovery fields and edge types.
    pub schema: PackageSchema,
    /// Engine selection and engine-specific configuration.
    pub parser: ParserConfig,
}

/// Package validation, input, parsing, and mapping failures.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("importer manifest is {actual} bytes; limit is {max} bytes")]
    ManifestTooLarge { actual: usize, max: usize },

    #[error("importer manifest is not UTF-8: {0}")]
    ManifestUtf8(#[source] std::str::Utf8Error),

    #[error("invalid importer TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error(
        "unsupported importer package format_version {found}; upgrade the package to format_version {supported}"
    )]
    UnsupportedFormatVersion { found: u32, supported: u32 },

    #[error("unknown parser engine {engine:?}; expected \"pest\", \"json\", or \"tvix\"")]
    UnknownEngine { engine: String },

    #[error("invalid importer metadata: {0}")]
    InvalidMetadata(String),

    #[error("invalid '{name}' limit {value}; value must be between 1 and {hard_max}")]
    InvalidLimit {
        name: &'static str,
        value: usize,
        hard_max: usize,
    },

    #[error("invalid json engine package: {0}")]
    JsonEngine(String),

    #[error("invalid tvix engine package: {0}")]
    TvixEngine(String),

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
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("importer input '{path}' is not UTF-8: {source}")]
    InputUtf8 {
        path: std::path::PathBuf,
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

    #[error("package engine is \"{actual}\"; {op} requires the \"{needed}\" engine")]
    WrongEngine {
        op: &'static str,
        needed: &'static str,
        actual: &'static str,
    },
}

/// Reads only `format_version`, so packages from older formats fail with the
/// upgrade error rather than a shape error against the v3 envelope.
#[derive(Deserialize)]
struct VersionProbe {
    format_version: u32,
}

/// Reads only the engine tag; the engine's own parser then validates the full
/// document with `deny_unknown_fields`.
#[derive(Deserialize)]
struct EngineProbe {
    parser: EngineProbeParser,
}

#[derive(Deserialize)]
struct EngineProbeParser {
    engine: String,
}

/// A TOML package whose shared envelope, limits, schema, and engine
/// configuration have been validated.
#[derive(Clone)]
pub struct ValidatedPackage {
    pub(crate) manifest: ImporterManifest,
    pub(crate) schema: ImporterSchema,
    pub(crate) runtime: EngineRuntime,
}

impl std::fmt::Debug for ValidatedPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedPackage")
            .field("manifest", &self.manifest)
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) enum EngineRuntime {
    Pest(pest::PestRuntime),
    Json,
    Tvix,
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
        let text = std::str::from_utf8(source).map_err(ImportError::ManifestUtf8)?;
        let version: VersionProbe = toml::from_str(text)?;
        if version.format_version != FORMAT_VERSION {
            return Err(ImportError::UnsupportedFormatVersion {
                found: version.format_version,
                supported: FORMAT_VERSION,
            });
        }
        let probe: EngineProbe = toml::from_str(text)?;
        match probe.parser.engine.as_str() {
            pest::ENGINE => pest::validate(text),
            json::ENGINE => json::validate(text),
            tvix::ENGINE => tvix::validate(text),
            other => Err(ImportError::UnknownEngine {
                engine: other.to_owned(),
            }),
        }
    }

    /// The validated manifest.
    pub fn manifest(&self) -> &ImporterManifest {
        &self.manifest
    }

    /// The discovery schema this package publishes.
    pub fn schema(&self) -> &ImporterSchema {
        &self.schema
    }

    /// The validated per-package limits.
    pub fn limits(&self) -> Limits {
        self.manifest.limits
    }

    /// The engine this package selects.
    pub fn engine(&self) -> EngineKind {
        match &self.manifest.parser {
            ParserConfig::Pest(_) => EngineKind::Pest,
            ParserConfig::Json(_) => EngineKind::Json,
            ParserConfig::Tvix(_) => EngineKind::Tvix,
        }
    }

    /// The pest engine configuration, or a typed error for json packages.
    pub fn pest_config(&self) -> Result<&pest::PestEngineConfig, ImportError> {
        match &self.manifest.parser {
            ParserConfig::Pest(config) => Ok(config),
            ParserConfig::Json(_) => Err(ImportError::WrongEngine {
                op: "pest engine access",
                needed: pest::ENGINE,
                actual: json::ENGINE,
            }),
            ParserConfig::Tvix(_) => Err(ImportError::WrongEngine {
                op: "pest engine access",
                needed: pest::ENGINE,
                actual: tvix::ENGINE,
            }),
        }
    }

    /// The json engine configuration, or a typed error for pest packages.
    pub fn json_config(&self) -> Result<&json::JsonEngineConfig, ImportError> {
        match &self.manifest.parser {
            ParserConfig::Json(config) => Ok(config),
            ParserConfig::Pest(_) => Err(ImportError::WrongEngine {
                op: "json engine access",
                needed: json::ENGINE,
                actual: pest::ENGINE,
            }),
            ParserConfig::Tvix(_) => Err(ImportError::WrongEngine {
                op: "json engine access",
                needed: json::ENGINE,
                actual: tvix::ENGINE,
            }),
        }
    }

    /// The tvix engine configuration, or a typed error for pest/json packages.
    pub fn tvix_config(&self) -> Result<&tvix::TvixEngineConfig, ImportError> {
        match &self.manifest.parser {
            ParserConfig::Tvix(config) => Ok(config),
            ParserConfig::Pest(_) => Err(ImportError::WrongEngine {
                op: "tvix engine access",
                needed: tvix::ENGINE,
                actual: pest::ENGINE,
            }),
            ParserConfig::Json(_) => Err(ImportError::WrongEngine {
                op: "tvix engine access",
                needed: tvix::ENGINE,
                actual: json::ENGINE,
            }),
        }
    }

    /// Pest engine: parse one UTF-8 input and deterministically map its
    /// semantic captures to a fresh graph.
    pub fn parse_input(
        &self,
        input: &str,
    ) -> Result<data_loader::LoadResult, ImportError> {
        let EngineRuntime::Pest(runtime) = &self.runtime else {
            return Err(ImportError::WrongEngine {
                op: "parse_input",
                needed: pest::ENGINE,
                actual: json::ENGINE,
            });
        };
        pest::parse_input(self, runtime, input)
    }

    /// Resolve administrator-supplied values against the package's declared
    /// variables, applying defaults and rejecting unknown or unusable values.
    /// Dispatches per engine: the json engine validates URL path segments; the
    /// tvix engine validates typed, bounded generator parameters. Pest packages
    /// declare no variables and fail with a typed wrong-engine error.
    pub fn resolve_variables(
        &self,
        supplied: &std::collections::BTreeMap<String, String>,
    ) -> Result<std::collections::BTreeMap<String, String>, PipelineError> {
        match &self.manifest.parser {
            ParserConfig::Tvix(config) => {
                tvix::resolve_variables(&self.manifest.metadata.id, config, supplied)
            }
            _ => json::resolve_variables(self, supplied),
        }
    }

    /// Shared metadata validation: the package id namespaces every emitted
    /// node ID, so it follows the shared source-id charset; the version must
    /// be semantic.
    pub(crate) fn validate_metadata(metadata: &PackageMetadata) -> Result<(), ImportError> {
        data_loader::identity::validate_source_id(&metadata.id).map_err(|_| {
            ImportError::InvalidMetadata(
                "metadata.id must be a non-empty lowercase ASCII identifier of at most 128 bytes using letters, digits, '.', '-', or '_'"
                    .to_owned(),
            )
        })?;
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

    /// Shared limit validation: every declared value must be between 1 and
    /// the [HARD_LIMITS] ceiling.
    pub(crate) fn validate_limits(limits: Limits) -> Result<(), ImportError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v2 pest packages (top-level [captures], no engine tag) must fail with
    /// the explicit upgrade error, never a shape error or silent acceptance.
    #[test]
    fn v2_packages_fail_with_an_upgrade_error() {
        let v2 = r#"format_version = 2

[metadata]
id = "example.legacy"
name = "Legacy"
version = "1.0.0"

[parser]
root_rule = "document"
grammar = "document = { SOI ~ EOI }"

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

[schema]
"#;
        let error = ValidatedPackage::from_toml(v2).expect_err("v2 must be rejected");
        assert!(matches!(
            error,
            ImportError::UnsupportedFormatVersion {
                found: 2,
                supported: FORMAT_VERSION
            }
        ));
        assert!(
            error.to_string().contains("upgrade the package to format_version 3"),
            "error must name the upgrade path: {error}"
        );
    }

    /// v1 http-json packages fail on the same explicit upgrade path.
    #[test]
    fn v1_packages_fail_with_an_upgrade_error() {
        let v1 = r#"format_version = 1

[metadata]
id = "example.legacy"
name = "Legacy"
version = "1.0.0"

[[collections]]
name = "memories"
path = "/v1/memories"

[collections.nodes]
id_pointer = "/id"
node_type = "memory"

[collections.nodes.title]
pointer = "/text"

[schema]
[[schema.edge_types]]
key = "mentions"
directed = true
"#;
        let error = ValidatedPackage::from_toml(v1).expect_err("v1 must be rejected");
        assert!(matches!(
            error,
            ImportError::UnsupportedFormatVersion {
                found: 1,
                supported: FORMAT_VERSION
            }
        ));
    }

    #[test]
    fn unknown_engines_are_rejected_loudly() {
        let unknown = r#"format_version = 3

[metadata]
id = "example.unknown"
name = "Unknown"
version = "1.0.0"

[parser]
engine = "protobuf"

[schema]
"#;
        let error = ValidatedPackage::from_toml(unknown).expect_err("unknown engine must fail");
        assert!(
            matches!(error, ImportError::UnknownEngine { .. }),
            "expected UnknownEngine, got {error:?}"
        );
        assert!(error.to_string().contains("protobuf"), "{error}");
    }

    #[test]
    fn envelope_unknown_fields_are_rejected() {
        let typo = r#"format_version = 3

[metadata]
id = "example.typo"
name = "Typo"
version = "1.0.0"
versoin = "9.9.9"

[parser]
engine = "pest"
root_rule = "document"
grammar = "document = { SOI ~ EOI }"

[parser.captures]
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

[schema]
"#;
        let error = ValidatedPackage::from_toml(typo).expect_err("typo field must fail");
        assert!(matches!(error, ImportError::Toml(_)), "got {error:?}");
        assert!(error.to_string().contains("versoin"), "{error}");
    }
}
