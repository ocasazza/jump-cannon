//! In-browser GitHub vault import (CORS-only; no graph-api server needed).
//!
//! Runs on GitHub Pages (`*.github.io`) where graph-api is absent. Fetches a
//! public repository's markdown corpus over the CORS-enabled GitHub API and
//! raw endpoints, extracts wikilink edges with vault-links, and produces a
//! [`data_loader::LoadResult`] that the panel promotes onto the canvas.
//!
//! This module is gated behind `cfg(target_arch = "wasm32")` so the native
//! build (Tauri) still compiles — a no-op stub covers every public symbol.

#[cfg(target_arch = "wasm32")]
use futures::stream::{self, StreamExt};
#[cfg(target_arch = "wasm32")]
use gloo_net::http::Request;
#[cfg(target_arch = "wasm32")]
use web_sys::window;

#[cfg(target_arch = "wasm32")]
use data_loader::identity::{MAX_SOURCE_ID_BYTES, Namespace, validate_source_id};
#[cfg(target_arch = "wasm32")]
use vault_links::{extract_notes, renamespace};

/// Repository slug for the GitHub Pages default import (`ocasazza/jump-cannon`).
const PAGES_DEFAULT_REPO: &str = "ocasazza/jump-cannon";
/// Default subdirectory within the repo that holds the vault corpus.
const PAGES_DEFAULT_PATH: &str = "charts/jump-cannon/knowledge";
/// Max markdown files to process (prevents runaway fetches on huge trees).
const MAX_MARKDOWN_FILES: usize = 512;
/// Max cumulative raw bytes across all note files.
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024; // 8 MiB
/// Bounded concurrency for parallel raw-note fetches.
const FETCH_CONCURRENCY: usize = 8;

/// Configuration for one GitHub repository import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubImportSpec {
    /// `owner/repo` slug (e.g. `"ocasazza/jump-cannon"`).
    pub repo: String,
    /// Git reference — branch, tag, or SHA. Empty string means "default branch".
    pub git_ref: String,
    /// Subdirectory within the repo that contains markdown notes. Empty means
    /// the repository root.
    pub path: String,
}

impl GitHubImportSpec {
    /// Validate this spec against the same rules as [`github_importer::GitHubSourceConfig`].
    ///
    /// Mirrors the native validation: owner/repo slug charset, ref constraints,
    /// path relative-without-`..`.
    pub fn validate(&self) -> Result<(), String> {
        // Owner/repo must be exactly two segments of [A-Za-z0-9._-].
        let segments: Vec<&str> = self.repo.split('/').collect();
        if segments.len() != 2
            || segments.iter().any(|s| s.is_empty())
            || !segments
                .iter()
                .all(|s| s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
        {
            return Err(format!(
                "repo must be an `owner/repo` slug of [A-Za-z0-9._-], got {:?}",
                self.repo
            ));
        }

        // Ref: empty means "default branch" (resolved via the repo-info
        // endpoint); when set it must be a clean ref name.
        if !self.git_ref.is_empty()
            && (self.git_ref.trim().is_empty()
                || self.git_ref.chars().any(char::is_whitespace)
                || self.git_ref.contains(".."))
        {
            return Err(format!(
                "ref must be empty (default branch) or a ref name without whitespace or `..`, got {:?}",
                self.git_ref
            ));
        }

        // Path: empty means the repository root; when set it must be a
        // relative subdirectory with no `..` or backslashes.
        if !self.path.is_empty()
            && (self.path.contains('\\')
                || self.path.contains("..")
                || self
                    .path
                    .split('/')
                    .any(|seg| seg.is_empty() || seg == "."))
        {
            return Err(format!(
                "path must be empty (repo root) or a relative subdirectory without `..` or backslashes, got {:?}",
                self.path
            ));
        }

        Ok(())
    }
}

// ── sanitize_source_id (port of github-importer's implementation) ────────────
//
// The github-importer crate uses reqwest (native-only) so we cannot share it
// across the wasm/native boundary. The logic is ~15 lines and mirrors the
// upstream implementation exactly; see crates/github-importer/src/lib.rs.
#[cfg(target_arch = "wasm32")]
fn sanitize_source_id(repo: &str) -> String {
    let sanitized: String = repo
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || matches!(ch, '.' | '_' | '-')
            {
                ch
            } else {
                '-'
            }
        })
        .take(MAX_SOURCE_ID_BYTES)
        .collect();
    if sanitized.is_empty() {
        "github".to_string()
    } else {
        sanitized
    }
}

// ── GitHub API types (minimized for the endpoints we use) ─────────────────────

#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize, Debug)]
struct RepoInfo {
    default_branch: String,
}

#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize, Debug)]
struct GitTreeResponse {
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize, Debug)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
}

// ── Public async import ──────────────────────────────────────────────────────

/// Fetch a public GitHub repository's markdown corpus, extract wikilink edges,
/// and return a [`data_loader::LoadResult`] ready for canvas promotion.
///
/// Three-step fetch:
/// 1. `GET /repos/{owner}/{repo}` → resolve `default_branch` when `git_ref` is empty.
/// 2. `GET /repos/{owner}/{repo}/git/trees/{ref}?recursive=1` → enumerate markdown paths.
/// 3. Per-note `GET /{owner}/{repo}/raw/{ref}/{path}` → fetch text in bounded parallelism.
///
/// Then `vault_links::extract_notes` → `renamespace` into
/// `github:{source_id}:` namespace.
///
/// Errors are returned as `String` messages suitable for display in the panel.
#[cfg(target_arch = "wasm32")]
pub async fn import(spec: &GitHubImportSpec) -> Result<data_loader::LoadResult, String> {
    spec.validate()?;

    // Step 1: resolve the git ref.
    let git_ref = if spec.git_ref.is_empty() {
        resolve_default_branch(&spec.repo).await?
    } else {
        spec.git_ref.clone()
    };

    // Step 2: fetch the recursive tree and collect markdown paths.
    let tree = fetch_git_tree(&spec.repo, &git_ref).await?;

    // Filter to markdown files under the configured path prefix and strip
    // that prefix: `extract_notes` receives VAULT-relative paths, so the
    // local id parts stay byte-identical to a mounted-vault or tarball
    // import of the same corpus (the identity contract in
    // crates/github-importer). Sorted for a deterministic extraction order
    // (node insertion order feeds the canvas's deterministic layout seed).
    let prefix = if spec.path.is_empty() {
        String::new()
    } else {
        format!("{}/", spec.path)
    };
    let mut markdown_paths: Vec<String> = tree
        .iter()
        .filter(|e| e.entry_type == "blob" && e.path.ends_with(".md"))
        .filter(|e| e.path.starts_with(&prefix))
        .map(|e| e.path[prefix.len()..].to_string())
        .collect();
    markdown_paths.sort();

    if markdown_paths.is_empty() {
        return Err(format!(
            "no markdown files found under `{}` in {}@{}",
            spec.path, spec.repo, git_ref
        ));
    }

    if markdown_paths.len() > MAX_MARKDOWN_FILES {
        return Err(format!(
            "repository contains {} markdown files under `{}` (max {}); \
             narrow the path",
            markdown_paths.len(),
            spec.path,
            MAX_MARKDOWN_FILES
        ));
    }

    // Step 3: fetch note contents in bounded parallelism. `raw_path` is the
    // full repo path (prefix + vault-relative name); the stored key stays
    // vault-relative. `buffer_unordered` returns out of order — re-sort so
    // the extraction's insertion order (and thus the canvas layout seed) is
    // deterministic across loads.
    let mut files: Vec<(String, String)> = stream::iter(markdown_paths)
        .map(|rel| {
            let repo = spec.repo.clone();
            let git_ref = git_ref.clone();
            let raw_path = format!("{prefix}{rel}");
            async move { fetch_raw_note(&repo, &git_ref, &raw_path).await.map(|text| (rel, text)) }
        })
        .buffer_unordered(FETCH_CONCURRENCY)
        .collect::<Vec<Result<(String, String), String>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, String>>()?; // propagate first error
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Cumulative byte cap.
    let total_bytes: usize = files.iter().map(|(_, text)| text.len()).sum();
    if total_bytes > MAX_TOTAL_BYTES {
        return Err(format!(
            "note corpus is {} bytes (max {}); try a narrower path",
            total_bytes, MAX_TOTAL_BYTES
        ));
    }

    // Step 4: extract wikilink edges and renamespace into github: namespace.
    let extraction = extract_notes(&files);
    let source_id = sanitize_source_id(&spec.repo);
    validate_source_id(&source_id).map_err(|e| format!("source_id validation failed: {e}"))?;
    let namespace =
        Namespace::new("github", &source_id).map_err(|e| format!("namespace creation failed: {e}"))?;
    renamespace(extraction, &namespace).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn resolve_default_branch(repo: &str) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{}", repo);
    let resp = Request::get(&url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| format!("failed to fetch repo info: {e}"))?;

    if resp.status() == 404 {
        return Err(format!("repository `{}` not found (404)", repo));
    }
    if resp.status() < 200 || resp.status() >= 300 {
        return Err(format!(
            "failed to fetch repo info: HTTP {}",
            resp.status()
        ));
    }

    let info: RepoInfo = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse repo info: {e}"))?;
    Ok(info.default_branch)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_git_tree(repo: &str, git_ref: &str) -> Result<Vec<TreeEntry>, String> {
    let url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        repo, git_ref
    );
    let resp = Request::get(&url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| format!("failed to fetch git tree: {e}"))?;

    if resp.status() == 404 {
        return Err(format!(
            "ref `{}` not found in repository `{}`",
            git_ref, repo
        ));
    }
    if resp.status() < 200 || resp.status() >= 300 {
        return Err(format!(
            "failed to fetch git tree: HTTP {}",
            resp.status()
        ));
    }

    let tree_resp: GitTreeResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse git tree: {e}"))?;

    if tree_resp.truncated {
        return Err(format!(
            "git tree for {}/{}@{} was truncated — \
             the repository is too large for browser-based import; \
             use the native app with a tarball importer instead",
            repo, git_ref, repo
        ));
    }

    Ok(tree_resp.tree)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_raw_note(repo: &str, git_ref: &str, path: &str) -> Result<String, String> {
    // Percent-encode each path segment (`Start Here.md` → `Start%20Here.md`);
    // the `/` separators stay literal.
    let encoded: Vec<String> = path
        .split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect();
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        repo,
        git_ref,
        encoded.join("/")
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch note {}: {e}", path))?;

    if resp.status() < 200 || resp.status() >= 300 {
        return Err(format!(
            "failed to fetch note {}: HTTP {}",
            path, resp.status()
        ));
    }

    resp.text()
        .await
        .map_err(|e| format!("failed to read note {}: {e}", path))
}

// ── URL / location helpers ───────────────────────────────────────────────────

/// Parse a `GitHubImportSpec` from the current page's URL query parameters.
///
/// Recognizes `?gh=owner/repo`, `&ref=...`, `&path=...` (percent-decoded).
/// Returns `None` when no `gh` param is present.
#[cfg(target_arch = "wasm32")]
pub fn spec_from_location() -> Option<GitHubImportSpec> {
    let search = window()?.location().search().ok()?;
    let mut spec = GitHubImportSpec {
        repo: String::new(),
        git_ref: String::new(),
        path: String::new(),
    };
    let mut found = false;
    for pair in search.trim_start_matches('?').split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = js_sys::decode_uri_component(value)
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        match key {
            "gh" => {
                spec.repo = value;
                found = true;
            }
            "ref" => spec.git_ref = value,
            "path" => spec.path = value,
            _ => {}
        }
    }
    found.then_some(spec)
}

/// Default spec for the GitHub Pages deployment.
///
/// Imports `ocasazza/jump-cannon` at the default branch, path `charts/jump-cannon/knowledge`.
pub fn pages_default_spec() -> GitHubImportSpec {
    GitHubImportSpec {
        repo: PAGES_DEFAULT_REPO.to_string(),
        git_ref: String::new(),
        path: PAGES_DEFAULT_PATH.to_string(),
    }
}

/// Whether the current page is served from a GitHub Pages hostname.
#[cfg(target_arch = "wasm32")]
pub fn is_pages_host() -> bool {
    window()
        .and_then(|w| w.location().hostname().ok())
        .map(|h| h.ends_with(".github.io"))
        .unwrap_or(false)
}

/// The boot-time import decision: an explicit `?gh=` spec wins; a bare
/// GitHub Pages host imports the default corpus; anything else (a reachable
/// graph-api deployment, the Tauri shell) boots against the server as usual.
/// Evaluated once per app mount (callers stash it in a `use_hook`).
pub fn boot_spec() -> Option<GitHubImportSpec> {
    #[cfg(target_arch = "wasm32")]
    {
        spec_from_location().or_else(|| is_pages_host().then(pages_default_spec))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

// ── Native stub ───────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub async fn import(_spec: &GitHubImportSpec) -> Result<data_loader::LoadResult, String> {
    Err("GitHub import is not available in native builds".into())
}