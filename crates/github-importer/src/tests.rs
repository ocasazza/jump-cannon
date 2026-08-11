//! Tests drive the entire importer through a fixture [`TarballSource`] — no
//! test in this crate ever reaches codeload.github.com.

use std::path::Path;
use std::sync::Mutex;

use data_loader::testing::assert_import_contract;
use data_loader::{Effect, ImportFuture, ImportError, Importer, Transport, WatchPlan};

use super::*;

const START_HERE: &str = "---\ntitle: Start Here\ntags: [jump-cannon]\n---\n\nSee [[Architecture]].\n";
const ARCHITECTURE: &str = "---\ntitle: Architecture\n---\n\nBack to [[Start Here]].\n";
// SHA-256 (first 16 bytes, hex) of the fixture note bytes above.
const START_HERE_HASH: &str = "h256:721f02a602cddefbf07ea5e40b38f531";
const ARCHITECTURE_HASH: &str = "h256:ab96720cb135f40bd4427b27d4cf5878";

const FIXTURE_ETAG: &str = "\"deadbeef1234\"";

/// Build an in-memory gzipped tarball shaped like a codeload archive: every
/// entry nested under one top-level `{repo}-{ref}/` directory.
fn build_tarball(entries: &[(&str, &str)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, content) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, content.as_bytes())
            .unwrap();
    }
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap()
}

/// Build a tarball whose entry path bypasses the tar builder's own path
/// hygiene checks, simulating a hostile or corrupt archive.
fn build_tarball_with_raw_path(path: &str, content: &str) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, content.as_bytes()).unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap()
}

/// The canonical fixture corpus: two linked notes under the configured
/// subdirectory plus a README outside it that must never be imported.
fn fixture_tarball() -> Vec<u8> {
    build_tarball(&[
        (
            "corpus-main/charts/jump-cannon/knowledge/Start Here.md",
            START_HERE,
        ),
        (
            "corpus-main/charts/jump-cannon/knowledge/Architecture.md",
            ARCHITECTURE,
        ),
        ("corpus-main/README.md", "# not part of the corpus\n"),
    ])
}

/// A local, deterministic tarball source: answers `Fetched` until the caller
/// revalidates with its ETag, then answers `NotModified` — exactly the
/// codeload ETag contract.
struct FixtureTarball {
    bytes: Vec<u8>,
    etag: String,
    requests: Mutex<Vec<Option<String>>>,
}

impl FixtureTarball {
    fn new(bytes: Vec<u8>, etag: &str) -> Self {
        Self {
            bytes,
            etag: etag.to_string(),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn sent_etags(&self) -> Vec<Option<String>> {
        self.requests.lock().unwrap().clone()
    }
}

impl TarballSource for FixtureTarball {
    fn fetch<'a>(
        &'a self,
        etag: Option<&'a str>,
    ) -> ImportFuture<'a, Result<FetchOutcome, ImportError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push(etag.map(str::to_string));
            if etag == Some(self.etag.as_str()) {
                Ok(FetchOutcome::NotModified)
            } else {
                Ok(FetchOutcome::Fetched {
                    etag: Some(self.etag.clone()),
                    bytes: self.bytes.clone(),
                })
            }
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn test_config(cache_dir: &Path) -> GitHubSourceConfig {
    GitHubSourceConfig {
        source_id: "acme-corpus".into(),
        repo: "acme/corpus".into(),
        git_ref: "main".into(),
        path: "charts/jump-cannon/knowledge".into(),
        token: None,
        poll_interval_ms: 60_000,
        cache_dir: cache_dir.to_path_buf(),
        max_bytes: 1024 * 1024,
    }
}

fn fixture_importer(cache_dir: &Path, etag: &str) -> GitHubImporter {
    let source = FixtureTarball::new(fixture_tarball(), etag);
    GitHubImporter::with_source(test_config(cache_dir), Box::new(source)).unwrap()
}

#[test]
fn config_validation_rejects_unusable_sources() {
    let valid = test_config(Path::new("/tmp/unused"));
    valid.validate().unwrap();

    for source_id in ["", "UPPER", "has space", "a:b", "slash/segment"] {
        let mut config = valid.clone();
        config.source_id = source_id.into();
        assert!(config.validate().is_err(), "source_id {source_id:?}");
    }
    for repo in ["", "noslash", "a/b/c", "a b/c", "/repo", "owner/"] {
        let mut config = valid.clone();
        config.repo = repo.into();
        assert!(config.validate().is_err(), "repo {repo:?}");
    }
    for git_ref in ["", "has space", "release..bad"] {
        let mut config = valid.clone();
        config.git_ref = git_ref.into();
        assert!(config.validate().is_err(), "git_ref {git_ref:?}");
    }
    for path in ["", "/absolute", "charts/../etc", "charts\\windows"] {
        let mut config = valid.clone();
        config.path = path.into();
        assert!(config.validate().is_err(), "path {path:?}");
    }
    let mut config = valid.clone();
    config.max_bytes = 0;
    assert!(config.validate().is_err(), "zero max_bytes");
}

#[test]
fn debug_redacts_the_token() {
    let mut config = test_config(Path::new("/tmp/unused"));
    config.token = Some("ghp_supersecret".into());
    let debug = format!("{config:?}");
    assert!(!debug.contains("ghp_supersecret"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");

    let source = HttpTarballSource::new(
        "https://codeload.github.com/acme/corpus/tar.gz/main".into(),
        Some("ghp_supersecret".into()),
        1024,
    )
    .unwrap();
    let debug = format!("{source:?}");
    assert!(!debug.contains("ghp_supersecret"), "{debug}");
}

#[test]
fn sanitize_source_id_matches_the_identity_charset() {
    assert_eq!(sanitize_source_id("ocasazza/jump-cannon"), "ocasazza-jump-cannon");
    assert_eq!(sanitize_source_id("Owner/Repo.Name_2"), "owner-repo.name_2");
    assert_eq!(sanitize_source_id(""), "github");
    assert!(sanitize_source_id(&"x".repeat(500)).len() <= 128);
}

#[test]
fn descriptor_declares_http_read_watch_and_poll_plan() {
    let importer = fixture_importer(Path::new("/tmp/unused"), FIXTURE_ETAG);
    let descriptor = importer.descriptor();
    descriptor.validate().unwrap();
    let url = "https://codeload.github.com/acme/corpus/tar.gz/main";
    for effect in [Effect::Read, Effect::Watch] {
        assert!(
            descriptor
                .capabilities
                .contains(&Capability::new(effect, Transport::Http, url)),
            "missing {effect:?} capability"
        );
    }
    assert_eq!(descriptor.watch, WatchPlan::Poll { interval_ms: 60_000 });
    assert_eq!(descriptor.schema.source_kind, "github");
    assert_eq!(
        descriptor.schema.schema_version,
        data_loader::DISCOVERY_SCHEMA_VERSION
    );

    // A zero poll interval advertises a static snapshot and drops the watch
    // capability.
    let mut config = test_config(Path::new("/tmp/unused"));
    config.poll_interval_ms = 0;
    let source = FixtureTarball::new(fixture_tarball(), FIXTURE_ETAG);
    let importer = GitHubImporter::with_source(config, Box::new(source)).unwrap();
    let descriptor = importer.descriptor();
    descriptor.validate().unwrap();
    assert_eq!(descriptor.watch, WatchPlan::Static);
    assert!(
        !descriptor
            .capabilities
            .iter()
            .any(|capability| capability.effect == Effect::Watch)
    );
}

#[test]
fn extract_strips_top_level_directory_and_bounds_output() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    extract::extract_tarball(&fixture_tarball(), &dest, 1024 * 1024).unwrap();
    assert!(
        dest.join("charts/jump-cannon/knowledge/Start Here.md")
            .is_file()
    );
    assert!(
        dest.join("charts/jump-cannon/knowledge/Architecture.md")
            .is_file()
    );
    assert!(dest.join("README.md").is_file());
    // The codeload wrapper directory must not survive.
    assert!(!dest.join("corpus-main").exists());

    // Traversal entries are rejected, never written outside `dest`.
    let malicious = build_tarball_with_raw_path("corpus-main/../evil.md", "owned");
    let dest2 = temp.path().join("out2");
    std::fs::create_dir_all(&dest2).unwrap();
    assert!(extract::extract_tarball(&malicious, &dest2, 1024 * 1024).is_err());
    assert!(!temp.path().join("evil.md").exists());

    // The cumulative byte bound rejects oversized archives.
    let oversized = build_tarball(&[("corpus-main/big.md", &"x".repeat(4096))]);
    let dest3 = temp.path().join("out3");
    std::fs::create_dir_all(&dest3).unwrap();
    let error = extract::extract_tarball(&oversized, &dest3, 1024).unwrap_err();
    assert!(error.to_string().contains("byte bound"), "{error}");
}

#[tokio::test]
async fn import_maps_the_corpus_into_the_github_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let importer = fixture_importer(temp.path(), FIXTURE_ETAG);

    let result = importer.import().await.unwrap();
    let graph = &result.graph;
    assert_eq!(graph.node_count(), 2, "README.md is outside the corpus path");
    let start_here = "github:acme-corpus:Start Here";
    let architecture = "github:acme-corpus:Architecture";
    let node = graph.nodes.get(start_here).expect("namespaced node id");
    assert_eq!(node.meta.source_id, "acme-corpus");
    assert_eq!(node.meta.title, "Start Here");
    assert_eq!(node.meta.tags, vec!["jump-cannon".to_string()]);
    assert_eq!(node.meta.path, "Start Here");
    assert!(!node.meta.content_readable);
    assert!(!node.meta.content_writable);

    // Both wikilinks resolve to namespaced edge endpoints.
    assert_eq!(graph.edge_count(), 2);
    for edge in &graph.edges {
        assert!(edge.source.starts_with("github:acme-corpus:"), "{edge:?}");
        assert!(edge.target.starts_with("github:acme-corpus:"), "{edge:?}");
    }
    assert!(result.unresolved.is_empty(), "{:?}", result.unresolved);

    // Golden identity + content addressing for the fixture corpus.
    let mut documents: std::collections::BTreeMap<_, _> = result
        .search_documents
        .iter()
        .map(|document| (document.node_id.as_str(), document))
        .collect();
    let document = documents.remove(start_here).unwrap();
    assert_eq!(
        document.fields.get("content_hash").unwrap(),
        START_HERE_HASH
    );
    assert_eq!(document.fields.get("id").unwrap(), start_here);
    assert_eq!(document.fields.get("path").unwrap(), "Start Here");
    let document = documents.remove(architecture).unwrap();
    assert_eq!(
        document.fields.get("content_hash").unwrap(),
        ARCHITECTURE_HASH
    );
    assert!(documents.is_empty());
}

#[tokio::test]
async fn etag_304_reuses_the_cached_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let source = FixtureTarball::new(fixture_tarball(), FIXTURE_ETAG);
    let importer =
        GitHubImporter::with_source(test_config(temp.path()), Box::new(source)).unwrap();

    let first = importer.import().await.unwrap();
    let second = importer.import().await.unwrap();

    // The second poll sent If-None-Match and got a 304: two requests, the
    // first unconditional, the second revalidating with the fixture ETag.
    let requests = importer
        .source
        .as_ref()
        .as_any()
        .downcast_ref::<FixtureTarball>()
        .unwrap()
        .sent_etags();
    assert_eq!(
        requests,
        vec![None, Some(FIXTURE_ETAG.to_string())],
        "expected one unconditional fetch then one revalidation"
    );

    // The extraction landed under the sanitized-etag cache key and both
    // imports are byte-identical (304 reuse never re-extracts).
    assert!(
        temp.path().join("acme-corpus-deadbeef1234").is_dir(),
        "extraction cache directory"
    );
    let mut first_documents = first.search_documents;
    let mut second_documents = second.search_documents;
    first_documents.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    second_documents.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    assert_eq!(first_documents, second_documents);
}

#[tokio::test]
async fn restart_recovers_the_cache_pointer() {
    let temp = tempfile::tempdir().unwrap();
    let importer = fixture_importer(temp.path(), FIXTURE_ETAG);
    importer.import().await.unwrap();
    drop(importer);

    // A new process (fresh in-memory state) over the same cache dir must
    // revalidate with the persisted ETag and serve the cached extraction
    // without ever re-downloading.
    let source = FixtureTarball::new(fixture_tarball(), FIXTURE_ETAG);
    let importer =
        GitHubImporter::with_source(test_config(temp.path()), Box::new(source)).unwrap();
    let result = importer.import().await.unwrap();
    assert_eq!(result.graph.node_count(), 2);
    let requests = importer
        .source
        .as_ref()
        .as_any()
        .downcast_ref::<FixtureTarball>()
        .unwrap()
        .sent_etags();
    assert_eq!(requests, vec![Some(FIXTURE_ETAG.to_string())]);
}

#[tokio::test]
async fn a_new_etag_replaces_and_prunes_the_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let importer = fixture_importer(temp.path(), "v1");
    importer.import().await.unwrap();
    assert!(temp.path().join("acme-corpus-v1").is_dir());

    let source = FixtureTarball::new(fixture_tarball(), "v2");
    let importer =
        GitHubImporter::with_source(test_config(temp.path()), Box::new(source)).unwrap();
    importer.import().await.unwrap();
    assert!(temp.path().join("acme-corpus-v2").is_dir());
    assert!(
        !temp.path().join("acme-corpus-v1").exists(),
        "stale extraction pruned"
    );
}

#[tokio::test]
async fn import_satisfies_the_shared_contract() {
    let temp = tempfile::tempdir().unwrap();
    let importer = fixture_importer(temp.path(), FIXTURE_ETAG);
    assert_import_contract(&importer).await;
}
