//! Source-neutral importer contracts.
//!
//! [`Importer`] is the asynchronous implementation contract and
//! [`HostedImporter`] pairs it with host-owned authority. [`SourceConnector`],
//! [`Decoder`], and [`GraphMapper`] keep effects separate from pure parsing and
//! graph projection, while [`Loader`] remains as a compatibility contract for
//! the original Obsidian, tvix, and generated graph adapters.

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vault_data::VaultGraph;

/// The result of a single load pass.
#[derive(Debug)]
pub struct LoadResult {
    /// The populated graph (nodes + resolved edges).
    pub graph: VaultGraph,
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

    /// Produce a fresh graph from the source.
    fn load(&self) -> LoadResult;

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
            _ => None,
        }
    }

    /// All known source kinds (for help text).
    pub fn all() -> &'static [&'static str] {
        &["obsidian", "tvix", "generate", "kubernetes", "okf", "pest"]
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
pub struct ImporterDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub watch: WatchPlan,
}

impl ImporterDescriptor {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        capabilities: Vec<Capability>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            capabilities,
            watch: WatchPlan::Static,
        }
    }

    pub fn with_watch(mut self, watch: WatchPlan) -> Self {
        self.watch = watch;
        self
    }
}

/// A fallible asynchronous graph importer.
///
/// The boxed-future method keeps the trait object-safe without requiring an
/// `async-trait` transformation.
pub trait Importer: Send + Sync {
    fn descriptor(&self) -> ImporterDescriptor;
    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>>;
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
        )
        .with_watch(watch)
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move { Ok(self.load()) })
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
        if descriptor.id.trim().is_empty()
            || descriptor.name.trim().is_empty()
            || descriptor.version.trim().is_empty()
        {
            return Err(ImportError::InvalidDescriptor {
                message: "id, name, and version must be non-empty".into(),
            });
        }
        if !descriptor
            .capabilities
            .iter()
            .any(|capability| capability.effect == Effect::Read)
        {
            return Err(ImportError::InvalidDescriptor {
                message: "an importer must request at least one read capability".into(),
            });
        }

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

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move {
            let descriptor = self.importer.descriptor();
            for capability in descriptor
                .capabilities
                .iter()
                .filter(|capability| capability.effect == Effect::Read)
            {
                self.authorize(capability)?;
            }
            self.importer.import().await
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedRecord {
    pub origin: String,
    pub value: serde_json::Value,
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
        if descriptor.id.trim().is_empty()
            || descriptor.name.trim().is_empty()
            || descriptor.version.trim().is_empty()
        {
            return Err(ImportError::InvalidDescriptor {
                message: "id, name, and version must be non-empty".into(),
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
        let decoded = source_records
            .into_iter()
            .map(|record| self.decoder.decode(record))
            .collect::<Result<Vec<_>, _>>()?;
        self.mapper.map(decoded)
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

    fn descriptor(capabilities: Vec<Capability>) -> ImporterDescriptor {
        ImporterDescriptor::new("fake", "Fake", "1", capabilities)
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
            for record in records {
                graph.add_node(VaultNode {
                    id: record.origin,
                    ..Default::default()
                });
            }
            Ok(LoadResult {
                graph,
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

    struct LegacyLoader;

    impl Loader for LegacyLoader {
        fn name(&self) -> &str {
            "legacy"
        }

        fn load(&self) -> LoadResult {
            LoadResult {
                graph: VaultGraph::new(),
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
}
