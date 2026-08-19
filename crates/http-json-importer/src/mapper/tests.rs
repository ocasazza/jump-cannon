//! Mapper tests: inline packages plus inline decoded documents, no network.

use std::collections::BTreeMap;

use data_loader::{identity::Namespace, DecodedRecord, GraphMapper, LoadResult};
use serde_json::{json, Value};

use crate::manifest::{ValidatedPackage, SOURCE_KIND};
use crate::mapper::ManifestMapper;
use crate::RECORD_COLLECTION_KEY;

/// A package covering every rule the engine supports: two node collections
/// joined by title, one joined by id, and an API-provided link list.
const PACKAGE: &str = r#"
format_version = 1

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

[[variables]]
name = "bank"
default = "omp"

[[collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[collections.nodes]
id_pointer = "/id"
node_type = "memory"
tags_pointer = "/tags"
skip_unless = { pointer = "/state", equals = "valid", missing_matches = true }

[collections.nodes.title]
pointer = "/text"
max_chars = 20
fallback_prefix = "memory"

[[collections.nodes.fields]]
key = "body"
pointer = "/text"

[[collections.nodes.fields]]
key = "entities"
pointer = "/entities"
transform = "split_csv"

[[collections.nodes.edges]]
kind = "mentions"
value_pointer = "/entities"
transform = "split_csv"
target_collection = "entities"
match_on = "title"

[[collections.nodes.edges]]
kind = "documented_in"
value_pointer = "/document_id"
target_collection = "documents"
match_on = "id"

[[collections]]
name = "entities"
path = "/v1/banks/{bank}/entities"

[collections.nodes]
id_pointer = "/id"
local_prefix = "entity:"
node_type = "entity"

[collections.nodes.title]
pointer = "/canonical_name"
fallback_prefix = "entity"

[[collections]]
name = "documents"
path = "/v1/banks/{bank}/documents"

[collections.nodes]
id_pointer = "/id"
local_prefix = "document:"
node_type = "document"

[collections.nodes.title]
pointer = "/name"
fallback_prefix = "document"

[[collections]]
name = "links"
path = "/v1/banks/{bank}/graph"
items_pointer = "/edges"

[collections.edges]
source_pointer = "/data/source"
target_pointer = "/data/target"
kind_pointer = "/data/linkType"
include_kinds = ["temporal", "semantic", "caused_by"]
endpoints_collection = "memories"

[schema]
[[schema.fields]]
key = "body"
field_type = "text"
required = false
searchable = true
boost = 1
facetable = false
snippet = true

[[schema.fields]]
key = "entities"
field_type = "keyword_list"
required = false
searchable = true
boost = 1
facetable = true

[[schema.edge_types]]
key = "temporal"
directed = true

[[schema.edge_types]]
key = "semantic"
directed = false

[[schema.edge_types]]
key = "caused_by"
directed = true

[[schema.edge_types]]
key = "mentions"
directed = true

[[schema.edge_types]]
key = "documented_in"
directed = true
"#;

fn package() -> ValidatedPackage {
    ValidatedPackage::from_toml_bytes(PACKAGE.as_bytes()).expect("package validates")
}

fn mapper() -> ManifestMapper {
    ManifestMapper::new(
        package(),
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    )
}

/// One decoded page tagged with its collection, exactly as the connector tags
/// records before decoding.
fn record(collection: &str, value: Value) -> DecodedRecord {
    let mut metadata = BTreeMap::new();
    metadata.insert(RECORD_COLLECTION_KEY.to_string(), json!(collection));
    DecodedRecord {
        origin: format!("http://test/{collection}"),
        value,
        metadata,
    }
}

/// The hindsight shape: valid + invalidated units, entities joined by name, a
/// document joined by id, and a link list carrying a self-loop, a reversed
/// duplicate, and an excluded `entity` link.
fn hindsight_records() -> Vec<DecodedRecord> {
    vec![
        record(
            "memories",
            json!({"items": [
                {"id": "u1", "text": "First fact about tofu state migration handling",
                 "entities": "tofu, Hydra", "tags": ["project:x", "project:x", " "],
                 "state": "valid", "document_id": "doc-1"},
                {"id": "u2", "text": "Second fact", "entities": "Hydra",
                 "tags": [], "state": "valid", "document_id": "doc-1"},
                {"id": "u3", "text": "Retired fact", "entities": "tofu",
                 "tags": [], "state": "invalidated", "document_id": "doc-1"}
            ]}),
        ),
        record(
            "entities",
            json!({"items": [
                {"id": "e1", "canonical_name": "tofu"},
                {"id": "e2", "canonical_name": "Hydra"}
            ]}),
        ),
        record(
            "documents",
            json!({"items": [{"id": "doc-1", "name": "session one"}]}),
        ),
        record(
            "links",
            json!({"edges": [
                {"data": {"source": "u1", "target": "u1", "linkType": "semantic"}},
                {"data": {"source": "u1", "target": "u2", "linkType": "temporal"}},
                {"data": {"source": "u2", "target": "u1", "linkType": "temporal"}},
                {"data": {"source": "u1", "target": "u2", "linkType": "entity"}},
                {"data": {"source": "u1", "target": "u3", "linkType": "caused_by"}}
            ]}),
        ),
    ]
}

fn mapped() -> LoadResult {
    mapper().map(hindsight_records()).expect("mapping succeeds")
}

fn node_id(local: &str) -> String {
    format!("{SOURCE_KIND}:omp:{local}")
}

fn has_edge(result: &LoadResult, source: &str, target: &str) -> bool {
    let (source, target) = (node_id(source), node_id(target));
    result
        .graph
        .edges
        .iter()
        .any(|edge| edge.source == source && edge.target == target)
}

#[test]
fn output_satisfies_the_declared_schema() {
    let result = mapped();
    // The real contract: the host runs exactly this on every import.
    package()
        .schema()
        .validate_result(&result)
        .expect("mapper output must satisfy its own declared schema");
    result.graph.validate().expect("graph invariants hold");
}

#[test]
fn skip_unless_drops_retired_documents() {
    let result = mapped();
    assert!(result.graph.nodes.contains_key(&node_id("u1")));
    assert!(result.graph.nodes.contains_key(&node_id("u2")));
    assert!(
        !result.graph.nodes.contains_key(&node_id("u3")),
        "invalidated unit must not publish"
    );
    // 2 memories + 2 entities + 1 document
    assert_eq!(result.graph.node_count(), 5);
}

#[test]
fn titles_truncate_at_a_word_boundary_and_never_empty() {
    let result = mapped();
    let title = &result.graph.nodes[&node_id("u1")].meta.title;
    assert!(title.ends_with('…'), "expected truncation, got {title:?}");
    assert!(title.chars().count() <= 21, "got {title:?}");
    assert!(!title.contains("migration"), "cut at a word boundary: {title:?}");
    // Untruncated titles pass through verbatim.
    assert_eq!(result.graph.nodes[&node_id("u2")].meta.title, "Second fact");
}

#[test]
fn title_falls_back_to_prefixed_short_id() {
    let records = vec![
        record("memories", json!({"items": [{"id": "abcdefghij", "text": "  "}]})),
        record("entities", json!({"items": []})),
        record("documents", json!({"items": []})),
        record("links", json!({"edges": []})),
    ];
    let result = mapper().map(records).expect("mapping succeeds");
    assert_eq!(
        result.graph.nodes[&node_id("abcdefghij")].meta.title,
        "memory abcdefgh"
    );
}

#[test]
fn tags_are_trimmed_deduplicated_and_mirrored_into_search() {
    let result = mapped();
    let node = &result.graph.nodes[&node_id("u1")];
    assert_eq!(node.meta.tags, vec!["project:x"]);
    let document = result
        .search_documents
        .iter()
        .find(|document| document.node_id == node_id("u1"))
        .expect("search document");
    assert_eq!(document.fields["tags"], json!(["project:x"]));
    assert_eq!(document.fields["type"], json!("memory"));
    assert_eq!(document.fields["folder"], json!("memories"));
}

#[test]
fn split_csv_fields_become_keyword_lists() {
    let result = mapped();
    let document = result
        .search_documents
        .iter()
        .find(|document| document.node_id == node_id("u1"))
        .expect("search document");
    assert_eq!(document.fields["entities"], json!(["tofu", "Hydra"]));
}

#[test]
fn mentions_resolve_by_title_case_insensitively() {
    let result = mapped();
    assert!(has_edge(&result, "u1", "entity:e1"), "tofu");
    assert!(has_edge(&result, "u1", "entity:e2"), "Hydra");
    assert!(has_edge(&result, "u2", "entity:e2"));
    assert!(result.unresolved.is_empty(), "{:?}", result.unresolved);

    // A name no entity carries is reported, not silently dropped.
    let mut records = hindsight_records();
    records[1] = record("entities", json!({"items": [{"id": "e1", "canonical_name": "tofu"}]}));
    let result = mapper().map(records).expect("mapping succeeds");
    assert!(
        result.unresolved.iter().any(|entry| entry.contains("Hydra")),
        "{:?}",
        result.unresolved
    );
}

#[test]
fn documented_in_resolves_by_id_without_suffix_collisions() {
    let result = mapped();
    assert!(has_edge(&result, "u1", "document:doc-1"));
    assert!(has_edge(&result, "u2", "document:doc-1"));

    // `doc-1` must not resolve against `doc-11`.
    let mut records = hindsight_records();
    records[2] = record(
        "documents",
        json!({"items": [{"id": "doc-11", "name": "other"}]}),
    );
    let result = mapper().map(records).expect("mapping succeeds");
    assert!(!has_edge(&result, "u1", "document:doc-11"));
    assert!(result
        .unresolved
        .iter()
        .any(|entry| entry.contains("doc-1")));
}

#[test]
fn link_lists_filter_kinds_drop_self_loops_and_dedupe_unordered() {
    let result = mapped();
    // temporal u1<->u2 survives once despite being reported twice reversed.
    let temporal: Vec<_> = result
        .graph
        .edges
        .iter()
        .filter(|edge| {
            (edge.source == node_id("u1") && edge.target == node_id("u2"))
                || (edge.source == node_id("u2") && edge.target == node_id("u1"))
        })
        .collect();
    assert_eq!(temporal.len(), 1, "reversed duplicate must dedupe");
    // semantic self-loop dropped.
    assert!(!has_edge(&result, "u1", "u1"));
    // caused_by pointed at the filtered-out u3: no dangling edge.
    assert!(!result
        .graph
        .edges
        .iter()
        .any(|edge| edge.target == node_id("u3")));
    // The `entity` link kind is excluded by include_kinds; the only u1<->u2
    // edge is the temporal one counted above.
    assert_eq!(temporal.len(), 1);
}

#[test]
fn edge_endpoints_always_exist_as_nodes() {
    let result = mapped();
    for edge in &result.graph.edges {
        assert!(
            result.graph.nodes.contains_key(&edge.source),
            "dangling source {}",
            edge.source
        );
        assert!(
            result.graph.nodes.contains_key(&edge.target),
            "dangling target {}",
            edge.target
        );
    }
}

#[test]
fn records_are_grouped_by_their_collection_tag_not_by_order() {
    let mut records = hindsight_records();
    records.reverse();
    let result = mapper().map(records).expect("mapping succeeds");
    // Edges still resolve although the link list decoded before the nodes.
    assert!(has_edge(&result, "u1", "entity:e1"));
    assert!(has_edge(&result, "u1", "document:doc-1"));
    assert_eq!(result.graph.node_count(), 5);
}

#[test]
fn duplicate_node_id_from_paginated_recollection_is_tolerated() {
    // Simulates offset pagination re-observing "u1" on a second "memories"
    // page after it shifted position under concurrent writes (the Hindsight
    // omp-bank failure mode this guards against). The paginated collection
    // must keep the first occurrence and must not error.
    let mut records = hindsight_records();
    records.push(record(
        "memories",
        json!({"items": [
            {"id": "u1", "text": "A different later snapshot of the same unit",
             "entities": "tofu, Hydra", "tags": [], "state": "valid", "document_id": "doc-1"}
        ]}),
    ));
    let result = mapper().map(records).expect("duplicate from pagination is tolerated, not an error");
    assert_eq!(
        result.graph.node_count(),
        5,
        "the re-observed u1 must not add a second node"
    );
    let kept = result
        .graph
        .nodes
        .get(&node_id("u1"))
        .expect("u1 present");
    assert_eq!(
        kept.meta.frontmatter["body"],
        json!("First fact about tofu state migration handling"),
        "the first occurrence must win, not the later duplicate page"
    );
}

#[test]
fn duplicate_node_id_in_non_paginated_collection_still_fails() {
    // "documents" declares no `paginate`, so it defaults to `Pagination::None`
    // (a single request) -- a duplicate id there can only be a genuine
    // package/data bug and must still hard-fail.
    let mut records = hindsight_records();
    records.push(record(
        "documents",
        json!({"items": [{"id": "doc-1", "name": "a conflicting second document"}]}),
    ));
    let error = mapper()
        .map(records)
        .expect_err("a genuine duplicate id in a non-paginated collection must fail");
    let message = error.to_string();
    assert!(
        message.contains("duplicate node id"),
        "unexpected error: {message}"
    );
}
