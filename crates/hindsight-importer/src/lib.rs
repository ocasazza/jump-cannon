//! Hindsight memory-bank importer.
//!
//! Hindsight (https://github.com/ocasazza/hindsight-style memory service) is a
//! fleet memory backend organized into **banks**. One importer instance
//! publishes exactly one bank as a jump-cannon graph:
//!
//! - **memory units** (consolidated facts) become `memory` nodes, searchable
//!   by their full text, context, entities, and tags,
//! - **entities** (canonical concepts extracted from the corpus) become
//!   `entity` nodes,
//! - **documents** (the retained source transcripts) become `document` nodes,
//! - unit→unit graph links reported by Hindsight (`temporal`, `semantic`,
//!   `caused_by`) become edges verbatim,
//! - each unit's extracted entities become `mentions` edges to `entity`
//!   nodes, and each unit's origin document becomes a `documented_in` edge.
//!
//! Hindsight's unit↔unit `entity` links (two units share an entity) are
//! deliberately **not** imported: the bipartite `mentions` edges express the
//! same adjacency without the quadratic blow-up.
//!
//! The bank is a first-class selection: the configured bank must exist in
//! `GET /v1/{tenant}/banks` and a mistyped bank fails the import with the
//! list of banks that do exist. Node IDs live under
//! `hindsight:{source_id}:` where `source_id` defaults to the sanitized bank
//! id (see [`data_loader::identity`]).
//!
//! # Content
//!
//! Content stays non-readable/non-writable: the corpus lives behind the
//! Hindsight API, not in the local vault root, so the document editor path
//! (`/vault/page`) does not apply. Unit text is still fully indexed as the
//! `body` search field.

mod client;

use std::collections::{HashMap, HashSet};
use std::fmt;

use data_loader::{
    identity::{self, Namespace},
    Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportError,
    ImportFuture, Importer, ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument,
    TagHierarchySchema, Transport, WatchPlan,
};
use vault_data::{VaultEdge, VaultGraph, VaultNode};

pub use client::{HindsightApi, HttpHindsightApi};

/// Default bound on imported memory units (and, independently per
/// collection, entities and documents). Sized to leave headroom in
/// graph-api's 2 GiB chart limit.
pub const DEFAULT_MAX_UNITS: usize = 50_000;

/// Page size used when paginating list endpoints.
const PAGE_SIZE: usize = 500;

/// Characters kept in a memory unit title before truncation.
const TITLE_MAX_CHARS: usize = 72;

/// linkType values imported as unit↔unit edges. `entity` is excluded on
/// purpose (see the module docs).
const IMPORTED_LINK_TYPES: [&str; 3] = ["temporal", "semantic", "caused_by"];

/// Configuration for one Hindsight bank source instance.
///
/// Construct directly or via graph-api's `--hindsight-*` flags; always
/// validate through [`HindsightImporter::new`].
#[derive(Clone)]
pub struct HindsightSourceConfig {
    /// Root of the Hindsight HTTP API, e.g.
    /// `http://hindsight-api-proxy.hindsight.svc.cluster.local`.
    pub base_url: String,
    /// Tenant path segment of the API (`/v1/{tenant}/banks/...`).
    pub tenant: String,
    /// The bank to import. Must exist in the tenant's bank list.
    pub bank: String,
    /// Optional bearer token. From env only; never logged or serialized.
    pub token: Option<String>,
    /// Poll cadence advertised through [`WatchPlan::Poll`]. Zero advertises a
    /// static one-shot snapshot.
    pub poll_interval_ms: u64,
    /// Stable source-instance identifier namespacing every node ID. Defaults
    /// to the sanitized bank id.
    pub source_id: Option<String>,
    /// Hard bound on imported units (and per-collection entity/document
    /// counts). A bank larger than this is an error, not a truncation.
    pub max_units: usize,
}

impl fmt::Debug for HindsightSourceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HindsightSourceConfig")
            .field("base_url", &self.base_url)
            .field("tenant", &self.tenant)
            .field("bank", &self.bank)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("source_id", &self.source_id)
            .field("max_units", &self.max_units)
            .finish()
    }
}

impl HindsightSourceConfig {
    /// Reject configurations that could never produce a conformant import.
    pub fn validate(&self) -> Result<(), ImportError> {
        let origin = self.base_url.clone();
        let bad = |message: String| ImportError::InvalidDescriptor {
            message: format!("Hindsight {message}"),
        };
        let valid_url = (self.base_url.starts_with("http://")
            || self.base_url.starts_with("https://"))
            && !self.base_url.chars().any(char::is_whitespace)
            && self.base_url.trim_end_matches('/').len() > "http://".len();
        if !valid_url {
            return Err(bad(format!(
                "base_url must be an absolute http(s) URL without whitespace, got {:?}",
                origin
            )));
        }
        for (field, value) in [("tenant", &self.tenant), ("bank", &self.bank)] {
            if value.is_empty()
                || value.len() > 128
                || value
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '%'))
            {
                return Err(bad(format!(
                    "{field} must be a non-empty URL path segment without whitespace, /, ?, #, or %, got {value:?}"
                )));
            }
        }
        identity::validate_source_id(&self.resolved_source_id())
            .map_err(|message| bad(format!("source_id {message}")))?;
        if self.max_units == 0 {
            return Err(bad("max_units must be non-zero".into()));
        }
        Ok(())
    }

    /// The source_id in effect: the explicit override or the sanitized bank.
    pub fn resolved_source_id(&self) -> String {
        self.source_id
            .clone()
            .unwrap_or_else(|| sanitize_source_id(&self.bank))
    }

    /// Root path every endpoint hangs off (`/v1/{tenant}`).
    fn api_root(&self) -> String {
        format!("/v1/{}", self.tenant)
    }

    /// Exact capability tuples for `effect` (the API base URL is the scope).
    pub fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        vec![Capability::new(
            effect,
            Transport::Http,
            self.base_url.trim_end_matches('/').to_string(),
        )]
    }

    /// How the host should learn the bank may have changed.
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

/// Derive a contract-valid `source_id` from a bank id
/// (`jira-ithelp` → `jira-ithelp`, `IT Ops` → `it-ops`). Characters outside
/// `[a-z0-9._-]` fold to `-`; an unusable bank falls back to `hindsight`.
pub fn sanitize_source_id(bank: &str) -> String {
    let sanitized: String = bank
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .take(identity::MAX_SOURCE_ID_BYTES)
        .collect();
    if sanitized.is_empty() {
        "hindsight".to_string()
    } else {
        sanitized
    }
}

// ---------------------------------------------------------------------------
// Wire records (parsed JSON shapes of the Hindsight API)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct BanksResponse {
    #[serde(default)]
    banks: Vec<BankSummary>,
}

#[derive(Debug, serde::Deserialize)]
struct BankSummary {
    bank_id: String,
}


#[derive(Debug, serde::Deserialize)]
struct MemoryUnit {
    id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    fact_type: Option<String>,
    #[serde(default)]
    document_id: Option<String>,
    /// Comma-separated extracted entity names, e.g. `"tofu, state-metadata.py"`.
    #[serde(default)]
    entities: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    proof_count: Option<u64>,
    #[serde(default)]
    mentioned_at: Option<String>,
    #[serde(default)]
    metadata: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, serde::Deserialize)]
struct GraphResponse {
    #[serde(default)]
    edges: Vec<GraphEdgeEnvelope>,
    #[serde(default)]
    total_units: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct GraphEdgeEnvelope {
    data: GraphEdgeData,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEdgeData {
    source: String,
    target: String,
    #[serde(default)]
    link_type: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EntityRecord {
    id: String,
    canonical_name: String,
    #[serde(default)]
    mention_count: Option<u64>,
    #[serde(default)]
    first_seen: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct DocumentRecord {
    id: String,
    #[serde(default)]
    text_length: Option<u64>,
    #[serde(default)]
    memory_unit_count: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    retain_params: Option<RetainParams>,
    #[serde(default)]
    content_hash: Option<String>,
}
#[derive(Debug, Default, serde::Deserialize)]
struct RetainParams {
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    metadata: Option<HashMap<String, serde_json::Value>>,
}

fn parse_page<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    origin: &str,
) -> Result<Vec<T>, ImportError> {
    serde_json::from_value::<PageOf<T>>(value)
        .map(|page| page.items)
        .map_err(|error| ImportError::Decode {
            origin: origin.to_string(),
            message: format!("invalid page response: {error}"),
        })
}

#[derive(serde::Deserialize)]
struct PageOf<T> {
    #[serde(default = "empty_items")]
    items: Vec<T>,
}

fn empty_items<T>() -> Vec<T> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

/// An asynchronous [`Importer`] that publishes one Hindsight bank under the
/// `hindsight:{source_id}:` namespace.
pub struct HindsightImporter {
    config: HindsightSourceConfig,
    namespace: Namespace,
    api: Box<dyn HindsightApi>,
}

impl HindsightImporter {
    /// Build an importer that speaks HTTPS to a Hindsight API.
    pub fn new(config: HindsightSourceConfig) -> Result<Self, ImportError> {
        let api = HttpHindsightApi::new(config.base_url.clone(), config.token.clone())?;
        Self::with_api(config, Box::new(api))
    }

    /// Build an importer over an explicit API boundary. The network shell is
    /// one thin implementation; tests inject a fixture API so no code path in
    /// this crate ever requires Hindsight to be reachable.
    pub fn with_api(
        config: HindsightSourceConfig,
        api: Box<dyn HindsightApi>,
    ) -> Result<Self, ImportError> {
        config.validate()?;
        let namespace = Namespace::new("hindsight", &config.resolved_source_id())?;
        Ok(Self {
            config,
            namespace,
            api,
        })
    }

    /// The validated identity namespace (`hindsight:{source_id}:`).
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    fn origin(&self) -> String {
        format!("{}{}", self.config.base_url.trim_end_matches('/'), self.config.api_root())
    }

    async fn get(&self, path: &str) -> Result<serde_json::Value, ImportError> {
        self.api.get_json(path).await
    }

    /// Fetch the tenant's bank list and confirm the configured bank exists.
    /// A mistyped bank fails with the banks that do exist.
    async fn confirm_bank(&self) -> Result<(), ImportError> {
        let path = format!("{}/banks", self.config.api_root());
        let value = self.get(&path).await?;
        let banks: BanksResponse = serde_json::from_value(value).map_err(|error| {
            ImportError::Decode {
                origin: self.origin(),
                message: format!("invalid bank list response: {error}"),
            }
        })?;
        let available: Vec<&str> = banks.banks.iter().map(|b| b.bank_id.as_str()).collect();
        if !available.contains(&self.config.bank.as_str()) {
            return Err(ImportError::SourceRead {
                origin: self.origin(),
                message: format!(
                    "bank {:?} not found; available banks: {}",
                    self.config.bank,
                    if available.is_empty() {
                        "(none)".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            });
        }
        Ok(())
    }

    /// Page one list endpoint until exhausted or the bound is hit. Returns
    /// `(records, truncated)`; `truncated` means the bound stopped the walk
    /// while the server still had a full page more.
    async fn paginate<T: serde::de::DeserializeOwned>(
        &self,
        path_prefix: &str,
        max: usize,
    ) -> Result<(Vec<T>, bool), ImportError> {
        let mut records = Vec::new();
        let mut offset = 0usize;
        loop {
            let page_limit = PAGE_SIZE.min(max.saturating_sub(records.len()).max(1));
            let separator = if path_prefix.contains('?') { '&' } else { '?' };
            let path = format!("{path_prefix}{separator}limit={page_limit}&offset={offset}");
            let value = self.get(&path).await?;
            let page: Vec<T> = parse_page(value, &self.origin())?;
            let page_len = page.len();
            records.extend(page);
            if records.len() >= max {
                return Ok((records, page_len == page_limit));
            }
            if page_len < page_limit {
                return Ok((records, false));
            }
            offset += page_len;
        }
    }


    /// One complete import pass. Every stage failure is an [`ImportError`];
    /// the host keeps the previous snapshot live.
    async fn import_bank(&self) -> Result<LoadResult, ImportError> {
        self.confirm_bank().await?;

        // Memory units: only currently-valid facts. Non-valid states are
        // retired knowledge (invalidated or superseded units); count them so
        // the drop is observable, never silent.
        let units_path = format!("{}/banks/{}/memories/list?state=valid", self.config.api_root(), self.config.bank);
        let (mut units, units_truncated) = self
            .paginate::<serde_json::Value>(&units_path, self.config.max_units)
            .await?;
        if units_truncated {
            return Err(ImportError::SourceRead {
                origin: self.origin(),
                message: format!(
                    "bank {:?} exceeds the {} unit bound; raise max_units",
                    self.config.bank, self.config.max_units
                ),
            });
        }
        let raw_count = units.len();
        let units: Vec<MemoryUnit> = units
            .drain(..)
            .map(|value| {
                serde_json::from_value(value).map_err(|error| ImportError::Decode {
                    origin: self.origin(),
                    message: format!("invalid memory unit: {error}"),
                })
            })
            .collect::<Result<_, _>>()?;
        let units: Vec<MemoryUnit> = units
            .into_iter()
            .filter(|unit| {
                unit.state
                    .as_deref()
                    .unwrap_or("valid")
                    .eq_ignore_ascii_case("valid")
            })
            .collect();
        let dropped = raw_count - units.len();
        if dropped > 0 {
            tracing::debug!(
                bank = %self.config.bank,
                dropped,
                "skipped non-valid memory units (retired knowledge)"
            );
        }

        // Unit↔unit graph edges. One bounded request; Hindsight reports
        // total_units so an over-bound bank is caught even when the edge set
        // alone would not reveal it.
        let graph_path = format!(
            "{}/banks/{}/graph?limit={}",
            self.config.api_root(),
            self.config.bank,
            self.config.max_units.max(1)
        );
        let graph_value = self.get(&graph_path).await?;
        let graph: GraphResponse = serde_json::from_value(graph_value).map_err(|error| {
            ImportError::Decode {
                origin: self.origin(),
                message: format!("invalid graph response: {error}"),
            }
        })?;
        if let Some(total) = graph.total_units {
            if total as usize > self.config.max_units {
                return Err(ImportError::SourceRead {
                    origin: self.origin(),
                    message: format!(
                        "bank {:?} reports {total} units, exceeding the {} unit bound; raise max_units",
                        self.config.bank, self.config.max_units
                    ),
                });
            }
        }
        let edges: Vec<GraphEdgeData> = graph.edges.into_iter().map(|e| e.data).collect();

        let entities_path = format!("{}/banks/{}/entities", self.config.api_root(), self.config.bank);
        let (entities, _) = self
            .paginate::<EntityRecord>(&entities_path, self.config.max_units)
            .await?;

        let documents_path = format!("{}/banks/{}/documents", self.config.api_root(), self.config.bank);
        let (documents, _) = self
            .paginate::<DocumentRecord>(&documents_path, self.config.max_units)
            .await?;

        tracing::info!(
            bank = %self.config.bank,
            units = units.len(),
            unit_unit_edges = edges.len(),
            entities = entities.len(),
            documents = documents.len(),
            "hindsight bank fetched"
        );

        self.build_result(&units, &edges, &entities, &documents)
    }

    /// Pure projection of one fetched bank into the vault graph. Split from
    /// the async shell so the mapping is testable without network.
    fn build_result(
        &self,
        units: &[MemoryUnit],
        unit_edges: &[GraphEdgeData],
        entities: &[EntityRecord],
        documents: &[DocumentRecord],
    ) -> Result<LoadResult, ImportError> {
        let mut graph = VaultGraph::new();
        let mut search_documents = Vec::new();
        let mut unresolved: Vec<String> = Vec::new();
        let bank = self.config.bank.clone();

        // --- memory unit nodes -------------------------------------------------
        let mut unit_ids: HashSet<String> = HashSet::with_capacity(units.len());
        for unit in units {
            let node_id = self.namespace.node_id(&unit.id)?;
            let title = memory_title(&unit.text, &unit.id);
            let tags = unique_tags(&unit.tags);
            let entity_names = split_entities(unit.entities.as_deref());
            let mentioned_at = unit
                .mentioned_at
                .as_deref()
                .or(unit.date.as_deref())
                .filter(|value| !value.trim().is_empty());
            let session_id = unit
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("session_id"))
                .and_then(serde_json::Value::as_str);

            let mut frontmatter = HashMap::new();
            frontmatter.insert("bank".to_string(), serde_json::json!(bank));
            if let Some(fact_type) = unit.fact_type.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("fact_type".to_string(), serde_json::json!(fact_type));
            }
            if let Some(state) = unit.state.as_deref() {
                frontmatter.insert("state".to_string(), serde_json::json!(state));
            }
            if let Some(context) = unit.context.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("context".to_string(), serde_json::json!(context));
            }
            if let Some(document_id) = unit.document_id.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("document_id".to_string(), serde_json::json!(document_id));
            }
            if let Some(at) = mentioned_at {
                frontmatter.insert("mentioned_at".to_string(), serde_json::json!(at));
            }
            if let Some(count) = unit.proof_count {
                frontmatter.insert("proof_count".to_string(), serde_json::json!(count));
            }
            if !entity_names.is_empty() {
                frontmatter.insert("entities".to_string(), serde_json::json!(entity_names));
            }

            graph.try_add_node(VaultNode {
                id: node_id.clone(),
                meta: vault_data::NodeMeta {
                    source_id: self.namespace.source_id().to_string(),
                    title: title.clone(),
                    tags: tags.clone(),
                    frontmatter,
                    mtime: 0,
                    path: format!("memories/{}", unit.id),
                    doctype: None,
                    folder: "memories".to_string(),
                    content_type: None,
                    content_readable: false,
                    content_writable: false,
                },
                ..VaultNode::default()
            })
            .map_err(|error| ImportError::Map {
                message: format!("duplicate node: {error}"),
            })?;

            let mut document = SearchDocument::new(&node_id)
                .with("id", node_id.clone())
                .with("title", title)
                .with("tags", serde_json::json!(tags))
                .with("path", format!("memories/{}", unit.id))
                .with("type", "memory")
                .with("folder", "memories")
                .with("bank", bank.clone())
                .with("body", unit.text.trim());
            if let Some(context) = unit.context.as_deref().filter(|v| !v.trim().is_empty()) {
                document = document.with("context", context.trim());
            }
            if !entity_names.is_empty() {
                document = document.with("entities", serde_json::json!(entity_names));
            }
            if let Some(fact_type) = unit.fact_type.as_deref().filter(|v| !v.is_empty()) {
                document = document.with("fact_type", fact_type);
            }
            if let Some(state) = unit.state.as_deref().filter(|v| !v.is_empty()) {
                document = document.with("state", state);
            }
            if let Some(document_id) = unit.document_id.as_deref().filter(|v| !v.is_empty()) {
                document = document.with("document_id", document_id);
            }
            if let Some(session) = session_id.filter(|v| !v.is_empty()) {
                document = document.with("session_id", session);
            }
            if let Some(at) = mentioned_at.filter(|v| !v.trim().is_empty()) {
                document = document.with("mentioned_at", at.trim());
            }
            if let Some(count) = unit.proof_count {
                document = document.with("proof_count", count);
            }
            search_documents.push(document);
            unit_ids.insert(unit.id.clone());
        }

        // --- entity nodes ------------------------------------------------------
        // Exact-name lookup first, then a case-insensitive fallback so a unit
        // mentioning "hydra" still links to the canonical "Hydra" entity.
        let mut entity_by_name: HashMap<String, String> = HashMap::with_capacity(entities.len() * 2);
        let mut entity_by_name_ci: HashMap<String, String> =
            HashMap::with_capacity(entities.len() * 2);
        for entity in entities {
            let node_id = self.namespace.node_id(&format!("entity:{}", entity.id))?;
            let title = entity.canonical_name.trim().to_string();
            let title = if title.is_empty() {
                format!("entity {}", short_id(&entity.id))
            } else {
                title
            };
            let mut frontmatter = HashMap::new();
            frontmatter.insert("bank".to_string(), serde_json::json!(bank));
            if let Some(count) = entity.mention_count {
                frontmatter.insert("mention_count".to_string(), serde_json::json!(count));
            }
            if let Some(seen) = entity.first_seen.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("first_seen".to_string(), serde_json::json!(seen));
            }
            if let Some(seen) = entity.last_seen.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("last_seen".to_string(), serde_json::json!(seen));
            }
            graph.try_add_node(VaultNode {
                id: node_id.clone(),
                meta: vault_data::NodeMeta {
                    source_id: self.namespace.source_id().to_string(),
                    title: title.clone(),
                    tags: Vec::new(),
                    frontmatter,
                    mtime: 0,
                    path: format!("entities/{}", entity.id),
                    doctype: None,
                    folder: "entities".to_string(),
                    content_type: None,
                    content_readable: false,
                    content_writable: false,
                },
                ..VaultNode::default()
            })
            .map_err(|error| ImportError::Map {
                message: format!("duplicate node: {error}"),
            })?;
            let mut document = SearchDocument::new(&node_id)
                .with("id", node_id.clone())
                .with("title", title)
                .with("tags", serde_json::json!(Vec::<String>::new()))
                .with("path", format!("entities/{}", entity.id))
                .with("type", "entity")
                .with("folder", "entities")
                .with("bank", bank.clone())
                .with("body", entity.canonical_name.trim());
            if let Some(count) = entity.mention_count {
                document = document.with("mention_count", count);
            }
            search_documents.push(document);
            entity_by_name.insert(entity.canonical_name.clone(), node_id.clone());
            entity_by_name_ci
                .insert(entity.canonical_name.to_lowercase(), node_id);
        }

        // --- document nodes ----------------------------------------------------
        let mut document_ids: HashMap<String, String> = HashMap::with_capacity(documents.len());
        for record in documents {
            let node_id = self.namespace.node_id(&format!("document:{}", record.id))?;
            let session_id = record
                .retain_params
                .as_ref()
                .and_then(|params| params.metadata.as_ref())
                .and_then(|metadata| metadata.get("session_id"))
                .and_then(serde_json::Value::as_str);
            let title = session_id
                .filter(|session| !session.is_empty())
                .map(|session| format!("session {}", short_id(session)))
                .unwrap_or_else(|| format!("document {}", short_id(&record.id)));
            let tags = unique_tags(&record.tags);
            let context = record
                .retain_params
                .as_ref()
                .and_then(|params| params.context.as_deref())
                .filter(|value| !value.trim().is_empty());

            let mut frontmatter = HashMap::new();
            frontmatter.insert("bank".to_string(), serde_json::json!(bank));
            if let Some(length) = record.text_length {
                frontmatter.insert("text_length".to_string(), serde_json::json!(length));
            }
            if let Some(count) = record.memory_unit_count {
                frontmatter.insert("memory_unit_count".to_string(), serde_json::json!(count));
            }
            if let Some(session) = session_id.filter(|v| !v.is_empty()) {
                frontmatter.insert("session_id".to_string(), serde_json::json!(session));
            }
            if let Some(hash) = record.content_hash.as_deref().filter(|v| !v.is_empty()) {
                frontmatter.insert("content_hash".to_string(), serde_json::json!(hash));
            }
            if let Some(context) = context {
                frontmatter.insert("context".to_string(), serde_json::json!(context));
            }
            graph.try_add_node(VaultNode {
                id: node_id.clone(),
                meta: vault_data::NodeMeta {
                    source_id: self.namespace.source_id().to_string(),
                    title: title.clone(),
                    tags: tags.clone(),
                    frontmatter,
                    mtime: 0,
                    path: format!("documents/{}", record.id),
                    doctype: None,
                    folder: "documents".to_string(),
                    content_type: None,
                    content_readable: false,
                    content_writable: false,
                },
                ..VaultNode::default()
            })
            .map_err(|error| ImportError::Map {
                message: format!("duplicate node: {error}"),
            })?;
            let mut document = SearchDocument::new(&node_id)
                .with("id", node_id.clone())
                .with("title", title)
                .with("tags", serde_json::json!(tags))
                .with("path", format!("documents/{}", record.id))
                .with("type", "document")
                .with("folder", "documents")
                .with("bank", bank.clone());
            if let Some(context) = context {
                document = document.with("body", context.trim());
            }
            if let Some(count) = record.memory_unit_count {
                document = document.with("memory_count", count);
            }
            if let Some(session) = session_id.filter(|v| !v.is_empty()) {
                document = document.with("session_id", session);
            }
            search_documents.push(document);
            document_ids.insert(record.id.clone(), node_id);
        }

        // --- unit↔unit edges ---------------------------------------------------
        // VaultEdge is untyped; hindsight's temporal/semantic/caused_by links
        // all land on the same canvas. Self-loops (semantic self-similarity)
        // and duplicate endpoint pairs are dropped, and the shared-entity
        // linkType is represented by the bipartite mentions edges instead.
        let mut seen_edges: HashSet<(String, String)> = HashSet::new();
        for edge in unit_edges {
            if !edge
                .link_type
                .as_deref()
                .is_some_and(|kind| IMPORTED_LINK_TYPES.contains(&kind))
            {
                continue;
            }
            if edge.source == edge.target {
                continue;
            }
            let (Some(source), Some(target)) = (
                resolve_unit(&self.namespace, &unit_ids, &edge.source)?,
                resolve_unit(&self.namespace, &unit_ids, &edge.target)?,
            ) else {
                continue;
            };
            let key = if source <= target {
                (source.clone(), target.clone())
            } else {
                (target.clone(), source.clone())
            };
            if seen_edges.insert(key) {
                graph.add_edge(VaultEdge { source, target });
            }
        }

        // --- unit→entity mentions edges ----------------------------------------
        for unit in units {
            for name in split_entities(unit.entities.as_deref()) {
                let unit_node = self.namespace.node_id(&unit.id)?;
                let target = entity_by_name
                    .get(&name)
                    .cloned()
                    .or_else(|| entity_by_name_ci.get(&name.to_lowercase()).cloned());
                match target {
                    Some(entity_node) => {
                        if seen_edges.insert((unit_node.clone(), entity_node.clone())) {
                            graph.add_edge(VaultEdge {
                                source: unit_node,
                                target: entity_node,
                            });
                        }
                    }
                    None => unresolved.push(format!("entity {name:?} on unit {}", unit.id)),
                }
            }
        }

        // --- unit→document containment edges ------------------------------------
        for unit in units {
            let Some(document_id) = unit.document_id.as_deref().filter(|v| !v.is_empty()) else {
                continue;
            };
            let unit_node = self.namespace.node_id(&unit.id)?;
            match document_ids.get(document_id) {
                Some(document_node) => {
                    if seen_edges.insert((unit_node.clone(), document_node.clone())) {
                        graph.add_edge(VaultEdge {
                            source: unit_node,
                            target: document_node.clone(),
                        });
                    }
                }
                None => unresolved.push(format!("document {document_id:?} of unit {}", unit.id)),
            }
        }

        Ok(LoadResult {
            graph,
            search_documents,
            unresolved,
        })
    }
}

/// Map a raw unit id to its namespaced node id when the unit was imported.
fn resolve_unit(
    namespace: &Namespace,
    unit_ids: &HashSet<String>,
    raw: &str,
) -> Result<Option<String>, ImportError> {
    if unit_ids.contains(raw) {
        namespace.node_id(raw).map(Some)
    } else {
        Ok(None)
    }
}


impl Importer for HindsightImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let watch = self.config.watch_plan();
        let mut capabilities = self.config.capabilities(Effect::Read);
        if !matches!(watch, WatchPlan::Static) {
            capabilities.extend(self.config.capabilities(Effect::Watch));
        }
        ImporterDescriptor::new(
            format!("hindsight.{}", self.namespace.source_id()),
            format!("Hindsight ({})", self.config.bank),
            env!("CARGO_PKG_VERSION"),
            capabilities,
            hindsight_schema(),
        )
        .with_watch(watch)
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move { self.import_bank().await })
    }
}

/// The hindsight discovery schema. Facets mirror the bank's own navigators:
/// `type` (memory/entity/document), `folder`, `bank`, `fact_type`, `state`,
/// `tags`, and `entities` (the unit's extracted entity names).
fn hindsight_schema() -> ImporterSchema {
    ImporterSchema::new(
        "hindsight",
        vec![
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
            DiscoveryField::new("body", DiscoveryFieldType::Text, false)
                .searchable(1)
                .snippet(),
            DiscoveryField::new("context", DiscoveryFieldType::Text, false)
                .searchable(2)
                .snippet(),
            DiscoveryField::new("entities", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("fact_type", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("state", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("bank", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("document_id", DiscoveryFieldType::Keyword, false).searchable(1),
            DiscoveryField::new("session_id", DiscoveryFieldType::Keyword, false).searchable(1),
            DiscoveryField::new("mentioned_at", DiscoveryFieldType::Date, false),
            DiscoveryField::new("mention_count", DiscoveryFieldType::Number, false),
            DiscoveryField::new("memory_count", DiscoveryFieldType::Number, false),
            DiscoveryField::new("proof_count", DiscoveryFieldType::Number, false),
        ],
        vec![
            EdgeTypeSchema::directed(
                "temporal",
                "hindsight temporal ordering between two memory units of one document",
            ),
            EdgeTypeSchema {
                key: "semantic".into(),
                directed: false,
                description: Some(
                    "hindsight semantic similarity between two memory units".into(),
                ),
            },
            EdgeTypeSchema::directed(
                "caused_by",
                "hindsight causal link from one memory unit to another",
            ),
            EdgeTypeSchema::directed(
                "mentions",
                "memory unit mentions a canonical entity",
            ),
            EdgeTypeSchema::directed(
                "documented_in",
                "memory unit was retained from this document",
            ),
        ],
        TagHierarchySchema::slash(),
    )
}

/// Deduplicate tags, preserving order and dropping empties, so the canonical
/// node tags always satisfy the unique KeywordList contract.
fn unique_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(tags.len());
    tags.iter()
        .filter(|tag| !tag.trim().is_empty())
        .filter(|tag| seen.insert(tag.trim().to_string()))
        .map(|tag| tag.trim().to_string())
        .collect()
}

/// Split hindsight's comma-separated entity name string into trimmed names.
fn split_entities(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// First-sentence title for a memory unit: first line, truncated at a word
/// boundary with an ellipsis. Never empty.
fn memory_title(text: &str, unit_id: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        return format!("memory {}", short_id(unit_id));
    }
    if first_line.chars().count() <= TITLE_MAX_CHARS {
        return first_line.to_string();
    }
    let truncated: String = first_line.chars().take(TITLE_MAX_CHARS).collect();
    match truncated.rfind(' ') {
        Some(space) if space > TITLE_MAX_CHARS / 2 => {
            format!("{}…", &truncated[..space])
        }
        _ => format!("{truncated}…"),
    }
}

/// First 8 characters of an identifier (character-safe), for human-facing
/// titles where the full UUID adds noise.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests;
