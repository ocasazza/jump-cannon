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

/// Optional x/y capture roles: authored coordinates land on the node's
/// initial position; nodes without them stay at 0.0 (the server's
/// circle-fallback signal).
const POSITIONS_PACKAGE: &str = r#"format_version = 3

[metadata]
id = "example.positions"
name = "Positioned graph"
version = "1.0.0"
description = "Test x/y captures"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''
document = { SOI ~ (node ~ NEWLINE?)* ~ EOI }
node = { "N|" ~ node_id ~ "|" ~ x_pos ~ "|" ~ y_pos }
node_id = @{ field }
x_pos = @{ number }
y_pos = @{ number }
title = { "@@title@@" }
kind = { "@@kind@@" }
tag = { "@@tag@@" }
property = { "@@property@@" }
key = { "@@key@@" }
value = { "@@value@@" }
edge = { "@@edge@@" }
source = { "@@source@@" }
target = { "@@target@@" }
field = _{ (!("|" | NEWLINE) ~ ANY)+ }
number = _{ "-"? ~ '0'..'9'+ ~ ("." ~ '0'..'9'+)? }
'''

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
x = "x_pos"
y = "y_pos"
"#;

#[test]
fn xy_captures_seed_authored_positions() {
    let package =
        ValidatedPackage::from_toml(POSITIONS_PACKAGE).expect("valid positions package");
    let result = package
        .parse_input("N|a|-1.5|2.25\nN|b|3|4")
        .expect("input parses");

    let a = &result.graph.nodes["pest:example.positions:a"];
    assert_eq!((a.x, a.y), (-1.5, 2.25));
    let b = &result.graph.nodes["pest:example.positions:b"];
    assert_eq!((b.x, b.y), (3.0, 4.0));
}

#[test]
fn overflowing_x_literal_pins_f32_infinity_behavior() {
    let package =
        ValidatedPackage::from_toml(POSITIONS_PACKAGE).expect("valid positions package");
    // Rust f32 parsing accepts overflowing literals as ±inf; the grammar's
    // `number` rule keeps non-numeric text out, so this is the only parse
    // edge reachable through a validated grammar. Pin the behavior.
    let result = package
        .parse_input("N|a|999999999999999999999999999999999999999999|1")
        .expect("huge literal parses");
    assert!(result.graph.nodes["pest:example.positions:a"].x.is_infinite());
}

#[test]
fn packages_without_xy_captures_default_to_zero_positions() {
    let result = package()
        .parse_input("N|n1|Alpha|service||\n")
        .expect("input parses");
    let node = &result.graph.nodes["pest:example.line-graph:n1"];
    assert_eq!((node.x, node.y), (0.0, 0.0));
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

/// Every pest package the chart ships under `charts/jump-cannon/packages/`
/// must parse its sibling example input (`examples/<package-stem>.txt`) and
/// satisfy the full discovery contract (`validate_result`: one search
/// document per node, indexed id/title/tags equal to the canonical node's).
/// The examples are the user-facing documentation of each grammar, so a
/// grammar edit that breaks its example fails here.
#[test]
fn shipped_pest_packages_parse_their_examples() {
    let packages_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../charts/jump-cannon/packages");
    let mut entries: Vec<_> = std::fs::read_dir(packages_dir)
        .expect("packages dir exists")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort();

    let mut parsed_examples = 0;
    for path in entries {
        let bytes = std::fs::read(&path).expect("package readable");
        let package = ValidatedPackage::from_toml_bytes(&bytes)
            .unwrap_or_else(|error| panic!("{} validates: {error}", path.display()));
        if package.engine() != crate::EngineKind::Pest {
            continue;
        }
        let example = path
            .parent()
            .expect("packages dir has a parent")
            .join("examples")
            .join(format!(
                "{}.txt",
                path.file_stem().expect("package file name").to_string_lossy()
            ));
        let input = std::fs::read_to_string(&example).unwrap_or_else(|error| {
            panic!(
                "pest package {} ships an example at {}: {error}",
                path.display(),
                example.display()
            )
        });
        let result = package
            .parse_input(&input)
            .unwrap_or_else(|error| panic!("{} parses its example: {error}", path.display()));
        assert!(
            result.graph.node_count() > 0,
            "{} example must produce nodes",
            path.display()
        );
        package
            .schema()
            .validate_result(&result)
            .unwrap_or_else(|error| panic!("{} discovery contract: {error}", path.display()));
        parsed_examples += 1;
    }
    assert!(
        parsed_examples > 0,
        "at least one shipped pest package must be exercised"
    );
}

const LABEL_GRAMMAR: &str = r#"
document = { SOI ~ (item ~ NEWLINE?)* ~ EOI }
item = { word ~ (" " ~ (hot | cold))* }
word = @{ (!(" " | "\t" | NEWLINE) ~ ANY)+ }
hot = @{ "H" }
cold = @{ "C" }
title = { "@@title@@" }
kind = { "@@kind@@" }
tag = { "@@tag@@" }
property = { key ~ "=" ~ value }
key = { "@@key@@" }
value = { "@@value@@" }
edge = { source ~ "@@->@@" ~ target }
source = { "@@source@@" }
target = { "@@target@@" }
"#;

fn label_manifest(tag_labels: &str) -> String {
    format!(
        r#"format_version = 3

[metadata]
id = "example.labels"
name = "Labeled tags"
version = "1.0.0"
description = "Test labeled feature tags"

[parser]
engine = "pest"
root_rule = "document"
grammar = '''{LABEL_GRAMMAR}'''

[parser.captures]
node = "item"
id = "word"
title = "title"
kind = "kind"
tag = "tag"
property = "property"
key = "key"
value = "value"
edge = "edge"
source = "source"
target = "target"
{tag_labels}
"#
    )
}

#[test]
fn tag_labels_push_static_labels_for_matching_rules() {
    let package = ValidatedPackage::from_toml(&label_manifest(
        "\n[parser.captures.tag_labels]\nhot = \"hot\"\ncold = \"cold\"\n",
    ))
    .expect("valid labeled package");
    let result = package
        .parse_input("n1 H C H\nn2\nn3 C")
        .expect("input parses");

    let tags = |id: &str| {
        result
            .graph
            .nodes
            .get(&format!("pest:example.labels:{id}"))
            .unwrap_or_else(|| panic!("{id} present"))
            .meta
            .tags
            .clone()
    };
    assert_eq!(tags("n1"), vec!["cold".to_owned(), "hot".to_owned()], "sorted, deduplicated");
    assert!(tags("n2").is_empty(), "no feature match, no label");
    assert_eq!(tags("n3"), vec!["cold".to_owned()]);
    package
        .schema()
        .validate_result(&result)
        .expect("output satisfies the discovery contract");
}

#[test]
fn tag_labels_validate_their_rule_bindings() {
    let missing = label_manifest("\n[parser.captures.tag_labels]\nghost = \"ghost\"\n");
    assert!(matches!(
        ValidatedPackage::from_toml(&missing),
        Err(ImportError::MissingRule { role: "tag_labels", .. })
    ));

    let empty = label_manifest("\n[parser.captures.tag_labels]\nhot = \"  \"\n");
    assert!(matches!(
        ValidatedPackage::from_toml(&empty),
        Err(ImportError::Grammar(_))
    ));

    let collides = label_manifest("\n[parser.captures.tag_labels]\nword = \"word-label\"\n");
    assert!(matches!(
        ValidatedPackage::from_toml(&collides),
        Err(ImportError::AmbiguousCaptureRule { second_role: "tag_labels", .. })
    ));
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
