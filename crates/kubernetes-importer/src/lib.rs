//! Kubernetes importer connector and default resource graph mapper.
//!
//! Acquisition and mapping are intentionally separate. [`KubernetesConnector`]
//! owns ambient Kubernetes client effects and produces JSON envelopes. Its
//! default path requests `PartialObjectMetadata`; fetching complete objects is
//! an explicit configuration opt-in. [`JsonDecoder`] and
//! [`KubernetesGraphMapper`] are pure and can be tested without a cluster. The
//! default mapper creates one node per resource and an owner-to-dependent edge
//! for each resolvable `ownerReference`.

use std::collections::{BTreeMap, HashMap};

use data_loader::{
    identity::Namespace, Capability, DecodedRecord, Decoder, DiscoveryField, DiscoveryFieldType,
    EdgeTypeSchema, Effect, GraphMapper, ImportError, ImportFuture, ImportPipeline, Importer,
    ImporterDescriptor, ImporterSchema, LoadResult, SearchDocument, SourceConnector, SourceRecord,
    TagHierarchySchema, Transport, WatchPlan,
};
use kube::{
    api::{Api, DynamicObject, GroupVersionKind, ListParams},
    core::ObjectMeta,
    discovery::{pinned_kind, verbs, Scope},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use vault_data::{NodeMeta, NodeMetrics, VaultEdge, VaultGraph, VaultNode};

// Kubernetes must deserialize a complete API page before the sanitized source
// byte budget can be applied. Keep every page small, including
// PartialObjectMetadata: ObjectMeta can still contain large annotations and
// managedFields that the default envelope deliberately discards afterward.
const MAX_LIST_PAGE_SIZE: usize = 50;
pub const HARD_MAX_OBJECTS: usize = 250_000;
pub const HARD_MAX_BYTES: usize = 512 * 1024 * 1024;

/// One explicitly allowlisted Kubernetes resource query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceQuery {
    #[serde(default)]
    pub group: String,
    pub version: String,
    pub kind: String,
    /// Required explicitly. Empty means all namespaces for namespaced
    /// resources, so omission is rejected rather than widening access.
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub label_selector: Option<String>,
    #[serde(default)]
    pub field_selector: Option<String>,
}

/// Configuration for one Kubernetes source instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KubernetesSourceConfig {
    /// Stable cluster/source-instance name used to namespace graph IDs.
    pub source_id: String,
    pub resources: Vec<ResourceQuery>,
    #[serde(default = "default_max_objects")]
    pub max_objects: usize,
    /// Maximum cumulative serialized source payload retained for one snapshot.
    /// This bounds the connector's batch representation before JSON decoding
    /// and graph mapping allocate their own structures.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    /// Polling is the compatibility bridge until graph-api consumes push
    /// source events. Zero disables polling and advertises a static snapshot.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Fetch complete Kubernetes objects and include them with annotations in
    /// connector records and node properties. Disabled by default, where the
    /// API request itself asks only for `PartialObjectMetadata`.
    #[serde(default)]
    pub include_object: bool,
    /// Secrets are rejected even when explicitly queried unless this is true.
    /// Deployment policy and RBAC must still grant them separately.
    #[serde(default)]
    pub allow_secrets: bool,
}

fn default_max_objects() -> usize {
    100_000
}

fn default_max_bytes() -> usize {
    128 * 1024 * 1024
}

fn default_poll_interval_ms() -> u64 {
    30_000
}

fn list_page_size(remaining: usize, include_object: bool) -> u32 {
    let _ = include_object;
    remaining
        .saturating_add(1)
        .min(MAX_LIST_PAGE_SIZE)
        .try_into()
        .expect("Kubernetes page limits fit in u32")
}

impl KubernetesSourceConfig {
    pub fn validate(&self) -> Result<(), ImportError> {
        if self.source_id.trim().is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "Kubernetes source_id must be non-empty".into(),
            });
        }
        if self.resources.is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "Kubernetes source must allowlist at least one resource".into(),
            });
        }
        if self.max_objects == 0 || self.max_objects > HARD_MAX_OBJECTS {
            return Err(ImportError::InvalidDescriptor {
                message: format!("Kubernetes max_objects must be between 1 and {HARD_MAX_OBJECTS}"),
            });
        }
        if self.max_bytes == 0 || self.max_bytes > HARD_MAX_BYTES {
            return Err(ImportError::InvalidDescriptor {
                message: format!("Kubernetes max_bytes must be between 1 and {HARD_MAX_BYTES}"),
            });
        }
        for query in &self.resources {
            if query.version.trim().is_empty() || query.kind.trim().is_empty() {
                return Err(ImportError::InvalidDescriptor {
                    message: "every Kubernetes resource needs version and kind".into(),
                });
            }
            if !self.allow_secrets && query.group.is_empty() && query.kind == "Secret" {
                return Err(ImportError::InvalidDescriptor {
                    message: "Secret imports are disabled unless allow_secrets is explicit".into(),
                });
            }
        }
        Ok(())
    }

    fn scope(&self) -> String {
        format!("kubernetes://{}", self.source_id)
    }

    /// Exact per-query capabilities requested from the host. Query shape,
    /// namespace scope, selectors, representation, and Secret opt-in are all
    /// part of the opaque scope, so widening configuration changes the grant.
    pub fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        self.resources
            .iter()
            .map(|query| {
                let scope = QueryCapabilityScope {
                    source_id: &self.source_id,
                    group: &query.group,
                    version: &query.version,
                    kind: &query.kind,
                    namespaces: &query.namespaces,
                    label_selector: query.label_selector.as_deref(),
                    field_selector: query.field_selector.as_deref(),
                    representation: if self.include_object {
                        "full_object"
                    } else {
                        "metadata"
                    },
                    allow_secrets: self.allow_secrets,
                };
                Capability::new(
                    effect,
                    Transport::Kubernetes,
                    format!(
                        "kubernetes:{}",
                        serde_json::to_string(&scope)
                            .expect("Kubernetes capability scope is serializable")
                    ),
                )
            })
            .collect()
    }

    fn watch_plan(&self) -> WatchPlan {
        if self.poll_interval_ms == 0 {
            WatchPlan::Static
        } else {
            WatchPlan::Poll {
                interval_ms: self.poll_interval_ms,
            }
        }
    }
}

#[derive(Serialize)]
struct QueryCapabilityScope<'a> {
    source_id: &'a str,
    group: &'a str,
    version: &'a str,
    kind: &'a str,
    namespaces: &'a [String],
    label_selector: Option<&'a str>,
    field_selector: Option<&'a str>,
    representation: &'a str,
    allow_secrets: bool,
}

/// Host-owned Kubernetes list connector. Credentials come only from
/// `Client::try_default` (kubeconfig or an explicitly mounted service-account
/// projection), never from an importer package.
pub struct KubernetesConnector {
    config: KubernetesSourceConfig,
}

impl KubernetesConnector {
    pub fn new(config: KubernetesSourceConfig) -> Result<Self, ImportError> {
        config.validate()?;
        Ok(Self { config })
    }

    async fn list(&self) -> Result<Vec<SourceRecord>, ImportError> {
        let client = Client::try_default()
            .await
            .map_err(|error| ImportError::SourceRead {
                origin: self.config.scope(),
                message: error.to_string(),
            })?;
        let mut records = Vec::new();
        let mut total_bytes = 0usize;

        for query in &self.config.resources {
            let gvk = GroupVersionKind::gvk(&query.group, &query.version, &query.kind);
            let (resource, capabilities) =
                pinned_kind(&client, &gvk)
                    .await
                    .map_err(|error| ImportError::SourceRead {
                        origin: format!("{}/{}/{}", query.group, query.version, query.kind),
                        message: error.to_string(),
                    })?;
            if !capabilities.supports_operation(verbs::LIST) {
                return Err(ImportError::SourceRead {
                    origin: format!("{}/{}/{}", query.group, query.version, query.kind),
                    message: "resource does not advertise list support".into(),
                });
            }

            let mut params = ListParams::default();
            if let Some(selector) = query.label_selector.as_deref() {
                params = params.labels(selector);
            }
            if let Some(selector) = query.field_selector.as_deref() {
                params = params.fields(selector);
            }

            match capabilities.scope {
                Scope::Cluster => {
                    if !query.namespaces.is_empty() {
                        return Err(ImportError::SourceRead {
                            origin: query.kind.clone(),
                            message: "cluster-scoped resources cannot select namespaces".into(),
                        });
                    }
                    let api = Api::<DynamicObject>::all_with(client.clone(), &resource);
                    append_list(
                        &self.config,
                        query,
                        api,
                        &params,
                        &mut records,
                        &mut total_bytes,
                    )
                    .await?;
                }
                Scope::Namespaced => {
                    if query.namespaces.is_empty() {
                        let api = Api::<DynamicObject>::all_with(client.clone(), &resource);
                        append_list(
                            &self.config,
                            query,
                            api,
                            &params,
                            &mut records,
                            &mut total_bytes,
                        )
                        .await?;
                    } else {
                        for namespace in &query.namespaces {
                            let api = Api::<DynamicObject>::namespaced_with(
                                client.clone(),
                                namespace,
                                &resource,
                            );
                            append_list(
                                &self.config,
                                query,
                                api,
                                &params,
                                &mut records,
                                &mut total_bytes,
                            )
                            .await?;
                        }
                    }
                }
            }
        }

        Ok(records)
    }
}

async fn append_list(
    config: &KubernetesSourceConfig,
    query: &ResourceQuery,
    api: Api<DynamicObject>,
    params: &ListParams,
    records: &mut Vec<SourceRecord>,
    total_bytes: &mut usize,
) -> Result<(), ImportError> {
    let mut continue_token: Option<String> = None;
    loop {
        let remaining = config
            .max_objects
            .checked_sub(records.len())
            .ok_or_else(|| ImportError::SourceRead {
                origin: config.scope(),
                message: format!("object limit {} exceeded", config.max_objects),
            })?;
        // Ask for one item past the remaining budget when possible. This lets
        // us reject an oversized result instead of silently truncating it.
        let mut page_params = params
            .clone()
            .limit(list_page_size(remaining, config.include_object));
        if let Some(token) = continue_token.as_deref() {
            page_params = page_params.continue_token(token);
        }

        let page = fetch_list_page(&api, &page_params, config.include_object)
            .await
            .map_err(|error| ImportError::SourceRead {
                origin: format!("{}/{}/{}", query.group, query.version, query.kind),
                message: error.to_string(),
            })?;
        if page.items.len() > remaining {
            return Err(ImportError::SourceRead {
                origin: config.scope(),
                message: format!("object limit {} exceeded", config.max_objects),
            });
        }

        let ListedPage {
            items,
            continue_token: next_token,
        } = page;
        for object in items {
            let namespace = object
                .metadata()
                .namespace
                .clone()
                .unwrap_or_else(|| "_cluster".into());
            let name = object.metadata().name.clone().unwrap_or_default();
            let origin = format!(
                "kubernetes://{}/{}/{}/{}/{namespace}/{name}",
                config.source_id, query.group, query.version, query.kind
            );
            let bytes = object
                .into_bytes(query)
                .map_err(|error| ImportError::SourceRead {
                    origin: origin.clone(),
                    message: error.to_string(),
                })?;
            let mut metadata = BTreeMap::new();
            metadata.insert("group".into(), Value::String(query.group.clone()));
            metadata.insert("version".into(), Value::String(query.version.clone()));
            metadata.insert("kind".into(), Value::String(query.kind.clone()));
            let record = SourceRecord {
                origin,
                content_type: "application/json".into(),
                bytes,
                metadata,
            };
            push_record_with_budget(config, records, total_bytes, record)?;
        }

        match next_token {
            Some(token) if continue_token.as_deref() == Some(token.as_str()) => {
                return Err(ImportError::SourceRead {
                    origin: config.scope(),
                    message: "Kubernetes list returned a repeated continue token".into(),
                });
            }
            Some(token) => continue_token = Some(token),
            None => break,
        }
    }

    Ok(())
}

fn push_record_with_budget(
    config: &KubernetesSourceConfig,
    records: &mut Vec<SourceRecord>,
    total_bytes: &mut usize,
    record: SourceRecord,
) -> Result<(), ImportError> {
    let next_total =
        total_bytes
            .checked_add(record.bytes.len())
            .ok_or_else(|| ImportError::SourceRead {
                origin: config.scope(),
                message: format!("source byte limit {} exceeded", config.max_bytes),
            })?;
    if next_total > config.max_bytes {
        return Err(ImportError::SourceRead {
            origin: config.scope(),
            message: format!("source byte limit {} exceeded", config.max_bytes),
        });
    }
    *total_bytes = next_total;
    records.push(record);
    Ok(())
}

enum ListedObject {
    Full(DynamicObject),
    Metadata(ObjectMeta),
}

impl ListedObject {
    fn metadata(&self) -> &ObjectMeta {
        match self {
            Self::Full(object) => &object.metadata,
            Self::Metadata(metadata) => metadata,
        }
    }

    fn into_bytes(self, query: &ResourceQuery) -> Result<Vec<u8>, serde_json::Error> {
        match self {
            Self::Full(object) => serde_json::to_vec(&object),
            Self::Metadata(metadata) => serialize_metadata(query, &metadata),
        }
    }
}

struct ListedPage {
    items: Vec<ListedObject>,
    continue_token: Option<String>,
}

async fn fetch_list_page(
    api: &Api<DynamicObject>,
    params: &ListParams,
    include_object: bool,
) -> Result<ListedPage, kube::Error> {
    if include_object {
        let list = api.list(params).await?;
        Ok(ListedPage {
            continue_token: list.metadata.continue_.filter(|token| !token.is_empty()),
            items: list.items.into_iter().map(ListedObject::Full).collect(),
        })
    } else {
        let list = api.list_metadata(params).await?;
        Ok(ListedPage {
            continue_token: list.metadata.continue_.filter(|token| !token.is_empty()),
            items: list
                .items
                .into_iter()
                .map(|object| ListedObject::Metadata(object.metadata))
                .collect(),
        })
    }
}

fn serialize_metadata(
    query: &ResourceQuery,
    object_metadata: &ObjectMeta,
) -> Result<Vec<u8>, serde_json::Error> {
    // Keep the default source envelope deliberately small. These are the only
    // fields the default graph mapper needs for identity, labels, and owner
    // edges. In particular, annotations, managed fields, spec, status, and
    // arbitrary CRD payloads never cross the connector boundary by default.
    let mut metadata = Map::new();
    if let Some(name) = &object_metadata.name {
        metadata.insert("name".into(), Value::String(name.clone()));
    }
    if let Some(namespace) = &object_metadata.namespace {
        metadata.insert("namespace".into(), Value::String(namespace.clone()));
    }
    if let Some(uid) = &object_metadata.uid {
        metadata.insert("uid".into(), Value::String(uid.clone()));
    }
    if let Some(labels) = &object_metadata.labels {
        metadata.insert("labels".into(), serde_json::to_value(labels)?);
    }
    if let Some(resource_version) = &object_metadata.resource_version {
        metadata.insert(
            "resourceVersion".into(),
            Value::String(resource_version.clone()),
        );
    }
    if let Some(owner_references) = &object_metadata.owner_references {
        metadata.insert(
            "ownerReferences".into(),
            serde_json::to_value(owner_references)?,
        );
    }

    let api_version = if query.group.is_empty() {
        query.version.clone()
    } else {
        format!("{}/{}", query.group, query.version)
    };
    let mut envelope = Map::new();
    envelope.insert("apiVersion".into(), Value::String(api_version));
    envelope.insert("kind".into(), Value::String(query.kind.clone()));
    envelope.insert("metadata".into(), Value::Object(metadata));
    serde_json::to_vec(&Value::Object(envelope))
}

impl SourceConnector for KubernetesConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        self.config.capabilities(effect)
    }

    fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move { self.list().await })
    }
}

/// Pure JSON decoder used for Kubernetes metadata and full-object envelopes.
pub struct JsonDecoder;

impl Decoder for JsonDecoder {
    fn decode(&self, record: SourceRecord) -> Result<DecodedRecord, ImportError> {
        let value = serde_json::from_slice(&record.bytes).map_err(|error| ImportError::Decode {
            origin: record.origin.clone(),
            message: error.to_string(),
        })?;
        Ok(DecodedRecord {
            origin: record.origin,
            value,
        })
    }
}

/// Pure default mapper for arbitrary Kubernetes resources.
pub struct KubernetesGraphMapper {
    source_id: String,
    include_object: bool,
}

impl KubernetesGraphMapper {
    pub fn new(source_id: impl Into<String>, include_object: bool) -> Self {
        Self {
            source_id: source_id.into(),
            include_object,
        }
    }
}

impl GraphMapper for KubernetesGraphMapper {
    fn map(&self, records: Vec<DecodedRecord>) -> Result<LoadResult, ImportError> {
        // Unified identity contract: `kubernetes:{source_id}:uid:{uid}` (the
        // UID is the external stable identity and stays unhashed).
        let ns = Namespace::new("kubernetes", &self.source_id)?;
        let mut graph = VaultGraph::new();
        let mut search_documents = Vec::new();
        let mut uid_to_id = HashMap::new();
        let mut owners_by_node = Vec::new();

        for record in records {
            let object = record.value.as_object().ok_or_else(|| ImportError::Map {
                message: format!("{} is not a JSON object", record.origin),
            })?;
            let api_version =
                string_field(object, "apiVersion").ok_or_else(|| ImportError::Map {
                    message: format!("{} has no string apiVersion", record.origin),
                })?;
            let kind = string_field(object, "kind").ok_or_else(|| ImportError::Map {
                message: format!("{} has no string kind", record.origin),
            })?;
            let metadata = object
                .get("metadata")
                .and_then(Value::as_object)
                .ok_or_else(|| ImportError::Map {
                    message: format!("{} has no metadata object", record.origin),
                })?;
            let name = string_field(metadata, "name").ok_or_else(|| ImportError::Map {
                message: format!("{} has no metadata.name", record.origin),
            })?;
            let namespace = string_field(metadata, "namespace").unwrap_or("_cluster");
            let uid = string_field(metadata, "uid");
            let node_id = match uid {
                Some(uid) => ns.node_id(&format!("uid:{uid}"))?,
                None => ns.node_id(&format!("{api_version}:{kind}:{namespace}:{name}"))?,
            };

            let labels = metadata
                .get("labels")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut label_values = labels
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| format!("{key}={value}")))
                .collect::<Vec<_>>();
            label_values.sort();
            label_values.dedup();
            let mut tags = vec!["kubernetes".into(), kind.to_string()];
            tags.extend(labels.iter().filter_map(|(key, value)| {
                value.as_str().map(|value| format!("label:{key}={value}"))
            }));
            tags.sort();
            tags.dedup();

            let mut properties = HashMap::new();
            properties.insert("apiVersion".into(), Value::String(api_version.into()));
            properties.insert("kind".into(), Value::String(kind.into()));
            properties.insert("namespace".into(), Value::String(namespace.into()));
            properties.insert("name".into(), Value::String(name.into()));
            properties.insert("labels".into(), Value::Object(labels));
            if let Some(uid) = uid {
                properties.insert("uid".into(), Value::String(uid.into()));
                uid_to_id.insert(uid.to_string(), node_id.clone());
            }
            if let Some(value) = metadata.get("resourceVersion") {
                properties.insert("resourceVersion".into(), value.clone());
            }
            if self.include_object {
                if let Some(value) = metadata.get("annotations") {
                    properties.insert("annotations".into(), value.clone());
                }
                properties.insert("object".into(), Value::Object(object.clone()));
            }

            let owners = metadata
                .get("ownerReferences")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|owner| owner.get("uid").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>();

            let mut search_document = SearchDocument::new(&node_id)
                .with("id", node_id.clone())
                .with("title", name)
                .with("tags", serde_json::json!(tags))
                .with("path", record.origin.clone())
                .with("type", kind)
                .with("namespace", namespace)
                .with("api_version", api_version)
                .with("labels", serde_json::json!(label_values));
            if let Some(uid) = uid {
                search_document.insert("uid", uid);
            }
            if let Some(resource_version) = metadata
                .get("resourceVersion")
                .and_then(serde_json::Value::as_str)
            {
                search_document.insert("resource_version", resource_version);
            }
            search_documents.push(search_document);

            graph
                .try_add_node(VaultNode {
                    id: node_id.clone(),
                    meta: NodeMeta {
                        source_id: self.source_id.clone(),
                        title: name.into(),
                        tags,
                        frontmatter: properties,
                        mtime: 0,
                        path: record.origin,
                        doctype: Some(kind.into()),
                        folder: namespace.into(),
                        content_type: Some("application/json".into()),
                        content_readable: false,
                        content_writable: false,
                    },
                    metrics: NodeMetrics::default(),
                    x: 0.0,
                    y: 0.0,
                })
                .map_err(|error| ImportError::Map {
                    message: error.to_string(),
                })?;
            owners_by_node.push((node_id, owners));
        }

        let mut unresolved = Vec::new();
        for (dependent_id, owner_uids) in owners_by_node {
            for owner_uid in owner_uids {
                if let Some(owner_id) = uid_to_id.get(&owner_uid) {
                    graph.add_edge(VaultEdge {
                        source: owner_id.clone(),
                        target: dependent_id.clone(),
                    });
                } else {
                    unresolved.push(format!("owner uid {owner_uid} for {dependent_id}"));
                }
            }
        }
        graph.validate().map_err(|error| ImportError::Map {
            message: error.to_string(),
        })?;

        Ok(LoadResult {
            graph,
            search_documents,
            unresolved,
        })
    }
}

fn string_field<'a>(object: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

/// Build a Kubernetes snapshot pipeline with exact per-query capability
/// declarations. The host must wrap it in `HostedImporter` with independently
/// selected grants before execution.
pub fn build_importer(config: KubernetesSourceConfig) -> Result<ImportPipeline, ImportError> {
    config.validate()?;
    let mut capabilities = config.capabilities(Effect::Read);
    let watch = config.watch_plan();
    if !matches!(watch, WatchPlan::Static) {
        capabilities.extend(config.capabilities(Effect::Watch));
    }
    let descriptor = ImporterDescriptor::new(
        format!("kubernetes.{}", config.source_id),
        format!("Kubernetes ({})", config.source_id),
        env!("CARGO_PKG_VERSION"),
        capabilities,
        kubernetes_schema(),
    )
    .with_watch(watch);
    let mapper = KubernetesGraphMapper::new(&config.source_id, config.include_object);
    let connector = KubernetesConnector::new(config)?;
    ImportPipeline::new(
        descriptor,
        Box::new(connector),
        Box::new(JsonDecoder),
        Box::new(mapper),
    )
}

fn kubernetes_schema() -> ImporterSchema {
    ImporterSchema::new(
        "kubernetes",
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                .searchable(4)
                .snippet(),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(1),
            DiscoveryField::new("type", DiscoveryFieldType::Keyword, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("namespace", DiscoveryFieldType::Keyword, true)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("api_version", DiscoveryFieldType::Keyword, true)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("labels", DiscoveryFieldType::KeywordList, true)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("uid", DiscoveryFieldType::Keyword, false).searchable(1),
            DiscoveryField::new("resource_version", DiscoveryFieldType::Keyword, false),
        ],
        vec![EdgeTypeSchema::directed(
            "owner_reference",
            "Kubernetes ownerReference from owner to dependent resource",
        )],
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["application/json"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decoded(origin: &str, value: Value) -> DecodedRecord {
        DecodedRecord {
            origin: origin.into(),
            value,
        }
    }

    fn resource_query() -> ResourceQuery {
        ResourceQuery {
            group: "apps".into(),
            version: "v1".into(),
            kind: "Deployment".into(),
            namespaces: vec!["demo".into()],
            label_selector: None,
            field_selector: None,
        }
    }

    #[test]
    fn rejects_unknown_config_fields() {
        let top_level = serde_json::from_value::<KubernetesSourceConfig>(json!({
            "source_id": "test",
            "resources": [{ "version": "v1", "kind": "Pod", "namespaces": [] }],
            "unexpected": true
        }))
        .unwrap_err();
        assert!(top_level.to_string().contains("unknown field `unexpected`"));

        let nested = serde_json::from_value::<KubernetesSourceConfig>(json!({
            "source_id": "test",
            "resources": [{
                "version": "v1",
                "kind": "Pod",
                "namespaces": [],
                "unexpected": true
            }]
        }))
        .unwrap_err();
        assert!(nested.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn requires_explicit_namespace_scope() {
        let error = serde_json::from_value::<KubernetesSourceConfig>(json!({
            "source_id": "test",
            "resources": [{ "version": "v1", "kind": "Pod" }]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("missing field `namespaces`"));
    }

    #[test]
    fn minimal_source_envelope_omits_annotations_and_object_payload() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": "web",
                "namespace": "demo",
                "uid": "deployment-uid",
                "resourceVersion": "42",
                "labels": { "app": "web" },
                "annotations": { "private.example/token": "do-not-expose" },
                "managedFields": [{ "manager": "controller" }],
                "ownerReferences": [{
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "name": "owner",
                    "uid": "owner-uid"
                }]
            },
            "spec": { "replicas": 3 },
            "status": { "readyReplicas": 3 }
        }))
        .unwrap();

        let bytes = serialize_metadata(&resource_query(), &object.metadata).unwrap();
        let envelope: Value = serde_json::from_slice(&bytes).unwrap();
        let envelope = envelope.as_object().unwrap();
        assert_eq!(envelope.len(), 3);
        assert!(!envelope.contains_key("spec"));
        assert!(!envelope.contains_key("status"));

        let metadata = envelope["metadata"].as_object().unwrap();
        assert!(!metadata.contains_key("annotations"));
        assert!(!metadata.contains_key("managedFields"));
        assert_eq!(metadata["uid"], "deployment-uid");
        assert_eq!(metadata["ownerReferences"][0]["uid"], "owner-uid");
    }

    #[test]
    fn mapper_omits_annotations_when_full_objects_are_disabled() {
        let loaded = KubernetesGraphMapper::new("test", false)
            .map(vec![decoded(
                "deployment",
                json!({
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "metadata": {
                        "name": "web",
                        "namespace": "demo",
                        "uid": "deployment-uid",
                        "annotations": { "private.example/token": "do-not-expose" }
                    },
                    "spec": { "replicas": 3 }
                }),
            )])
            .unwrap();

        let node = &loaded.graph.nodes["kubernetes:test:uid:deployment-uid"];
        assert!(!node.meta.frontmatter.contains_key("annotations"));
        assert!(!node.meta.frontmatter.contains_key("object"));

        let schema = kubernetes_schema();
        schema.validate_result(&loaded).unwrap();
        let document = &loaded.search_documents[0];
        assert!(!document.fields.contains_key("annotations"));
        assert!(!document.fields.contains_key("object"));
        assert!(!document
            .fields
            .values()
            .any(|value| value.to_string().contains("do-not-expose")));
    }

    #[test]
    fn kubernetes_schema_exposes_only_allowlisted_search_keys() {
        let schema = kubernetes_schema();
        schema.validate().unwrap();
        assert_eq!(
            schema
                .fields
                .iter()
                .map(|field| field.key.as_str())
                .collect::<Vec<_>>(),
            [
                "id",
                "title",
                "tags",
                "path",
                "type",
                "namespace",
                "api_version",
                "labels",
                "uid",
                "resource_version",
            ]
        );
        assert!(schema.field("labels").unwrap().facetable);
        assert!(!schema.fields.iter().any(|field| matches!(
            field.key.as_str(),
            "annotations" | "object" | "spec" | "status"
        )));

        let config = serde_json::from_value(json!({
            "source_id": "test",
            "resources": [{
                "group": "apps",
                "version": "v1",
                "kind": "Deployment",
                "namespaces": ["demo"]
            }]
        }))
        .unwrap();
        let importer = build_importer(config).unwrap();
        importer.descriptor().validate().unwrap();
        assert_eq!(importer.descriptor().schema, schema);
    }

    #[test]
    fn full_object_mode_does_not_leak_object_or_annotations_into_search() {
        let loaded = KubernetesGraphMapper::new("test", true)
            .map(vec![decoded(
                "deployment",
                json!({
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "metadata": {
                        "name": "web",
                        "namespace": "demo",
                        "uid": "deployment-uid",
                        "annotations": { "private.example/token": "do-not-index" }
                    },
                    "spec": { "privateValue": "do-not-index" }
                }),
            )])
            .unwrap();
        let node = &loaded.graph.nodes["kubernetes:test:uid:deployment-uid"];
        assert!(node.meta.frontmatter.contains_key("annotations"));
        assert!(node.meta.frontmatter.contains_key("object"));

        let document = &loaded.search_documents[0];
        assert!(!document.fields.contains_key("annotations"));
        assert!(!document.fields.contains_key("object"));
        assert!(!document
            .fields
            .values()
            .any(|value| value.to_string().contains("do-not-index")));
        kubernetes_schema().validate_result(&loaded).unwrap();
    }

    #[test]
    fn page_size_is_bounded_and_detects_one_past_the_budget() {
        assert_eq!(list_page_size(0, false), 1);
        assert_eq!(list_page_size(1, false), 2);
        assert_eq!(list_page_size(MAX_LIST_PAGE_SIZE - 1, false), 50);
        assert_eq!(list_page_size(MAX_LIST_PAGE_SIZE, false), 50);
        assert_eq!(list_page_size(usize::MAX, false), 50);
        assert_eq!(list_page_size(usize::MAX, true), 50);
    }

    #[test]
    fn maps_resources_and_owner_edges() {
        let mapper = KubernetesGraphMapper::new("test", false);
        let loaded = mapper
            .map(vec![
                decoded(
                    "deployment",
                    json!({
                        "apiVersion": "apps/v1",
                        "kind": "Deployment",
                        "metadata": {
                            "name": "web",
                            "namespace": "demo",
                            "uid": "owner-uid",
                            "labels": { "app": "web" }
                        }
                    }),
                ),
                decoded(
                    "replicaset",
                    json!({
                        "apiVersion": "apps/v1",
                        "kind": "ReplicaSet",
                        "metadata": {
                            "name": "web-abc",
                            "namespace": "demo",
                            "uid": "child-uid",
                            "ownerReferences": [{ "uid": "owner-uid" }]
                        }
                    }),
                ),
            ])
            .unwrap();

        assert_eq!(loaded.graph.node_count(), 2);
        assert_eq!(loaded.graph.edge_count(), 1);
        assert_eq!(
            loaded.graph.edges[0],
            VaultEdge {
                source: "kubernetes:test:uid:owner-uid".into(),
                target: "kubernetes:test:uid:child-uid".into(),
            }
        );
        assert!(loaded.unresolved.is_empty());
        let owner = &loaded.graph.nodes["kubernetes:test:uid:owner-uid"];
        assert!(owner.meta.tags.contains(&"label:app=web".into()));
    }

    #[test]
    fn reports_unselected_owners_without_dangling_edges() {
        let mapper = KubernetesGraphMapper::new("test", false);
        let loaded = mapper
            .map(vec![decoded(
                "pod",
                json!({
                    "apiVersion": "v1",
                    "kind": "Pod",
                    "metadata": {
                        "name": "web-abc",
                        "namespace": "demo",
                        "uid": "pod-uid",
                        "ownerReferences": [{ "uid": "missing-owner" }]
                    }
                }),
            )])
            .unwrap();

        assert_eq!(loaded.graph.edge_count(), 0);
        assert_eq!(loaded.unresolved.len(), 1);
        assert!(loaded.unresolved[0].contains("missing-owner"));
    }

    #[test]
    fn rejects_secret_queries_by_default() {
        let config = KubernetesSourceConfig {
            source_id: "test".into(),
            resources: vec![ResourceQuery {
                group: String::new(),
                version: "v1".into(),
                kind: "Secret".into(),
                namespaces: vec!["demo".into()],
                label_selector: None,
                field_selector: None,
            }],
            max_objects: 10,
            max_bytes: 1024,
            poll_interval_ms: 0,
            include_object: false,
            allow_secrets: false,
        };

        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("Secret imports are disabled"));
    }

    #[test]
    fn hard_snapshot_limits_and_byte_budget_are_enforced() {
        let mut config: KubernetesSourceConfig = serde_json::from_value(json!({
            "source_id": "test",
            "resources": [{
                "group": "apps",
                "version": "v1",
                "kind": "Deployment",
                "namespaces": ["demo"]
            }]
        }))
        .unwrap();

        config.max_objects = HARD_MAX_OBJECTS + 1;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max_objects"));
        config.max_objects = 10;
        config.max_bytes = 3;
        let mut records = Vec::new();
        let mut total_bytes = 0;
        let error = push_record_with_budget(
            &config,
            &mut records,
            &mut total_bytes,
            SourceRecord {
                origin: "fixture".into(),
                content_type: "application/json".into(),
                bytes: vec![0; 4],
                metadata: BTreeMap::new(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("byte limit 3 exceeded"));
        assert!(records.is_empty());
        assert_eq!(total_bytes, 0);
    }

    #[test]
    fn capability_scope_changes_when_query_authority_widens() {
        let mut config: KubernetesSourceConfig = serde_json::from_value(json!({
            "source_id": "test",
            "resources": [{
                "group": "apps",
                "version": "v1",
                "kind": "Deployment",
                "namespaces": ["demo"],
                "label_selector": "app=web"
            }]
        }))
        .unwrap();
        let narrow = config.capabilities(Effect::Read);

        config.resources[0].namespaces.clear();
        let cluster_wide = config.capabilities(Effect::Read);

        assert_eq!(narrow.len(), 1);
        assert_ne!(narrow, cluster_wide);
        assert!(narrow[0].scope.contains("app=web"));
        assert!(narrow[0].scope.contains("demo"));
    }

    #[test]
    fn fallback_identity_is_namespaced_and_stable() {
        let mapper = KubernetesGraphMapper::new("test", false);
        let loaded = mapper
            .map(vec![decoded(
                "service",
                json!({
                    "apiVersion": "v1",
                    "kind": "Service",
                    "metadata": { "name": "api", "namespace": "demo" }
                }),
            )])
            .unwrap();

        assert!(loaded
            .graph
            .nodes
            .contains_key("kubernetes:test:v1:Service:demo:api"));
    }

    /// The live connector needs a cluster, so the shared harness runs against
    /// the pure mapper path with fixed records — the same code that shapes
    /// every node ID and edge the connector feed produces.
    struct MapperImporter {
        records: Vec<DecodedRecord>,
    }

    impl Importer for MapperImporter {
        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new(
                "kubernetes.test",
                "Kubernetes test mapper",
                "1",
                vec![Capability::new(
                    Effect::Read,
                    Transport::Kubernetes,
                    "kubernetes:test",
                )],
                kubernetes_schema(),
            )
        }

        fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
            Box::pin(async move {
                KubernetesGraphMapper::new("test", false).map(self.records.clone())
            })
        }
    }

    #[tokio::test]
    async fn kubernetes_mapper_satisfies_the_shared_import_contract() {
        let importer = MapperImporter {
            records: vec![
                decoded(
                    "deployment",
                    json!({
                        "apiVersion": "apps/v1",
                        "kind": "Deployment",
                        "metadata": {
                            "name": "web",
                            "namespace": "demo",
                            "uid": "owner-uid",
                            "labels": { "app": "web" }
                        }
                    }),
                ),
                decoded(
                    "replicaset",
                    json!({
                        "apiVersion": "apps/v1",
                        "kind": "ReplicaSet",
                        "metadata": {
                            "name": "web-abc",
                            "namespace": "demo",
                            "uid": "child-uid",
                            "ownerReferences": [{ "uid": "owner-uid" }]
                        }
                    }),
                ),
            ],
        };
        data_loader::testing::assert_import_contract(&importer).await;
    }

    #[test]
    fn kubernetes_ids_are_golden() {
        let loaded = KubernetesGraphMapper::new("test", false)
            .map(vec![decoded(
                "deployment",
                json!({
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "metadata": {
                        "name": "web",
                        "namespace": "demo",
                        "uid": "9f8e7d6c-1234-4abc-9def-0123456789ab"
                    }
                }),
            )])
            .unwrap();

        assert!(loaded.graph.nodes.contains_key(
            "kubernetes:test:uid:9f8e7d6c-1234-4abc-9def-0123456789ab"
        ));
    }
}
