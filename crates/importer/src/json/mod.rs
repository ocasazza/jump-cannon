//! The `json` engine: declarative paged-JSON-API importer packages.
//!
//! This engine is a **mechanism**, not a data source. It reads paged JSON
//! APIs and projects their documents into the canonical graph according to
//! the package's `[parser]` configuration — which carries no data-source
//! binding of its own. Adding another JSON API means writing a package and
//! binding it to an instance; it does not mean writing Rust, a `SourceKind`
//! variant, or a crate. See "Importers: packages, not crates" in `AGENTS.md`.
//!
//! The three responsibilities stay separated exactly as
//! [`data_loader::ImportPipeline`] requires:
//!
//! - [`connector::HttpJsonConnector`] is the only component that performs I/O.
//!   It resolves path templates, walks pagination, enforces byte and record
//!   bounds, and emits one [`data_loader::SourceRecord`] per fetched page.
//! - [`JsonDecoder`] is a pure wire-format decoder.
//! - [`mapper::ManifestMapper`] is a pure projection from decoded documents
//!   into nodes, edges, and discovery documents.
//!
//! Every node ID is `httpjson:{source_id}:{local}`, where `source_id` names
//! the bound instance and `local` is the package's identifier for the document
//! (prefixed per collection, e.g. `entity:`).

pub mod cache;
pub mod config;
pub mod connector;
pub mod mapper;

use std::collections::BTreeMap;
use std::fmt;

use data_loader::{
    identity::Namespace, Capability, Decoder, DecodedRecord, Effect, ImportError, ImportPipeline,
    ImporterDescriptor, SourceConnector, SourceRecord, Transport, WatchPlan,
};
pub use config::{
    Collection, Dedupe, DoctypeRule, EdgeListRules, EdgeRule, FieldRule, JsonEngineConfig, MatchOn,
    NodeRules, Pagination, Predicate, Preflight, Produces, TitleRule, Transform, VariableSpec,
    ENGINE, MAX_PAGE_SIZE, MAX_REQUEST_TIMEOUT_SECONDS, SOURCE_KIND,
};
pub use cache::{ByteCache, CachingJsonTransport};
pub use connector::{HttpJsonConnector, JsonTransport};
#[cfg(feature = "native")]
pub use connector::ReqwestTransport;
pub use mapper::ManifestMapper;

use crate::{EngineRuntime, ImporterManifest, ParserConfig, ValidatedPackage};

/// Metadata key naming the collection a [`SourceRecord`] was fetched for.
/// The mapper dispatches on it, so the connector must always set it.
pub const RECORD_COLLECTION_KEY: &str = "collection";
/// Metadata key holding the zero-based page index within a collection.
pub const RECORD_PAGE_KEY: &str = "page";

/// The administrator-supplied binding for one package: where the API lives,
/// which instance to read, and how often to poll.
///
/// This is the runtime configuration a package deliberately does not contain.
#[derive(Clone)]
pub struct InstanceConfig {
    /// Stable slug namespacing node IDs (`httpjson:{source_id}:…`).
    pub source_id: String,
    /// Root of the API, e.g. `http://hindsight-api-proxy.hindsight.svc.cluster.local`.
    pub base_url: String,
    /// Values for the package's declared variables (tenant, bank, …).
    pub variables: BTreeMap<String, String>,
    /// Optional bearer token. Attached as a header only; never logged.
    pub token: Option<String>,
    /// Poll cadence. Zero advertises a static one-shot snapshot.
    pub poll_interval_ms: u64,
}

impl fmt::Debug for InstanceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstanceConfig")
            .field("source_id", &self.source_id)
            .field("base_url", &self.base_url)
            .field("variables", &self.variables)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("poll_interval_ms", &self.poll_interval_ms)
            .finish()
    }
}

impl InstanceConfig {
    /// Reject bindings that could never produce a conformant import.
    pub fn validate(&self) -> Result<(), ImportError> {
        data_loader::identity::validate_source_id(&self.source_id).map_err(|message| {
            ImportError::InvalidDescriptor {
                message: format!("json engine instance source_id {message}"),
            }
        })?;
        let url = self.base_url.trim_end_matches('/');
        let absolute = url.starts_with("http://") || url.starts_with("https://");
        if !absolute || url.chars().any(char::is_whitespace) || url.len() <= "http://".len() {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "json engine instance base_url must be an absolute http(s) URL without whitespace, got {:?}",
                    self.base_url
                ),
            });
        }
        Ok(())
    }

    /// The API root with any trailing slash removed.
    pub fn root(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    pub fn watch_plan(&self) -> WatchPlan {
        if self.poll_interval_ms == 0 {
            WatchPlan::Static
        } else {
            WatchPlan::Poll {
                interval_ms: self.poll_interval_ms,
            }
        }
    }
}

/// Pure JSON wire-format decoder. Holds no authority to perform I/O.
pub struct JsonDecoder;

impl Decoder for JsonDecoder {
    fn decode(&self, record: SourceRecord) -> Result<DecodedRecord, ImportError> {
        let value = serde_json::from_slice(&record.bytes).map_err(|error| ImportError::Decode {
            origin: record.origin.clone(),
            message: format!("invalid JSON response: {error}"),
        })?;
        Ok(DecodedRecord {
            origin: record.origin,
            value,
            metadata: record.metadata,
        })
    }
}

/// Parse the full document, validate the shared envelope and the json engine
/// configuration, and prebuild the discovery schema.
pub(crate) fn validate(source: &str) -> Result<ValidatedPackage, crate::ImportError> {
    let wire: config::JsonManifestToml = toml::from_str(source)?;
    debug_assert_eq!(wire.format_version, crate::FORMAT_VERSION);
    debug_assert_eq!(wire.parser.engine, config::ENGINE);

    ValidatedPackage::validate_metadata(&wire.metadata)?;
    ValidatedPackage::validate_limits(wire.limits)?;
    if source.len() > wire.limits.manifest_bytes {
        return Err(crate::ImportError::ManifestTooLarge {
            actual: source.len(),
            max: wire.limits.manifest_bytes,
        });
    }

    let config = JsonEngineConfig {
        variables: wire.parser.variables,
        preflight: wire.parser.preflight,
        collections: wire.parser.collections,
        page_size: wire.parser.page_size,
        request_timeout_seconds: wire.parser.request_timeout_seconds,
    };
    config::validate_config(&config, &wire.schema)
        .map_err(|error| crate::ImportError::JsonEngine(error.to_string()))?;
    let schema = config::build_schema(&wire.schema);
    schema
        .validate()
        .map_err(|error| crate::ImportError::JsonEngine(error.to_string()))?;

    let manifest = ImporterManifest {
        format_version: wire.format_version,
        metadata: wire.metadata,
        limits: wire.limits,
        schema: wire.schema,
        parser: ParserConfig::Json(config),
    };
    Ok(ValidatedPackage {
        manifest,
        schema,
        runtime: EngineRuntime::Json,
    })
}

/// Resolve administrator-supplied values against the package's declared
/// variables. Pest packages have no variables: they fail with a typed
/// wrong-engine error.
pub(crate) fn resolve_variables(
    package: &ValidatedPackage,
    supplied: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ImportError> {
    let config = package
        .json_config()
        .map_err(|error| ImportError::InvalidDescriptor {
            message: error.to_string(),
        })?;
    config::resolve_variables(&package.manifest().metadata.id, config, supplied)
}

/// Bind a validated package to an instance over the production HTTP transport.
#[cfg(feature = "native")]
pub fn build_importer(
    package: ValidatedPackage,
    instance: InstanceConfig,
    cache_dir: Option<std::path::PathBuf>,
) -> Result<ImportPipeline, ImportError> {
    let config = package
        .json_config()
        .map_err(|error| ImportError::InvalidDescriptor {
            message: error.to_string(),
        })?;
    let transport = ReqwestTransport::new(
        &instance,
        package.limits(),
        config.request_timeout_seconds,
    )?;
    build_importer_with_transport(package, instance, Box::new(transport), cache_dir)
}

/// Bind a package over an explicit transport. The network shell is one thin
/// implementation; tests inject a fixture transport, so no code path in this
/// crate requires a reachable API.
pub fn build_importer_with_transport(
    package: ValidatedPackage,
    instance: InstanceConfig,
    transport: Box<dyn JsonTransport>,
    cache_dir: Option<std::path::PathBuf>,
) -> Result<ImportPipeline, ImportError> {
    instance.validate()?;
    // Reject non-json packages before anything is constructed on their behalf.
    package
        .json_config()
        .map_err(|error| ImportError::InvalidDescriptor {
            message: error.to_string(),
        })?;
    let variables = package.resolve_variables(&instance.variables)?;
    let namespace = Namespace::new(SOURCE_KIND, &instance.source_id)?;

    // Wrap transport in a response-body cache when a cache directory is
    // configured. The cache namespace is {cache_dir}/{source_id}/{varhash}
    // so variable changes create a fresh namespace without invalidating
    // the old one (it becomes eligible for GC on next restart).
    let transport: Box<dyn JsonTransport> = if let Some(ref cache_root) = cache_dir {
        let var_hash = blake3::hash(
            serde_json::to_string(&variables)
                .unwrap_or_default()
                .as_bytes(),
        );
        let namespace_dir = cache_root
            .join(&instance.source_id)
            .join(var_hash.to_hex().as_str());
        let ttl = if instance.poll_interval_ms > 0 {
            std::time::Duration::from_millis(instance.poll_interval_ms)
        } else {
            std::time::Duration::from_secs(3600)
        };
        match cache::ByteCache::open(namespace_dir, ttl) {
            Ok(cache) => Box::new(cache::CachingJsonTransport::new(transport, cache)),
            Err(error) => {
                tracing::warn!(?error, "failed to open httpjson cache, proceeding uncached");
                transport
            }
        }
    } else {
        transport
    };

    let connector = HttpJsonConnector::new(
        package.clone(),
        instance.clone(),
        variables.clone(),
        transport,
    )?;
    let mapper = ManifestMapper::new(package.clone(), namespace);

    let watch = instance.watch_plan();
    let mut capabilities = connector.capabilities(Effect::Read);
    if !matches!(watch, WatchPlan::Static) {
        capabilities.extend(connector.capabilities(Effect::Watch));
    }
    let metadata = &package.manifest().metadata;
    let descriptor = ImporterDescriptor::new(
        format!("{}.{}", metadata.id, instance.source_id),
        instance_display_name(&package, &variables),
        metadata.version.clone(),
        capabilities,
        package.schema().clone(),
    )
    .with_watch(watch);

    ImportPipeline::new(
        descriptor,
        Box::new(connector),
        Box::new(JsonDecoder),
        Box::new(mapper),
    )
}

/// `Hindsight memory bank (bank=omp)` — the package name plus the bound
/// variables, so operators can tell two instances apart in `GET /importers`.
fn instance_display_name(
    package: &ValidatedPackage,
    variables: &BTreeMap<String, String>,
) -> String {
    let name = package.manifest().metadata.name.clone();
    if variables.is_empty() {
        return name;
    }
    let bound = variables
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name} ({bound})")
}

/// One exact capability tuple over the API root. Collections scope their own
/// read capabilities through the connector.
pub fn root_capability(effect: Effect, root: &str) -> Capability {
    Capability::new(effect, Transport::Http, root.to_string())
}

/// Shared minimal json package for cross-engine tests (e.g. the pest engine's
/// wrong-engine rejection tests).
#[cfg(test)]
pub(crate) mod tests_support {
    use crate::ValidatedPackage;

    pub(crate) fn minimal_json_package() -> ValidatedPackage {
        ValidatedPackage::from_toml(
            r#"format_version = 3

[metadata]
id = "test.minimal-json"
name = "Minimal json"
version = "1.0.0"

[[schema.edge_types]]
key = "related"
directed = true

[parser]
engine = "json"

[[parser.collections]]
name = "records"
path = "/v1/records"

[parser.collections.nodes]
id_pointer = "/id"
node_type = "record"

[parser.collections.nodes.title]
pointer = "/name"
"#,
        )
        .expect("minimal json package validates")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chart ships these exact files to graph-api under the packages
    /// mount; if one stops validating, deployments selecting it fail loudly
    /// at startup rather than silently. Keep this in lockstep with
    /// `charts/jump-cannon/packages/`.
    #[test]
    fn shipped_json_packages_validate() {
        let shipped: [(&str, &[u8]); 3] = [
            (
                "hindsight-memory-bank.toml",
                include_bytes!("../../../../charts/jump-cannon/packages/hindsight-memory-bank.toml"),
            ),
            (
                "chembl-pharmacology.toml",
                include_bytes!("../../../../charts/jump-cannon/packages/chembl-pharmacology.toml"),
            ),
            (
                "openalex-works.toml",
                include_bytes!("../../../../charts/jump-cannon/packages/openalex-works.toml"),
            ),
        ];
        for (name, bytes) in shipped {
            let package = ValidatedPackage::from_toml_bytes(bytes)
                .unwrap_or_else(|error| panic!("{name} validates: {error}"));
            assert_eq!(package.engine(), crate::EngineKind::Json, "{name}");
            assert!(
                !package.manifest().metadata.id.trim().is_empty(),
                "{name} must declare a stable metadata.id"
            );
            let config = package
                .json_config()
                .unwrap_or_else(|_| panic!("{name} exposes a json config"));
            assert!(
                !config.collections.is_empty(),
                "{name} must declare at least one collection"
            );
            package
                .schema()
                .validate()
                .unwrap_or_else(|error| panic!("{name} discovery schema is consistent: {error}"));
        }
    }

    #[test]
    fn build_importer_rejects_pest_engine_packages() {
        let pest_package = ValidatedPackage::from_toml(
            r#"format_version = 3

[metadata]
id = "test.minimal-pest"
name = "Minimal pest"
version = "1.0.0"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''
document = { SOI ~ EOI }
node = { "n" }
node_id = { "i" }
title = { "t" }
kind = { "k" }
tag = { "g" }
property = { "p" }
key = { "y" }
value = { "v" }
edge = { "e" }
source = { "s" }
target = { "r" }
'''

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
"#,
        )
        .expect("minimal pest package validates");

        struct NoopTransport;
        impl JsonTransport for NoopTransport {
            fn get<'a>(
                &'a self,
                url: &'a str,
            ) -> data_loader::ImportFuture<'a, Result<Vec<u8>, ImportError>> {
                let _ = url;
                Box::pin(async { unreachable!("no request may be issued") })
            }
        }

        let instance = InstanceConfig {
            source_id: "test".into(),
            base_url: "http://api.example.test".into(),
            variables: BTreeMap::new(),
            token: None,
            poll_interval_ms: 0,
        };
        let error = build_importer_with_transport(pest_package, instance, Box::new(NoopTransport), None)
            
            .err()
            .expect("a pest package must not bind to an HTTP instance");
        assert!(
            matches!(error, ImportError::InvalidDescriptor { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("json"), "{error}");
    }
}
