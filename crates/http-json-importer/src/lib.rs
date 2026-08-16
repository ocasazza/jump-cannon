//! Declarative HTTP/JSON importer engine.
//!
//! This crate is a **mechanism**, not a data source. It reads paged JSON APIs
//! and projects their documents into the canonical graph according to a
//! [`manifest::ValidatedPackage`] — a versioned TOML package that carries no
//! data-source binding of its own. Adding another JSON API means writing a
//! package and binding it to an instance; it does not mean writing Rust, a
//! `SourceKind` variant, or a crate. See "Importers: packages, not crates" in
//! `AGENTS.md`.
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

pub mod connector;
pub mod manifest;
pub mod mapper;

use std::collections::BTreeMap;
use std::fmt;

use data_loader::{
    identity::Namespace, Capability, Decoder, DecodedRecord, Effect, ImportError, ImportPipeline,
    ImporterDescriptor, SourceConnector, SourceRecord, Transport, WatchPlan,
};
pub use connector::{HttpJsonConnector, JsonTransport, ReqwestTransport};
pub use manifest::{ValidatedPackage, SOURCE_KIND};
pub use mapper::ManifestMapper;

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
                message: format!("http-json instance source_id {message}"),
            }
        })?;
        let url = self.base_url.trim_end_matches('/');
        let absolute = url.starts_with("http://") || url.starts_with("https://");
        if !absolute || url.chars().any(char::is_whitespace) || url.len() <= "http://".len() {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "http-json instance base_url must be an absolute http(s) URL without whitespace, got {:?}",
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

/// Bind a validated package to an instance over the production HTTP transport.
pub fn build_importer(
    package: ValidatedPackage,
    instance: InstanceConfig,
) -> Result<ImportPipeline, ImportError> {
    let transport = ReqwestTransport::new(&instance, package.limits())?;
    build_importer_with_transport(package, instance, Box::new(transport))
}

/// Bind a package over an explicit transport. The network shell is one thin
/// implementation; tests inject a fixture transport, so no code path in this
/// crate requires a reachable API.
pub fn build_importer_with_transport(
    package: ValidatedPackage,
    instance: InstanceConfig,
    transport: Box<dyn JsonTransport>,
) -> Result<ImportPipeline, ImportError> {
    instance.validate()?;
    let variables = package.resolve_variables(&instance.variables)?;
    let namespace = Namespace::new(SOURCE_KIND, &instance.source_id)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The chart ships this exact file to graph-api under
    /// `httpJsonImporter.mountPath`; if it stops validating, deployments
    /// selecting `httpjson` fail loudly at startup rather than silently. Keep
    /// this in lockstep with `charts/jump-cannon/packages/hindsight-memory-bank.toml`.
    #[test]
    fn hindsight_memory_bank_package_validates() {
        let bytes = include_bytes!(
            "../../../charts/jump-cannon/packages/hindsight-memory-bank.toml"
        );
        let package = ValidatedPackage::from_toml_bytes(bytes)
            .expect("charts/jump-cannon/packages/hindsight-memory-bank.toml validates");
        assert!(
            !package.manifest().metadata.id.trim().is_empty(),
            "package must declare a stable metadata.id"
        );
        assert!(
            !package.collections().is_empty(),
            "package must declare at least one collection"
        );
        package
            .schema()
            .validate()
            .expect("published discovery schema is internally consistent");
    }
}
