//! Pest engine tests: inline packages and caller-supplied inputs; the
//! filesystem binding tests use temp files, never the network.

use super::*;
use crate::FORMAT_VERSION;

const GRAMMAR: &str = r#"
document = { SOI ~ (record ~ NEWLINE?)* ~ EOI }
record = _{ node | edge }
node = { "N|" ~ node_id ~ "|" ~ title ~ "|" ~ kind ~ "|" ~ tags ~ "|" ~ properties }
node_id = @{ field }
title = @{ field }
kind = @{ field }
tags = _{ (tag ~ ("," ~ tag)*)? }
tag = @{ atom }
properties = _{ (property ~ (";" ~ property)*)? }
property = { key ~ "=" ~ value }
key = @{ atom }
value = @{ atom }
edge = { "E|" ~ source ~ "|" ~ target }
source = @{ field }
target = @{ field }
field = _{ (!("|" | NEWLINE) ~ ANY)+ }
atom = _{ (!("," | ";" | "=" | "|" | NEWLINE) ~ ANY)+ }
"#;

fn manifest(extra_limits: &str) -> String {
    format!(
        r#"format_version = 3

[metadata]
id = "example.line-graph"
name = "Line graph"
version = "1.2.3"
description = "Test line-oriented graph"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''{GRAMMAR}'''

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

[[schema.fields]]
key = "owner"
field_type = "keyword"
required = false
searchable = true
facetable = true

[[schema.fields]]
key = "zone"
field_type = "keyword"
required = false
searchable = true
facetable = true

[[schema.fields]]
key = "state"
field_type = "keyword"
required = false
searchable = true
facetable = true
{extra_limits}
"#
    )
}

fn package() -> ValidatedPackage {
    ValidatedPackage::from_toml(&manifest("")).expect("valid test package")
}

#[test]
fn validates_inline_runtime_grammar_and_metadata() {
    let package = package();
    assert_eq!(package.manifest().format_version, FORMAT_VERSION);
    assert_eq!(package.manifest().metadata.id, "example.line-graph");
    assert_eq!(package.manifest().metadata.version, "1.2.3");
    assert_eq!(package.engine(), crate::EngineKind::Pest);
    assert_eq!(package.pest_config().unwrap().root_rule, "document");
}

#[test]
fn rejects_unknown_format_and_bad_grammar() {
    let unknown = manifest("").replacen("format_version = 3", "format_version = 4", 1);
    assert!(matches!(
        ValidatedPackage::from_toml(&unknown),
        Err(ImportError::UnsupportedFormatVersion {
            found: 4,
            supported: FORMAT_VERSION
        })
    ));

    let bad = manifest("").replace("target = @{ field }", "target = @{ missing_rule }");
    assert!(matches!(
        ValidatedPackage::from_toml(&bad),
        Err(ImportError::Grammar(_))
    ));
}

#[test]
fn json_engine_config_keys_are_rejected_from_pest_packages() {
    // deny_unknown_fields on the pest [parser] table: a json key in a pest
    // package is a hard error, not a silent ignore.
    let mixed = manifest("").replace(
        "root_rule = \"document\"",
        "root_rule = \"document\"\npage_size = 500",
    );
    let error = ValidatedPackage::from_toml(&mixed).expect_err("json key must fail");
    assert!(matches!(error, ImportError::Toml(_)), "got {error:?}");
    assert!(error.to_string().contains("page_size"), "{error}");
}

#[test]
fn maps_good_graph_and_metadata_deterministically() {
    let input = concat!(
        "N|n1|Alpha|service|red,prod|owner=platform;zone=west\n",
        "N|n2|Beta|database||state=ready\n",
        "E|n1|n2"
    );
    let result = package().parse_input(input).expect("input parses");

    assert!(result.unresolved.is_empty());
    assert_eq!(result.graph.node_count(), 2);
    assert_eq!(result.graph.edge_count(), 1);
    assert_eq!(
        result
            .graph
            .nodes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["pest:example.line-graph:n1", "pest:example.line-graph:n2"]
    );

    let n1 = result.graph.nodes.get("pest:example.line-graph:n1").expect("n1");
    assert_eq!(n1.meta.title, "Alpha");
    assert_eq!(n1.meta.doctype.as_deref(), Some("service"));
    assert_eq!(n1.meta.tags, vec!["prod".to_owned(), "red".to_owned()]);
    assert_eq!(n1.meta.path, "n1");
    assert_eq!(
        n1.meta.frontmatter["owner"],
        Value::String("platform".into())
    );
    assert_eq!(n1.meta.frontmatter["zone"], Value::String("west".into()));

    let edge = &result.graph.edges[0];
    assert_eq!(edge.source, "pest:example.line-graph:n1");
    assert_eq!(edge.target, "pest:example.line-graph:n2");

    package()
        .schema()
        .validate_result(&result)
        .expect("output satisfies the package schema");
}

#[test]
fn undeclared_properties_remain_metadata_but_never_enter_search_documents() {
    let package = package();
    let result = package
        .parse_input("N|n1|Alpha|service|prod|owner=platform;private=secret")
        .unwrap();
    let node = &result.graph.nodes["pest:example.line-graph:n1"];
    let document = &result.search_documents[0];

    assert_eq!(node.meta.frontmatter["private"], "secret");
    assert_eq!(document.fields["owner"], "platform");
    assert!(!document.fields.contains_key("private"));
    assert!(!document
        .fields
        .values()
        .any(|value| value.as_str() == Some("secret")));
    package.schema().validate_result(&result).unwrap();
}

#[test]
fn duplicate_node_ids_are_rejected() {
    let error = package()
        .parse_input("N|same|One|kind||\nN|same|Two|kind||")
        .expect_err("duplicate must fail");
    assert!(matches!(error, ImportError::DuplicateNodeId(id) if id == "same"));
}

#[test]
fn dangling_edges_are_unresolved_and_not_added() {
    let result = package()
        .parse_input("N|n1|One|kind||\nE|n1|missing")
        .expect("dangling edges do not fail the load");
    assert_eq!(result.graph.node_count(), 1);
    assert_eq!(result.graph.edge_count(), 0);
    assert_eq!(
        result.unresolved,
        ["edge 'pest:example.line-graph:n1' -> 'pest:example.line-graph:missing' has missing target"]
    );
}

#[test]
fn malformed_input_is_rejected() {
    let error = package()
        .parse_input("this is not a graph record")
        .expect_err("malformed input must fail");
    assert!(matches!(error, ImportError::Parse { .. }));
}

#[test]
fn input_node_and_edge_limits_are_enforced() {
    let limited = ValidatedPackage::from_toml(&manifest(
        "\n[limits]\ninput_bytes = 80\nnodes = 1\nedges = 1\n",
    ))
    .expect("limited package validates");

    assert!(matches!(
        limited.parse_input("N|n1|One|kind||\nN|n2|Two|kind||"),
        Err(ImportError::RecordLimit {
            kind: "nodes",
            actual: 2,
            max: 1
        })
    ));
    assert!(matches!(
        limited.parse_input("N|n1|One|kind||\nE|n1|n1\nE|n1|n1"),
        Err(ImportError::RecordLimit {
            kind: "edges",
            actual: 2,
            max: 1
        })
    ));
    assert!(matches!(
        limited.parse_input(&"x".repeat(81)),
        Err(ImportError::InputTooLarge {
            actual: 81,
            max: 80
        })
    ));
}

#[test]
fn manifest_and_grammar_limits_are_enforced() {
    let oversized = vec![b'x'; crate::HARD_LIMITS.manifest_bytes + 1];
    assert!(matches!(
        ValidatedPackage::from_toml_bytes(&oversized),
        Err(ImportError::ManifestTooLarge { .. })
    ));

    let grammar_limited = manifest("\n[limits]\ngrammar_bytes = 16\n");
    assert!(matches!(
        ValidatedPackage::from_toml(&grammar_limited),
        Err(ImportError::GrammarTooLarge { max: 16, .. })
    ));
}

#[test]
fn pest_ids_are_golden() {
    let result = package()
        .parse_input("N|n1|Alpha|service|prod|owner=platform\nE|n1|n1")
        .expect("input parses");

    // Node IDs are exactly `pest:{package id}:{capture id}`.
    let node = result
        .graph
        .nodes
        .get("pest:example.line-graph:n1")
        .expect("namespaced node");
    assert_eq!(node.meta.source_id, "example.line-graph");
    assert_eq!(node.meta.path, "n1");
    assert_eq!(result.graph.edges[0].source, "pest:example.line-graph:n1");
}

#[cfg(feature = "native")]
mod native {
    use std::io::Write;

    use data_loader::{Importer, Loader};

    use super::*;
    use crate::{FilesystemImporter, FilesystemLoader};

    #[test]
    fn filesystem_loader_binds_explicit_path_and_implements_loader() {
        let mut file = tempfile::NamedTempFile::new().expect("temp input");
        write!(file, "N|n1|One|kind||").expect("write input");
        let path = file.path().to_owned();
        let loader = FilesystemLoader::new(package(), &path).expect("pest package binds");

        assert_eq!(loader.name(), "Line graph");
        assert_eq!(loader.root_path(), Some(&path));
        let schema = loader.schema();
        assert!(schema.field("owner").unwrap().searchable);
        assert!(schema.field("zone").unwrap().facetable);
        let result = loader.load();
        assert_eq!(result.graph.node_count(), 1);
        schema.validate_result(&result).unwrap();

        let importer = FilesystemImporter::new(package(), &path).expect("pest package binds");
        let descriptor = importer.descriptor();
        descriptor.validate().unwrap();
        assert_eq!(descriptor.schema, schema);
    }

    #[test]
    fn filesystem_loader_rejects_json_engine_packages() {
        let json_package = crate::json::tests_support::minimal_json_package();
        let error = FilesystemLoader::new(json_package, "input.txt")
            
            .err()
            .expect("a json package must not bind to a filesystem input");
        assert!(matches!(error, ImportError::WrongEngine { .. }), "{error:?}");
    }

    #[test]
    fn loader_reports_parse_failures_through_load_result() {
        let mut file = tempfile::NamedTempFile::new().expect("temp input");
        write!(file, "not valid").expect("write input");
        let loader = FilesystemLoader::new(package(), file.path()).expect("pest package binds");
        let result = loader.load();

        assert_eq!(result.graph.node_count(), 0);
        assert_eq!(result.unresolved.len(), 1);
        assert!(result.unresolved[0].contains("example.line-graph"));
    }

    #[tokio::test]
    async fn filesystem_importer_satisfies_the_shared_import_contract() {
        let mut file = tempfile::NamedTempFile::new().expect("temp input");
        write!(
            file,
            "N|n1|Alpha|service|red,prod|owner=platform\nN|n2|Beta|database||\nE|n1|n2"
        )
        .expect("write input");
        let importer = FilesystemImporter::new(package(), file.path()).expect("pest package binds");
        data_loader::testing::assert_import_contract(&importer).await;
    }
}

// --- optional content capture -------------------------------------------------

const CONTENT_GRAMMAR: &str = r#"
document = { SOI ~ (record ~ NEWLINE?)* ~ EOI }
record = _{ node | edge }
node = { "N|" ~ node_id ~ "|" ~ title ~ "|" ~ kind ~ "|" ~ tags ~ "|" ~ properties ~ content }
node_id = @{ field }
title = @{ field }
kind = @{ field }
tags = _{ (tag ~ ("," ~ tag)*)? }
tag = @{ atom }
properties = _{ (property ~ (";" ~ property)*)? }
property = { key ~ "=" ~ value }
key = @{ atom }
value = @{ atom }
content = _{ "|" ~ body }
body = @{ (!NEWLINE ~ ANY)+ }
edge = { "E|" ~ source ~ "|" ~ target }
source = @{ field }
target = @{ field }
field = _{ (!("|" | NEWLINE) ~ ANY)+ }
atom = _{ (!("," | ";" | "=" | "|" | NEWLINE) ~ ANY)+ }
"#;

fn content_manifest(captures: &str) -> String {
    format!(
        r#"format_version = 3

[metadata]
id = "example.content-graph"
name = "Content graph"
version = "0.1.0"
description = "Package with per-node bodies"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''{CONTENT_GRAMMAR}'''

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
{captures}
"#
    )
}

#[test]
fn content_capture_marks_nodes_readable_and_returns_bodies() {
    let package = ValidatedPackage::from_toml(&content_manifest("content = \"body\""))
        .expect("valid content package");
    assert!(captures_content(package.manifest()));

    let input = concat!(
        "N|n1|Alpha|service|red|owner=platform|# alpha body\n",
        "N|n2|Beta|database||state=ready|# beta body\n"
    );
    let (result, bodies) = package
        .parse_input_with_bodies(input)
        .expect("input parses");

    for node in result.graph.nodes.values() {
        assert!(node.meta.content_readable, "node must advertise body");
        assert_eq!(
            node.meta.content_type.as_deref(),
            Some("text/markdown")
        );
    }
    assert_eq!(bodies.get("n1").map(String::as_str), Some("# alpha body"));
    assert_eq!(bodies.get("n2").map(String::as_str), Some("# beta body"));

    // The schema advertises readable content, and the node bodies do not
    // escape outside it.
    let schema = package.schema();
    assert!(schema.content.readable);
    assert!(!schema.content.writable);
    assert!(schema.validate_result(&result).is_ok());
}

#[test]
fn packages_without_content_stay_metadata_only() {
    let package = package();
    assert!(!captures_content(package.manifest()));
    let result = package
        .parse_input("N|n1|Alpha|service|red|owner=platform")
        .expect("input parses");
    for node in result.graph.nodes.values() {
        assert!(!node.meta.content_readable);
    }
    assert!(package.schema().validate_result(&result).is_ok());
}

#[test]
fn content_rule_must_be_distinct_from_canonical_roles() {
    let clash = content_manifest("content = \"title\"");
    let error = ValidatedPackage::from_toml(&clash).expect_err("clashing rule must fail");
    assert!(
        matches!(error, ImportError::AmbiguousCaptureRule { .. }),
        "got {error:?}"
    );
}
