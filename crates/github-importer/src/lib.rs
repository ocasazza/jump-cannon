//! GitHub repository tarball importer.
//!
//! Delivers a vault corpus (for example the chart's knowledge notes) straight
//! from a GitHub repository instead of a mounted filesystem: the host polls
//! `https://codeload.github.com/{owner}/{repo}/tar.gz/{ref}` with ETag
//! revalidation (`If-None-Match`), the importer streams a bounded extraction
//! into a local cache directory, and the exact Obsidian parser
//! (`vault_links::try_extract_vault`) runs over the configured subdirectory so
//! the corpus behaves identically to obsidian mode.
//!
//! Node IDs are re-namespaced from the extractor's fixed
//! `obsidian:obsidian:{local}` form to `github:{source_id}:{local}` (see
//! [`data_loader::identity`]). The local part — the vault-relative path
//! without extension — is byte-identical between the two sources, so a note
//! keeps a stable per-path identity when a deployment moves from a mounted
//! vault to the github transport.
//!
//! # Token hygiene
//!
//! The optional access token (private repositories) is only ever attached as
//! an `Authorization: Bearer` header. It is redacted from every `Debug`
//! implementation in this crate and never appears in logs, capability scopes,
//! or error messages.
//!
//! # ETag note
//!
//! codeload answers tarball requests with a strong `ETag` validator. If
//! GitHub ever stops sending one, the importer falls back to content-keyed
//! extraction (every poll re-downloads, still bounded by `max_bytes`); it
//! deliberately does not add a GitHub API round-trip for a commit SHA — the
//! design stays single-endpoint.

mod extract;
mod fetch;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use data_loader::{
    identity::{self, Namespace},
    Capability, DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, Effect, ImportError,
    ImportFuture, Importer, ImporterDescriptor, ImporterSchema, LoadResult, TagHierarchySchema,
    Transport, WatchPlan,
};

pub use fetch::{FetchOutcome, HttpTarballSource, TarballSource};

/// Default tarball bound used by the graph-api wiring (64 MiB compressed).
pub const DEFAULT_MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024;

/// User-Agent sent with every codeload request.
const USER_AGENT: &str = "jump-cannon-github-importer";

/// Name of the pointer file inside the cache directory that names the active
/// extraction directory, and its sibling holding the raw ETag to revalidate.
const CURRENT_POINTER_FILE: &str = "current";
const CURRENT_ETAG_FILE: &str = "current-etag";

/// Configuration for one GitHub tarball source instance.
///
/// Construct directly or via graph-api's `--github-*` flags; always validate
/// through [`GitHubImporter::new`].
#[derive(Clone)]
pub struct GitHubSourceConfig {
    /// Stable source-instance identifier namespacing every node ID
    /// (`[a-z0-9._-]{1,128}` per the identity contract).
    pub source_id: String,
    /// `owner/repo` slug, e.g. `ocasazza/jump-cannon`.
    pub repo: String,
    /// Branch, tag, or commit SHA, e.g. `main`.
    pub git_ref: String,
    /// Subdirectory within the repository holding the vault corpus, e.g.
    /// `charts/jump-cannon/knowledge`.
    pub path: String,
    /// Optional bearer token for private repositories. From env only; never
    /// logged or serialized.
    pub token: Option<String>,
    /// Poll cadence advertised through [`WatchPlan::Poll`]. Zero advertises a
    /// static one-shot snapshot.
    pub poll_interval_ms: u64,
    /// Root of the extraction cache (`{cache_dir}/{repo-slug}-{key}/`).
    pub cache_dir: PathBuf,
    /// Hard bound on the compressed tarball and on the cumulative extracted
    /// bytes. A knowledge corpus is plain markdown; anything larger is an
    /// error (or an attack), not a valid source.
    pub max_bytes: u64,
}

impl fmt::Debug for GitHubSourceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubSourceConfig")
            .field("source_id", &self.source_id)
            .field("repo", &self.repo)
            .field("git_ref", &self.git_ref)
            .field("path", &self.path)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("cache_dir", &self.cache_dir)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl GitHubSourceConfig {
    /// Reject configurations that could never produce a conformant import.
    pub fn validate(&self) -> Result<(), ImportError> {
        identity::validate_source_id(&self.source_id).map_err(|message| {
            ImportError::InvalidDescriptor {
                message: format!("GitHub {message}"),
            }
        })?;
        let segments: Vec<&str> = self.repo.split('/').collect();
        let valid_repo = segments.len() == 2
            && segments.iter().all(|segment| {
                !segment.is_empty()
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'.' | b'_' | b'-')
                    })
            });
        if !valid_repo {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "GitHub repo must be an `owner/repo` slug of [A-Za-z0-9._-], got {:?}",
                    self.repo
                ),
            });
        }
        if self.git_ref.trim().is_empty()
            || self.git_ref.chars().any(char::is_whitespace)
            || self.git_ref.contains("..")
        {
            return Err(ImportError::InvalidDescriptor {
                message: format!("GitHub ref must be a non-empty ref name, got {:?}", self.git_ref),
            });
        }
        if self.path.is_empty()
            || self.path.contains('\\')
            || !Path::new(&self.path)
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "GitHub path must be a relative subdirectory without `..` or separators to escape, got {:?}",
                    self.path
                ),
            });
        }
        if self.max_bytes == 0 {
            return Err(ImportError::InvalidDescriptor {
                message: "GitHub max_bytes must be non-zero".into(),
            });
        }
        Ok(())
    }

    /// The single codeload endpoint this source polls.
    pub fn tarball_url(&self) -> String {
        format!(
            "https://codeload.github.com/{}/tar.gz/{}",
            self.repo, self.git_ref
        )
    }

    /// Exact capability tuples for `effect` (the codeload URL is the scope).
    pub fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        vec![Capability::new(
            effect,
            Transport::Http,
            self.tarball_url(),
        )]
    }

    /// How the host should learn the corpus may have changed.
    pub fn watch_plan(&self) -> WatchPlan {
        if self.poll_interval_ms == 0 {
            WatchPlan::Static
        } else {
            WatchPlan::Poll {
                interval_ms: self.poll_interval_ms,
            }
        }
    }

    /// Filesystem-safe slug for cache directory names (`owner/repo` →
    /// `owner-repo`).
    fn repo_slug(&self) -> String {
        self.repo.replace('/', "-")
    }
}

/// Derive a contract-valid `source_id` from a repository slug
/// (`ocasazza/jump-cannon` → `ocasazza-jump-cannon`). Characters outside
/// `[a-z0-9._-]` fold to `-`; an unusable slug falls back to `github`.
pub fn sanitize_source_id(repo: &str) -> String {
    let sanitized: String = repo
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
        "github".to_string()
    } else {
        sanitized
    }
}

#[derive(Clone)]
struct CacheState {
    /// Validator sent as `If-None-Match` on the next poll.
    etag: Option<String>,
    /// Active extraction directory under `cache_dir`.
    extraction: Option<PathBuf>,
}

/// An asynchronous [`Importer`] that publishes the vault corpus of one GitHub
/// repository ref under the `github:{source_id}:` namespace.
pub struct GitHubImporter {
    config: GitHubSourceConfig,
    namespace: Namespace,
    source: Box<dyn TarballSource>,
    state: Mutex<CacheState>,
}

impl GitHubImporter {
    /// Build an importer that fetches from codeload over HTTPS.
    pub fn new(config: GitHubSourceConfig) -> Result<Self, ImportError> {
        config.validate()?;
        let source = HttpTarballSource::new(
            config.tarball_url(),
            config.token.clone(),
            config.max_bytes,
        )?;
        Self::with_source(config, Box::new(source))
    }

    /// Build an importer over an explicit tarball source. The network shell
    /// is one thin implementation; tests inject a local fixture source so no
    /// code path in this crate ever requires codeload to be reachable.
    pub fn with_source(
        config: GitHubSourceConfig,
        source: Box<dyn TarballSource>,
    ) -> Result<Self, ImportError> {
        config.validate()?;
        let namespace = Namespace::new("github", &config.source_id)?;
        Ok(Self {
            config,
            namespace,
            source,
            state: Mutex::new(CacheState {
                etag: None,
                extraction: None,
            }),
        })
    }

    /// The validated identity namespace (`github:{source_id}:`).
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// Resolve the extraction directory for this poll: revalidate the cached
    /// ETag, extract fresh bytes on a 200, reuse the cached extraction on a
    /// 304. A failed fetch or extraction never disturbs the previously
    /// published extraction — the error surfaces through [`ImportError`] and
    /// graph-api keeps the old snapshot live.
    async fn resolve_extraction(&self) -> Result<PathBuf, ImportError> {
        let cached = self.cached_state();
        let outcome = self.source.fetch(cached.etag.as_deref()).await?;
        self.apply_fetch_outcome(outcome)
    }

    /// Apply one fetch outcome against the on-disk cache. Split from the
    /// async shell so the ETag/304 cache-reuse path is testable without
    /// network.
    fn apply_fetch_outcome(&self, outcome: FetchOutcome) -> Result<PathBuf, ImportError> {
        match outcome {
            FetchOutcome::NotModified => {
                let cached = self.cached_state();
                cached.extraction.ok_or_else(|| ImportError::SourceRead {
                    origin: self.config.tarball_url(),
                    message: "codeload answered 304 but no cached extraction exists".into(),
                })
            }
            FetchOutcome::Fetched { etag, bytes } => {
                let key = self.cache_key(etag.as_deref(), &bytes);
                let dir_name = format!("{}-{key}", self.config.repo_slug());
                let dest = self.config.cache_dir.join(&dir_name);
                if !dest.is_dir() {
                    self.extract_into(&bytes, &dir_name, &dest)?;
                }
                self.publish_current(&dir_name, etag.as_deref())?;
                let mut state = self.lock_state();
                state.etag = etag;
                state.extraction = Some(dest.clone());
                drop(state);
                self.prune_stale_extractions(&dir_name);
                Ok(dest)
            }
        }
    }

    /// Extract into a staging sibling and atomically rename into place so a
    /// crashed or failed extraction can never leave a half-written `dest`.
    fn extract_into(&self, bytes: &[u8], dir_name: &str, dest: &Path) -> Result<(), ImportError> {
        std::fs::create_dir_all(&self.config.cache_dir).map_err(|error| ImportError::SourceRead {
            origin: self.config.cache_dir.display().to_string(),
            message: format!("create GitHub extraction cache: {error}"),
        })?;
        let staging = self.config.cache_dir.join(format!(".staging-{dir_name}"));
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        std::fs::create_dir_all(&staging).map_err(|error| ImportError::SourceRead {
            origin: staging.display().to_string(),
            message: format!("create staging directory: {error}"),
        })?;
        let extracted = extract::extract_tarball(bytes, &staging, self.config.max_bytes);
        if let Err(error) = extracted {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        std::fs::rename(&staging, dest).map_err(|error| ImportError::SourceRead {
            origin: dest.display().to_string(),
            message: format!("publish extraction: {error}"),
        })?;
        tracing::info!(
            extraction = %dest.display(),
            bytes = bytes.len(),
            "extracted GitHub tarball into cache"
        );
        Ok(())
    }

    /// Persist the `current` pointer so a restarted process can revalidate
    /// against the previous extraction instead of re-downloading.
    fn publish_current(&self, dir_name: &str, etag: Option<&str>) -> Result<(), ImportError> {
        let pointer = self.config.cache_dir.join(CURRENT_POINTER_FILE);
        std::fs::write(&pointer, dir_name).map_err(|error| ImportError::SourceRead {
            origin: pointer.display().to_string(),
            message: format!("write cache pointer: {error}"),
        })?;
        let etag_file = self.config.cache_dir.join(CURRENT_ETAG_FILE);
        match etag {
            Some(etag) => {
                std::fs::write(&etag_file, etag).map_err(|error| ImportError::SourceRead {
                    origin: etag_file.display().to_string(),
                    message: format!("write cache ETag: {error}"),
                })?;
            }
            // No validator (codeload may stop sending ETags): remove any stale
            // one so the next poll cannot send an incorrect If-None-Match.
            None => {
                let _ = std::fs::remove_file(&etag_file);
            }
        }
        Ok(())
    }

    /// Drop extraction directories for this repository that are no longer the
    /// active one. Best-effort: a prune failure never fails an import.
    fn prune_stale_extractions(&self, active_dir_name: &str) {
        let prefix = format!("{}-", self.config.repo_slug());
        let Ok(entries) = std::fs::read_dir(&self.config.cache_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix) && name != active_dir_name {
                if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                    tracing::warn!(
                        path = %entry.path().display(),
                        "failed to prune stale GitHub extraction: {error}"
                    );
                }
            }
        }
    }

    /// Read the in-memory cache state, recovering it from the on-disk pointer
    /// on first use after a process restart.
    fn cached_state(&self) -> CacheState {
        let mut state = self.lock_state();
        if state.extraction.is_none() {
            let pointer = self.config.cache_dir.join(CURRENT_POINTER_FILE);
            let etag_file = self.config.cache_dir.join(CURRENT_ETAG_FILE);
            if let Ok(dir_name) = std::fs::read_to_string(&pointer) {
                let dir = self.config.cache_dir.join(dir_name.trim());
                if dir.is_dir() {
                    state.extraction = Some(dir);
                    state.etag = std::fs::read_to_string(&etag_file)
                        .ok()
                        .map(|etag| etag.trim().to_string())
                        .filter(|etag| !etag.is_empty());
                }
            }
        }
        state.clone()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CacheState> {
        // A poisoned mutex means a prior import panicked mid-cache-update;
        // recovering the state is always safe because every mutation above is
        // either idempotent or already persisted to disk.
        self.state.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Directory name component identifying one cached extraction: the
    /// sanitized ETag when codeload sends one, otherwise the content address
    /// of the tarball bytes (always-reimport fallback, still cache-keyed).
    fn cache_key(&self, etag: Option<&str>, bytes: &[u8]) -> String {
        if let Some(etag) = etag {
            let cleaned: String = etag
                .trim_start_matches("W/")
                .trim_matches('"')
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
                .take(64)
                .collect();
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
        self.namespace
            .content_id(bytes)
            .trim_start_matches("h256:")
            .to_string()
    }

    /// Run the Obsidian extractor over the configured subdirectory of one
    /// cached extraction and re-namespace the result into
    /// `github:{source_id}:`. The corpus is bounded by `max_bytes`, so this
    /// synchronous parse is a few milliseconds even on the largest admitted
    /// tarball.
    fn load_from_extraction(&self, extraction: &Path) -> Result<LoadResult, ImportError> {
        let root = extraction.join(&self.config.path);
        let result = vault_links::try_extract_vault(&root).map_err(|error| ImportError::SourceRead {
            origin: root.display().to_string(),
            message: format!("{error:#}"),
        })?;
        vault_links::renamespace(result, &self.namespace)
    }
}

impl Importer for GitHubImporter {
    fn descriptor(&self) -> ImporterDescriptor {
        let watch = self.config.watch_plan();
        let mut capabilities = self.config.capabilities(Effect::Read);
        if !matches!(watch, WatchPlan::Static) {
            capabilities.extend(self.config.capabilities(Effect::Watch));
        }
        ImporterDescriptor::new(
            format!("github.{}", self.config.source_id),
            format!("GitHub ({})", self.config.repo),
            env!("CARGO_PKG_VERSION"),
            capabilities,
            github_schema(),
        )
        .with_watch(watch)
    }

    fn import<'a>(&'a self) -> ImportFuture<'a, Result<LoadResult, ImportError>> {
        Box::pin(async move {
            let extraction = self.resolve_extraction().await?;
            self.load_from_extraction(&extraction)
        })
    }
}

/// The github discovery schema mirrors vault-links field-for-field so facets
/// and search behave identically to obsidian mode. Content stays
/// non-readable/non-writable through graph-api: the corpus lives in a
/// remote repository, not in the local vault root, so the document editor
/// path (`/vault/page`) does not apply.
fn github_schema() -> ImporterSchema {
    ImporterSchema::new(
        "github",
        vec![
            DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("title", DiscoveryFieldType::Text, true)
                .searchable(4)
                .snippet(),
            DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true)
                .searchable(3)
                .facetable(),
            DiscoveryField::new("path", DiscoveryFieldType::Keyword, true).searchable(2),
            DiscoveryField::new("type", DiscoveryFieldType::Keyword, false)
                .searchable(2)
                .facetable(),
            DiscoveryField::new("folder", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("body", DiscoveryFieldType::Text, true)
                .searchable(1)
                .snippet(),
            DiscoveryField::new("description", DiscoveryFieldType::Text, false)
                .searchable(2)
                .snippet(),
            DiscoveryField::new("status", DiscoveryFieldType::Keyword, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("authors", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("entities", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("key_topics", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            DiscoveryField::new("related", DiscoveryFieldType::KeywordList, false)
                .searchable(1)
                .facetable(),
            // Content address of the raw file bytes (`h256:{32hex}`).
            DiscoveryField::new("content_hash", DiscoveryFieldType::Keyword, false).facetable(),
        ],
        vec![EdgeTypeSchema::directed(
            "wikilink",
            "Obsidian wikilink from the source note to its target note",
        )],
        TagHierarchySchema::slash(),
    )
    .with_input_media_types(["text/markdown"])
}

#[cfg(test)]
mod tests;
