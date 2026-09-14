//! Per-package contract tests for the shipped json-engine importer packages
//! (`charts/jump-cannon/packages/*.toml`).
//!
//! Every package runs end-to-end against recorded API responses through the
//! real connector/decoder/mapper pipeline (the transport is the only seam
//! faked — see `build_importer_with_transport`, the documented test seam).
//! The asserted contract is the one the OpenAlex regression violated: a
//! package whose schema declares edge types must produce edges, and every
//! package must produce nodes. A projection that silently maps to zero edges
//! (like the pre-fix array-valued `/referenced_works` pointer) fails loudly
//! here instead of shipping an empty graph to users.

use std::collections::BTreeMap;
use std::path::PathBuf;

use data_loader::{ImportError, ImportFuture, ImportOutcome, Importer, NoProgress};
use importer::json::JsonTransport;
use importer::{build_importer_with_transport, InstanceConfig, ValidatedPackage};
use rstest::rstest;

/// Fake API root; fixture routes match on the exact request path below it.
const BASE_URL: &str = "http://fixture.test";

fn repo_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../").join(relative)
}

/// Serves one recorded response per exact request path. Pagination never
/// advances: every fixture page holds fewer items than the package page size,
/// which is the engine's natural stop condition.
struct FixtureTransport {
    routes: BTreeMap<String, Vec<u8>>,
}

impl FixtureTransport {
    fn new(fixture_dir: &str, routes: &[(&str, &str)]) -> Self {
        let dir = repo_file(&format!("crates/importer/tests/fixtures/packages/{fixture_dir}"));
        let routes = routes
            .iter()
            .map(|(path, file)| {
                let bytes = std::fs::read(dir.join(file))
                    .unwrap_or_else(|error| panic!("fixture {file}: {error}"));
                ((*path).to_string(), bytes)
            })
            .collect();
        Self { routes }
    }
}

impl JsonTransport for FixtureTransport {
    fn get<'a>(&'a self, url: &'a str) -> ImportFuture<'a, Result<Vec<u8>, ImportError>> {
        Box::pin(async move {
            let path = url
                .strip_prefix(BASE_URL)
                .unwrap_or(url)
                .split('?')
                .next()
                .unwrap_or_default();
            self.routes.get(path).cloned().ok_or_else(|| ImportError::SourceRead {
                origin: url.to_string(),
                message: format!("no fixture recorded for path {path}"),
            })
        })
    }
}

/// One package's fixture binding: the chart TOML, the variables an
/// administrator would supply, and the request-path -> recorded-response map.
struct PackageCase {
    manifest: &'static str,
    fixture_dir: &'static str,
    variables: &'static [(&'static str, &'static str)],
    routes: &'static [(&'static str, &'static str)],
}

#[rstest]
#[case::openalex_works(PackageCase {
    manifest: "openalex-works.toml",
    fixture_dir: "openalex-works",
    variables: &[],
    routes: &[("/works", "works.json")],
})]
#[case::chembl_pharmacology(PackageCase {
    manifest: "chembl-pharmacology.toml",
    fixture_dir: "chembl-pharmacology",
    variables: &[],
    routes: &[
        ("/chembl/api/data/molecule.json", "molecule.json"),
        ("/chembl/api/data/target.json", "target.json"),
        ("/chembl/api/data/mechanism.json", "mechanism.json"),
        ("/chembl/api/data/drug_indication.json", "drug_indication.json"),
    ],
})]
#[case::hindsight_memory_bank(PackageCase {
    manifest: "hindsight-memory-bank.toml",
    fixture_dir: "hindsight-memory-bank",
    variables: &[("bank", "fixture")],
    routes: &[
        ("/v1/default/banks", "banks.json"),
        ("/v1/default/banks/fixture/memories/list", "memories.json"),
        ("/v1/default/banks/fixture/entities", "entities.json"),
        ("/v1/default/banks/fixture/documents", "documents.json"),
        ("/v1/default/banks/fixture/graph", "graph.json"),
    ],
})]
#[tokio::test]
async fn shipped_package_produces_nodes_and_declared_edges(#[case] case: PackageCase) {
    let manifest_path = repo_file(&format!("charts/jump-cannon/packages/{}", case.manifest));
    let source = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let package = ValidatedPackage::from_toml(&source)
        .unwrap_or_else(|error| panic!("{} validates: {error}", case.manifest));
    let declares_edges = !package.schema().edge_types.is_empty();

    let variables = case
        .variables
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    let instance = InstanceConfig {
        source_id: "fixture".to_string(),
        base_url: BASE_URL.to_string(),
        variables,
        token: None,
        poll_interval_ms: 0,
    };
    let transport = FixtureTransport::new(case.fixture_dir, case.routes);
    let importer = build_importer_with_transport(package, instance, Box::new(transport))
        .unwrap_or_else(|error| panic!("{} binds: {error}", case.manifest));

    let result = match importer
        .import(&NoProgress)
        .await
        .unwrap_or_else(|error| panic!("{} imports from its fixtures: {error}", case.manifest))
    {
        ImportOutcome::Loaded(result) => result,
        ImportOutcome::Unchanged => {
            panic!("{} reported unchanged on its first import", case.manifest)
        }
    };

    assert!(
        result.graph.node_count() > 0,
        "{} produced zero nodes",
        case.manifest
    );
    if declares_edges {
        assert!(
            result.graph.edge_count() > 0,
            "{} declares edge types but produced zero edges (silent projection break)",
            case.manifest
        );
    }
}
