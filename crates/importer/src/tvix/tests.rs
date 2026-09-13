//! Tvix engine tests: package validation, typed/bounded variable resolution,
//! and evaluation of the embedded generator library. Evaluation runs the real
//! `tvix_wasm::eval_graph` against the compiled-in `graph.nix`.

use std::collections::BTreeMap;

use data_loader::NoProgress;

use super::*;
use crate::ValidatedPackage;

/// A minimal `random`-generator package with two integer variables.
fn random_package(extra_variables: &str) -> String {
    format!(
        r#"format_version = 3

[metadata]
id = "generate.test"
name = "Test generator"
version = "1.0.0"

[parser]
engine = "tvix"
expr = '''
{{ nodes, edges }}:
let g = import /jc/src/graph.nix {{ }};
in g.random {{ inherit nodes edges; seed = 0; }}
'''

[[parser.variables]]
name = "nodes"
type = "integer"
default = "4"
min = 1
{extra_variables}

[[parser.variables]]
name = "edges"
type = "integer"
default = "3"
min = 0
"#
    )
}

fn supplied(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A tvix package with two integer variables evaluates to exactly the node and
/// edge counts its bound variables ask for.
#[tokio::test]
async fn bound_variables_drive_the_evaluated_counts() {
    let package = ValidatedPackage::from_toml(&random_package("")).expect("package validates");
    let importer = build_tvix_importer(package, supplied(&[("nodes", "6"), ("edges", "9")]), "test".to_owned())
        .expect("importer binds");
    let result = importer
        .import(&NoProgress)
        .await
        .expect("evaluation succeeds");
    assert_eq!(result.graph.node_count(), 6, "six nodes requested");
    assert_eq!(result.graph.edge_count(), 9, "nine edges requested");
    // The instance namespace threads through every node id.
    assert!(
        result.graph.nodes.contains_key("tvix:test:n0"),
        "node ids are namespaced by source_id: {:?}",
        result.graph.nodes.keys().collect::<Vec<_>>()
    );
}

/// Applying defaults: with nothing supplied, the declared defaults (4 nodes,
/// 3 edges) drive the graph.
#[tokio::test]
async fn defaults_apply_when_nothing_is_supplied() {
    let package = ValidatedPackage::from_toml(&random_package("")).expect("package validates");
    let importer =
        build_tvix_importer(package, BTreeMap::new(), "test".to_owned()).expect("importer binds");
    let result = importer.import(&NoProgress).await.expect("evaluation succeeds");
    assert_eq!(result.graph.node_count(), 4);
    assert_eq!(result.graph.edge_count(), 3);
}

/// A variable value outside its declared `min..max` fails validation with a
/// message naming the offending variable.
#[test]
fn out_of_bounds_variable_fails_naming_it() {
    let package =
        ValidatedPackage::from_toml(&random_package("max = 100")).expect("package validates");
    let Err(error) = build_tvix_importer(package, supplied(&[("nodes", "500")]), "test".to_owned())
    else {
        panic!("value above max must be rejected");
    };
    let message = error.to_string();
    assert!(
        message.contains("nodes"),
        "error must name the variable: {message}"
    );
    assert!(
        message.contains("maximum"),
        "error must explain the bound: {message}"
    );
}

/// A package whose expression is not a function fails at evaluation with a
/// clear, non-empty message — the engine always applies the variable attrset.
#[tokio::test]
async fn non_function_expression_fails_at_evaluation() {
    let source = r#"format_version = 3

[metadata]
id = "generate.notfn"
name = "Not a function"
version = "1.0.0"

[parser]
engine = "tvix"
expr = '''
let g = import /jc/src/graph.nix { };
in g.random { nodes = 5; edges = 4; seed = 0; }
'''
"#;
    let package = ValidatedPackage::from_toml(source).expect("package validates");
    let importer =
        build_tvix_importer(package, BTreeMap::new(), "test".to_owned()).expect("importer binds");
    let error = importer
        .import(&NoProgress)
        .await
        .expect_err("applying a non-function must fail");
    match error {
        data_loader::ImportError::Decode { origin, message } => {
            assert_eq!(origin, "tvix");
            assert!(!message.is_empty(), "evaluation error must carry a message");
        }
        other => panic!("expected a decode error, got {other}"),
    }
}

/// An unknown variable `type` is rejected at parse time, naming the bad type.
#[test]
fn unknown_variable_type_is_rejected_at_parse() {
    let source = r#"format_version = 3

[metadata]
id = "generate.badtype"
name = "Bad type"
version = "1.0.0"

[parser]
engine = "tvix"
expr = "{ nodes }: { nodes = [ ]; links = [ ]; }"

[[parser.variables]]
name = "nodes"
type = "int"
"#;
    let error = ValidatedPackage::from_toml(source).expect_err("unknown type must fail");
    assert!(
        matches!(error, crate::ImportError::TvixEngine(_)),
        "expected a tvix engine error, got {error:?}"
    );
    assert!(
        error.to_string().contains("int"),
        "error must name the offending type: {error}"
    );
}

/// The clustered generator partitions nodes into communities exposed as the
/// node `type` tag.
#[tokio::test]
async fn clustered_generator_tags_communities() {
    let source = r#"format_version = 3

[metadata]
id = "generate.clustertest"
name = "Cluster test"
version = "1.0.0"

[parser]
engine = "tvix"
expr = '''
{ nodes, edges, clusters, affinity, seed }:
let g = import /jc/src/graph.nix { };
in g.clustered { inherit nodes edges clusters affinity seed; }
'''

[[parser.variables]]
name = "nodes"
type = "integer"
default = "12"

[[parser.variables]]
name = "edges"
type = "integer"
default = "24"

[[parser.variables]]
name = "clusters"
type = "integer"
default = "3"

[[parser.variables]]
name = "affinity"
type = "number"
default = "0.8"
min = 0.0
max = 1.0

[[parser.variables]]
name = "seed"
type = "integer"
default = "0"
"#;
    let package = ValidatedPackage::from_toml(source).expect("package validates");
    let importer =
        build_tvix_importer(package, BTreeMap::new(), "test".to_owned()).expect("importer binds");
    let result = importer.import(&NoProgress).await.expect("evaluation succeeds");
    assert_eq!(result.graph.node_count(), 12);
    for node in result.graph.nodes.values() {
        assert!(
            node.meta.tags.iter().any(|tag| tag.starts_with("cluster-")),
            "every node carries its cluster tag, got {:?}",
            node.meta.tags
        );
    }
}

/// A declared default that violates its own bound is caught at parse time.
#[test]
fn default_outside_bounds_is_rejected_at_parse() {
    let error = ValidatedPackage::from_toml(&random_package("max = 2"))
        .expect_err("default 4 above max 2 must fail");
    assert!(
        matches!(error, crate::ImportError::TvixEngine(_)),
        "expected a tvix engine error, got {error:?}"
    );
}

/// An unknown supplied variable is rejected, listing the accepted names.
#[test]
fn unknown_supplied_variable_is_rejected() {
    let package = ValidatedPackage::from_toml(&random_package("")).expect("package validates");
    let Err(error) = build_tvix_importer(package, supplied(&[("nodez", "6")]), "test".to_owned())
    else {
        panic!("unknown variable must be rejected");
    };
    assert!(
        error.to_string().contains("nodez"),
        "error must name the unknown variable: {error}"
    );
}
