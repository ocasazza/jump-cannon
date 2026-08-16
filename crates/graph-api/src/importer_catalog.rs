//! Deployment-owned importer source-instance catalog.
//!
//! Helm still selects the deployment-default source before `graph-api` starts,
//! and the unauthenticated API exposes only the bounded, non-secret fields
//! needed to explain that deployment; there are intentionally no mutation
//! methods. When runtime switching is enabled (`JUMP_CANNON_IMPORTER_SWITCH_GROUP`),
//! the catalog additionally drives per-viewer source selection: entries marked
//! `runnable` (filesystem kinds constructible from catalog metadata alone) may
//! be served to authorized callers as read-only graph views built lazily by the
//! [`crate::source_host::SourceHost`]. Writes and compute stay default-only.

use std::collections::BTreeMap;
use std::path::Path;

use data_loader::SourceKind;
use serde::{Deserialize, Serialize};

use crate::state::SnapshotSource;

/// Maximum accepted catalog JSON. This is configuration metadata, not a data
/// transport, so a small hard limit keeps startup allocation bounded.
pub const MAX_IMPORTER_CATALOG_BYTES: usize = 64 * 1024;
/// Maximum number of named deployment source instances.
pub const MAX_IMPORTER_CATALOG_SOURCES: usize = 128;

const MAX_ID_BYTES: usize = 128;
const MAX_DNS_LABEL_BYTES: usize = 63;
const MAX_DNS_SUBDOMAIN_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_RESCAN_SECONDS: u64 = 24 * 60 * 60;

/// Canonical source kind serialized by the read-only catalog endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogSourceKind {
    Obsidian,
    Tvix,
    Generate,
    Kubernetes,
    Okf,
    Pest,
    GitHub,
    /// One bank of a Hindsight memory service, reached over its HTTP API;
    /// like the other remote kinds it is never constructible from chart
    /// catalog metadata alone (the API URL and bank live in CLI config).
    Hindsight,
    /// Session-manager shared world; never constructible from chart catalog
    /// metadata (no filesystem source), like the other non-filesystem kinds.
    World,
}

impl From<SourceKind> for CatalogSourceKind {
    fn from(value: SourceKind) -> Self {
        match value {
            SourceKind::Obsidian => Self::Obsidian,
            SourceKind::Tvix => Self::Tvix,
            SourceKind::Generate => Self::Generate,
            SourceKind::Kubernetes => Self::Kubernetes,
            SourceKind::Okf => Self::Okf,
            SourceKind::Pest => Self::Pest,
            SourceKind::GitHub => Self::GitHub,
            SourceKind::Hindsight => Self::Hindsight,
            SourceKind::World => Self::World,
        }
    }
}

/// Read-only filesystem/PVC contract for one source instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImporterFilesystemSource {
    pub volume_name: String,
    pub existing_claim: String,
    pub mount_path: String,
    pub path: String,
    pub read_only: bool,
}

/// Producer-side handoff metadata shown to operators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImporterProducerContract {
    pub chart: String,
    pub default_claim: String,
    pub repository_root: String,
    pub workflow_input: String,
    pub existing_claim_value_path: String,
    pub existing_claim_value: String,
}

/// One named source definition accepted from the trusted deployment config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImporterSourceDefinition {
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub kind: CatalogSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_rescan_interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ImporterFilesystemSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<ImporterProducerContract>,
}

impl ImporterSourceDefinition {
    /// Whether graph-api can construct this source at runtime from catalog
    /// metadata alone. v1: the filesystem kinds (Obsidian, OKF) with a
    /// filesystem source contract — everything else needs configuration the
    /// catalog deliberately does not carry (GitHub repo/token, Kubernetes
    /// allowlists, Pest packages, Nix expressions).
    pub fn runnable(&self) -> bool {
        matches!(
            self.kind,
            CatalogSourceKind::Obsidian | CatalogSourceKind::Okf
        ) && self.source.is_some()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawImporterCatalog {
    #[serde(default)]
    selected: Option<String>,
    #[serde(default)]
    sources: BTreeMap<String, ImporterSourceDefinition>,
}

/// Validated catalog retained in application state.
#[derive(Debug, Clone)]
pub struct ImporterCatalog {
    selected: Option<String>,
    active_kind: Option<CatalogSourceKind>,
    sources: Vec<(String, ImporterSourceDefinition)>,
}

impl Default for ImporterCatalog {
    fn default() -> Self {
        Self {
            selected: None,
            active_kind: None,
            sources: Vec::new(),
        }
    }
}

impl ImporterCatalog {
    /// Parse and validate the deployment-owned catalog for the source kind the
    /// process actually activated. Missing or whitespace-only JSON means an
    /// empty catalog while still recording the active kind.
    pub fn parse(raw: Option<&str>, active_kind: SourceKind) -> Result<Self, String> {
        Self::parse_with_runtime_switch(raw, active_kind, false)
    }

    /// [`Self::parse`] with the runtime-switching posture made explicit. When
    /// `runtime_switch` is enabled the selected source's kind no longer has to
    /// match the process's active kind: per-viewer selection can serve any
    /// runnable catalog entry, so the deployment default is just one of them.
    pub fn parse_with_runtime_switch(
        raw: Option<&str>,
        active_kind: SourceKind,
        runtime_switch: bool,
    ) -> Result<Self, String> {
        let active_kind = CatalogSourceKind::from(active_kind);
        let Some(raw) = raw else {
            return Ok(Self {
                active_kind: Some(active_kind),
                ..Self::default()
            });
        };

        if raw.len() > MAX_IMPORTER_CATALOG_BYTES {
            return Err(format!(
                "importer catalog is {} bytes; maximum is {MAX_IMPORTER_CATALOG_BYTES}",
                raw.len()
            ));
        }
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self {
                active_kind: Some(active_kind),
                ..Self::default()
            });
        }

        let RawImporterCatalog { selected, sources } = serde_json::from_str(raw)
            .map_err(|error| format!("invalid importer catalog JSON: {error}"))?;
        if sources.len() > MAX_IMPORTER_CATALOG_SOURCES {
            return Err(format!(
                "importer catalog has {} sources; maximum is {MAX_IMPORTER_CATALOG_SOURCES}",
                sources.len()
            ));
        }

        let selected = selected.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        });
        if let Some(selected) = &selected {
            validate_stable_id("selected source", selected)?;
        }

        for (id, source) in &sources {
            validate_source(id, source)?;
        }

        if let Some(selected) = &selected {
            let definition = sources.get(selected).ok_or_else(|| {
                format!("selected importer source {selected:?} does not exist in sources")
            })?;
            if !runtime_switch && definition.kind != active_kind {
                return Err(format!(
                    "selected importer source {selected:?} has kind {:?}, but graph-api activated {:?}",
                    definition.kind, active_kind
                ));
            }
        }

        Ok(Self {
            selected,
            active_kind: Some(active_kind),
            // BTreeMap iteration is lexicographic, making the API stable.
            sources: sources.into_iter().collect(),
        })
    }

    /// The deployment-default source id, when the catalog declares one.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Look up one named source definition. Used by the runtime source host
    /// to lazily construct alternate sources.
    pub fn source(&self, id: &str) -> Option<&ImporterSourceDefinition> {
        self.sources
            .iter()
            .find(|(source_id, _)| source_id == id)
            .map(|(_, definition)| definition)
    }

    /// Build the sanitized API representation for the currently published
    /// graph snapshot. Importer capabilities and their scopes are omitted.
    /// `runtime_switch` is computed per request by the caller: `enabled` and
    /// `required_group` are process config, `allowed` reflects the calling
    /// viewer's group membership.
    pub fn response(
        &self,
        importer: &SnapshotSource,
        runtime_switch: &RuntimeSwitchStatus,
    ) -> ImporterCatalogResponse {
        let sources = self
            .sources
            .iter()
            .map(|(id, definition)| {
                let selected = self.selected.as_deref() == Some(id.as_str());
                ImporterCatalogItem {
                    id: id.clone(),
                    display_name: definition.display_name.clone(),
                    description: definition.description.clone(),
                    kind: definition.kind,
                    source_id: definition.source_id.clone(),
                    filesystem_rescan_interval_seconds: definition
                        .filesystem_rescan_interval_seconds,
                    selected,
                    active: selected && self.active_kind == Some(definition.kind),
                    runnable: definition.runnable(),
                    source: definition.source.clone(),
                    producer: definition.producer.clone(),
                }
            })
            .collect();

        ImporterCatalogResponse {
            activation: "helm_rollout",
            selected: self.selected.clone(),
            active: ActiveImporter {
                kind: self.active_kind,
                importer: SanitizedImporterDescriptor {
                    id: importer.id.clone(),
                    name: importer.name.clone(),
                    version: importer.version.clone(),
                },
            },
            runtime_switch: runtime_switch.clone(),
            sources,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImporterCatalogResponse {
    pub activation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    pub active: ActiveImporter,
    pub runtime_switch: RuntimeSwitchStatus,
    pub sources: Vec<ImporterCatalogItem>,
}

/// Per-request runtime switching posture surfaced at `GET /importers`.
/// `required_group` serializes as `null` when switching is disabled; the UI
/// shows it in the "requires group X" affordance when `allowed` is false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSwitchStatus {
    pub enabled: bool,
    pub allowed: bool,
    pub required_group: Option<String>,
}

impl RuntimeSwitchStatus {
    /// Switching disabled: every request is served the deployment default.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed: false,
            required_group: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveImporter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<CatalogSourceKind>,
    pub importer: SanitizedImporterDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedImporterDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImporterCatalogItem {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub kind: CatalogSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_rescan_interval_seconds: Option<u64>,
    pub selected: bool,
    pub active: bool,
    /// True when graph-api can construct this source at runtime (see
    /// [`ImporterSourceDefinition::runnable`]). The UI disables selection
    /// for non-runnable sources.
    pub runnable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ImporterFilesystemSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<ImporterProducerContract>,
}

fn validate_source(id: &str, source: &ImporterSourceDefinition) -> Result<(), String> {
    validate_stable_id("source id", id)?;
    validate_nonempty("displayName", &source.display_name, MAX_LABEL_BYTES)?;
    validate_bounded("description", &source.description, MAX_DESCRIPTION_BYTES)?;
    let filesystem_kind = matches!(
        source.kind,
        CatalogSourceKind::Obsidian | CatalogSourceKind::Okf
    );
    if let Some(source_id) = &source.source_id {
        validate_stable_id("sourceId", source_id)?;
        if !matches!(
            source.kind,
            CatalogSourceKind::Okf
                | CatalogSourceKind::Kubernetes
                | CatalogSourceKind::GitHub
                | CatalogSourceKind::Hindsight
        ) {
            return Err(format!(
                "source {id:?} kind {:?} must not declare sourceId",
                source.kind
            ));
        }
    }
    if source.kind == CatalogSourceKind::Okf && source.source_id.is_none() {
        return Err(format!("OKF source {id:?} must declare sourceId"));
    }
    if !filesystem_kind && source.filesystem_rescan_interval_seconds.is_some() {
        return Err(format!(
            "source {id:?} kind {:?} must not declare filesystemRescanIntervalSeconds",
            source.kind
        ));
    }
    if source
        .filesystem_rescan_interval_seconds
        .is_some_and(|seconds| seconds > MAX_RESCAN_SECONDS)
    {
        return Err(format!(
            "source {id:?} filesystemRescanIntervalSeconds exceeds {MAX_RESCAN_SECONDS}"
        ));
    }

    if !filesystem_kind && source.source.is_some() {
        return Err(format!(
            "source {id:?} kind {:?} must not declare a filesystem source contract",
            source.kind
        ));
    }

    if source.kind == CatalogSourceKind::Okf && source.source.is_none() {
        return Err(format!(
            "OKF source {id:?} must declare a read-only filesystem source"
        ));
    }

    if let Some(filesystem) = &source.source {
        validate_dns_label("source.volumeName", &filesystem.volume_name)?;
        validate_dns_subdomain("source.existingClaim", &filesystem.existing_claim)?;
        validate_absolute_path("source.mountPath", &filesystem.mount_path)?;
        validate_absolute_path("source.path", &filesystem.path)?;
        if !Path::new(&filesystem.path).starts_with(Path::new(&filesystem.mount_path)) {
            return Err(format!("source {id:?} path must be within its mountPath"));
        }
        if source.kind == CatalogSourceKind::Okf && !filesystem.read_only {
            return Err(format!("OKF source {id:?} must be read-only"));
        }
        // Obsidian exposes content-write operations today. Until host grants
        // are scoped per named source instance, describing a read-only mount
        // would contradict the active importer's advertised write surface.
        if source.kind == CatalogSourceKind::Obsidian && filesystem.read_only {
            return Err(format!("Obsidian source {id:?} must not be read-only"));
        }
    }

    if let Some(producer) = &source.producer {
        let filesystem = source.source.as_ref().ok_or_else(|| {
            format!("source {id:?} producer metadata requires a filesystem source contract")
        })?;
        validate_nonempty("producer.chart", &producer.chart, MAX_LABEL_BYTES)?;
        validate_dns_subdomain("producer.defaultClaim", &producer.default_claim)?;
        validate_absolute_path("producer.repositoryRoot", &producer.repository_root)?;
        validate_absolute_path("producer.workflowInput", &producer.workflow_input)?;
        if !Path::new(&producer.workflow_input).starts_with(Path::new(&producer.repository_root)) {
            return Err(format!(
                "source {id:?} producer.workflowInput must be within producer.repositoryRoot"
            ));
        }
        validate_nonempty(
            "producer.existingClaimValuePath",
            &producer.existing_claim_value_path,
            MAX_LABEL_BYTES,
        )?;
        validate_dns_subdomain(
            "producer.existingClaimValue",
            &producer.existing_claim_value,
        )?;
        if producer.existing_claim_value != filesystem.existing_claim {
            return Err(format!(
                "source {id:?} producer.existingClaimValue must equal source.existingClaim"
            ));
        }
    }
    Ok(())
}

fn validate_stable_id(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value, MAX_ID_BYTES)?;
    let bytes = value.as_bytes();
    let edge_is_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let valid = edge_is_alphanumeric(bytes[0])
        && edge_is_alphanumeric(bytes[bytes.len() - 1])
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        });
    if !valid {
        return Err(format!(
            "{field} must be a stable lowercase ASCII id (letters, digits, '-', '_' or '.')"
        ));
    }
    Ok(())
}

fn validate_dns_label(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value, MAX_DNS_LABEL_BYTES)?;
    if !is_dns_label(value.as_bytes()) {
        return Err(format!(
            "{field} must be a lowercase DNS label (letters, digits or '-')"
        ));
    }
    Ok(())
}

fn validate_dns_subdomain(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value, MAX_DNS_SUBDOMAIN_BYTES)?;
    if value.split('.').any(|label| {
        label.is_empty() || label.len() > MAX_DNS_LABEL_BYTES || !is_dns_label(label.as_bytes())
    }) {
        return Err(format!(
            "{field} must be a lowercase DNS subdomain with labels of at most {MAX_DNS_LABEL_BYTES} bytes"
        ));
    }
    Ok(())
}

fn is_dns_label(bytes: &[u8]) -> bool {
    let edge_is_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && edge_is_alphanumeric(bytes[0])
        && edge_is_alphanumeric(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn validate_absolute_path(field: &str, value: &str) -> Result<(), String> {
    validate_nonempty(field, value, MAX_PATH_BYTES)?;
    if !Path::new(value).is_absolute() {
        return Err(format!("{field} must be an absolute path"));
    }
    if value.contains('\0') || value.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(format!("{field} must not contain '.' or '..' components"));
    }
    Ok(())
}

fn validate_nonempty(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    validate_bounded(field, value, max)
}

fn validate_bounded(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        return Err(format!("{field} exceeds {max} bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAVENDER: &str = r#"{
      "selected": "lavender-ingest-okf",
      "sources": {
        "z-last": {
          "displayName": "Later source",
          "kind": "obsidian"
        },
        "lavender-ingest-okf": {
          "displayName": "Lavender ingest OKF",
          "description": "Read-only shared OKF handoff",
          "kind": "okf",
          "sourceId": "lavender-ingest",
          "filesystemRescanIntervalSeconds": 60,
          "source": {
            "volumeName": "lavender-okf-repository",
            "existingClaim": "lavender-okf-shared",
            "mountPath": "/var/lib/lavender/okf-repository",
            "path": "/var/lib/lavender/okf-repository/okf",
            "readOnly": true
          },
          "producer": {
            "chart": "lavender-ingest",
            "defaultClaim": "lavender-ingest-okf",
            "repositoryRoot": "/data/okf-repository",
            "workflowInput": "/data/okf-repository/okf",
            "existingClaimValuePath": "okf.persistence.existingClaim",
            "existingClaimValue": "lavender-okf-shared"
          }
        }
      }
    }"#;

    #[test]
    fn accepts_read_only_lavender_okf_contract_and_sorts_sources() {
        let catalog = ImporterCatalog::parse(Some(LAVENDER), SourceKind::Okf).unwrap();
        let response = catalog.response(
            &SnapshotSource::new("okf", "OKF", "0.2"),
            &RuntimeSwitchStatus::disabled(),
        );

        assert_eq!(response.activation, "helm_rollout");
        assert_eq!(response.selected.as_deref(), Some("lavender-ingest-okf"));
        assert_eq!(response.active.kind, Some(CatalogSourceKind::Okf));
        assert_eq!(response.runtime_switch, RuntimeSwitchStatus::disabled());
        assert_eq!(response.sources[0].id, "lavender-ingest-okf");
        assert!(response.sources[0].selected);
        assert!(response.sources[0].active);
        assert!(response.sources[0].runnable);
        assert_eq!(response.sources[1].id, "z-last");
        // No filesystem contract: not constructible at runtime.
        assert!(!response.sources[1].runnable);
        assert_eq!(
            response.sources[0].source.as_ref().unwrap().path,
            "/var/lib/lavender/okf-repository/okf"
        );
    }

    #[test]
    fn runtime_switch_relaxes_selected_kind_match() {
        // Strict parse (switching disabled) keeps the historical rejection.
        assert!(ImporterCatalog::parse(Some(LAVENDER), SourceKind::Obsidian)
            .unwrap_err()
            .contains("activated Obsidian"));

        // With runtime switching enabled the deployment default is just one
        // runnable entry among several, so a kind mismatch is tolerated.
        let catalog =
            ImporterCatalog::parse_with_runtime_switch(Some(LAVENDER), SourceKind::Obsidian, true)
                .unwrap();
        assert_eq!(catalog.selected(), Some("lavender-ingest-okf"));
        assert!(catalog
            .source("lavender-ingest-okf")
            .expect("selected source lookup")
            .runnable());
        assert!(catalog.source("missing").is_none());

        // The selected source must still exist, even when switching.
        let missing = LAVENDER.replace(
            "\"selected\": \"lavender-ingest-okf\"",
            "\"selected\": \"missing\"",
        );
        assert!(ImporterCatalog::parse_with_runtime_switch(
            Some(&missing),
            SourceKind::Obsidian,
            true
        )
        .unwrap_err()
        .contains("does not exist"));
    }

    #[test]
    fn rejects_missing_selection_kind_mismatch_and_unknown_secret_fields() {
        let missing = LAVENDER.replace(
            "\"selected\": \"lavender-ingest-okf\"",
            "\"selected\": \"missing\"",
        );
        assert!(ImporterCatalog::parse(Some(&missing), SourceKind::Okf)
            .unwrap_err()
            .contains("does not exist"));

        assert!(ImporterCatalog::parse(Some(LAVENDER), SourceKind::Obsidian)
            .unwrap_err()
            .contains("activated Obsidian"));

        let secret = LAVENDER.replace(
            "\"readOnly\": true",
            "\"readOnly\": true, \"token\": \"do-not-expose\"",
        );
        assert!(ImporterCatalog::parse(Some(&secret), SourceKind::Okf)
            .unwrap_err()
            .contains("unknown field"));
    }

    #[test]
    fn rejects_writable_okf_and_relative_paths() {
        let writable = LAVENDER.replace("\"readOnly\": true", "\"readOnly\": false");
        assert!(ImporterCatalog::parse(Some(&writable), SourceKind::Okf)
            .unwrap_err()
            .contains("must be read-only"));

        let relative =
            LAVENDER.replace("/var/lib/lavender/okf-repository/okf", "okf-repository/okf");
        assert!(ImporterCatalog::parse(Some(&relative), SourceKind::Okf)
            .unwrap_err()
            .contains("must be an absolute path"));
    }

    #[test]
    fn rejects_missing_okf_source_id_and_lexical_path_traversal() {
        let missing_source_id = LAVENDER.replace("\"sourceId\": \"lavender-ingest\",", "");
        assert!(
            ImporterCatalog::parse(Some(&missing_source_id), SourceKind::Okf)
                .unwrap_err()
                .contains("must declare sourceId")
        );

        // Lexical starts_with would accept this because it begins at the mount
        // path; rejecting path components prevents the apparent child from
        // escaping after filesystem normalization.
        let traversal = LAVENDER.replace(
            "/var/lib/lavender/okf-repository/okf",
            "/var/lib/lavender/okf-repository/../private",
        );
        assert!(ImporterCatalog::parse(Some(&traversal), SourceKind::Okf)
            .unwrap_err()
            .contains("must not contain '.' or '..' components"));
    }

    #[test]
    fn accepts_long_dns_subdomain_claim_names_and_rejects_invalid_kubernetes_names() {
        let long_claim = format!(
            "{}.{}.{}",
            "a".repeat(MAX_DNS_LABEL_BYTES),
            "b".repeat(MAX_DNS_LABEL_BYTES),
            "c".repeat(MAX_DNS_LABEL_BYTES)
        );
        assert!(long_claim.len() > MAX_ID_BYTES);
        assert!(long_claim.len() <= MAX_DNS_SUBDOMAIN_BYTES);

        let long_claims = LAVENDER
            .replace(
                "\"existingClaim\": \"lavender-okf-shared\"",
                &format!("\"existingClaim\": \"{long_claim}\""),
            )
            .replace(
                "\"defaultClaim\": \"lavender-ingest-okf\"",
                &format!("\"defaultClaim\": \"{long_claim}\""),
            )
            .replace(
                "\"existingClaimValue\": \"lavender-okf-shared\"",
                &format!("\"existingClaimValue\": \"{long_claim}\""),
            );
        ImporterCatalog::parse(Some(&long_claims), SourceKind::Okf).unwrap();

        let invalid_claim = LAVENDER.replace("lavender-okf-shared", "lavender_okf_shared");
        assert!(
            ImporterCatalog::parse(Some(&invalid_claim), SourceKind::Okf)
                .unwrap_err()
                .contains("lowercase DNS subdomain")
        );

        let invalid_volume = LAVENDER.replace(
            "\"volumeName\": \"lavender-okf-repository\"",
            "\"volumeName\": \"lavender.okf.repository\"",
        );
        assert!(
            ImporterCatalog::parse(Some(&invalid_volume), SourceKind::Okf)
                .unwrap_err()
                .contains("lowercase DNS label")
        );
    }

    #[test]
    fn rejects_contradictory_or_unbound_producer_claim_contracts() {
        let mismatched = LAVENDER.replace(
            "\"existingClaimValue\": \"lavender-okf-shared\"",
            "\"existingClaimValue\": \"different-okf-claim\"",
        );
        assert!(ImporterCatalog::parse(Some(&mismatched), SourceKind::Okf)
            .unwrap_err()
            .contains("must equal source.existingClaim"));

        let producer_without_source = r#"{
          "sources": {
            "producer-only": {
              "displayName": "Producer only",
              "kind": "obsidian",
              "producer": {
                "chart": "example-writer",
                "defaultClaim": "example-data",
                "repositoryRoot": "/data/repository",
                "workflowInput": "/data/repository/input",
                "existingClaimValuePath": "persistence.existingClaim",
                "existingClaimValue": "example-shared"
              }
            }
          }
        }"#;
        assert!(
            ImporterCatalog::parse(Some(producer_without_source), SourceKind::Obsidian)
                .unwrap_err()
                .contains("requires a filesystem source contract")
        );
    }

    #[test]
    fn enforces_source_kind_runtime_parity() {
        let non_filesystem_source = r#"{
          "selected": "cluster",
          "sources": {
            "cluster": {
              "displayName": "Cluster",
              "kind": "kubernetes",
              "source": {
                "volumeName": "cluster-data",
                "existingClaim": "cluster-data",
                "mountPath": "/data/cluster",
                "path": "/data/cluster/input",
                "readOnly": true
              }
            }
          }
        }"#;
        assert!(
            ImporterCatalog::parse(Some(non_filesystem_source), SourceKind::Kubernetes)
                .unwrap_err()
                .contains("must not declare a filesystem source contract")
        );

        let ignored_rescan = r#"{
          "selected": "generated",
          "sources": {
            "generated": {
              "displayName": "Generated",
              "kind": "generate",
              "filesystemRescanIntervalSeconds": 30
            }
          }
        }"#;
        assert!(
            ImporterCatalog::parse(Some(ignored_rescan), SourceKind::Generate)
                .unwrap_err()
                .contains("must not declare filesystemRescanIntervalSeconds")
        );

        let read_only_obsidian = r#"{
          "selected": "local-vault",
          "sources": {
            "local-vault": {
              "displayName": "Local vault",
              "kind": "obsidian",
              "source": {
                "volumeName": "vault",
                "existingClaim": "local-vault",
                "mountPath": "/vault",
                "path": "/vault",
                "readOnly": true
              }
            }
          }
        }"#;
        assert!(
            ImporterCatalog::parse(Some(read_only_obsidian), SourceKind::Obsidian)
                .unwrap_err()
                .contains("must not be read-only")
        );

        let unused_source_id = r#"{
          "selected": "generated",
          "sources": {
            "generated": {
              "displayName": "Generated",
              "kind": "generate",
              "sourceId": "ignored"
            }
          }
        }"#;
        assert!(
            ImporterCatalog::parse(Some(unused_source_id), SourceKind::Generate)
                .unwrap_err()
                .contains("must not declare sourceId")
        );

        let kubernetes_source_id = r#"{
          "selected": "cluster",
          "sources": {
            "cluster": {
              "displayName": "Cluster",
              "kind": "kubernetes",
              "sourceId": "in-cluster"
            }
          }
        }"#;
        ImporterCatalog::parse(Some(kubernetes_source_id), SourceKind::Kubernetes).unwrap();

        // Local/raw Obsidian sources may omit a PVC contract entirely; the
        // process-level VAULT_ROOT remains authoritative for that legacy path.
        let local_obsidian = r#"{
          "selected": "local-vault",
          "sources": {
            "local-vault": {
              "displayName": "Local vault",
              "kind": "obsidian"
            }
          }
        }"#;
        ImporterCatalog::parse(Some(local_obsidian), SourceKind::Obsidian).unwrap();
    }

    #[test]
    fn rejects_oversized_catalog_before_deserialization() {
        let raw = "x".repeat(MAX_IMPORTER_CATALOG_BYTES + 1);
        assert!(ImporterCatalog::parse(Some(&raw), SourceKind::Okf)
            .unwrap_err()
            .contains("maximum"));

        let whitespace = " ".repeat(MAX_IMPORTER_CATALOG_BYTES + 1);
        assert!(ImporterCatalog::parse(Some(&whitespace), SourceKind::Okf)
            .unwrap_err()
            .contains("maximum"));
    }
}
