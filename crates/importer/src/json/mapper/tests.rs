//! Mapper tests: inline packages plus inline decoded documents, no network.

use std::collections::BTreeMap;

use data_loader::{identity::Namespace, DecodedRecord, GraphMapper, LoadResult};
use serde_json::{json, Value};

use super::ManifestMapper;
use crate::json::config::SOURCE_KIND;
use crate::json::RECORD_COLLECTION_KEY;
use crate::ValidatedPackage;

/// A package covering every rule the engine supports: two node collections
/// joined by title, one joined by id, and an API-provided link list.
const PACKAGE: &str = r#"
format_version = 3

[metadata]
id = "test.shape"
name = "Test shape"
version = "1.0.0"

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

[parser]
engine = "json"

[[parser.variables]]
name = "bank"
default = "omp"

[[parser.collections]]
name = "memories"
path = "/v1/banks/{bank}/memories"
paginate = { style = "limit_offset" }

[parser.collections.nodes]
id_pointer = "/id"
node_type = "memory"
tags_pointer = "/tags"
skip_unless = { pointer = "/state", equals = "valid", missing_matches = true }

[parser.collections.nodes.doctype]
pointer = "/fact_type"
map = { world = "World Fact", experience = "Experience", observation = "Observation" }

[parser.collections.nodes.title]
pointer = "/text"
max_chars = 20
fallback_prefix = "memory"

[[parser.collections.nodes.fields]]
key = "body"
pointer = "/text"

[[parser.collections.nodes.fields]]
key = "entities"
pointer = "/entities"
transform = "split_csv"

[[parser.collections.nodes.edges]]
kind = "mentions"
value_pointer = "/entities"
transform = "split_csv"
target_collection = "entities"
match_on = "title"

[[parser.collections.nodes.edges]]
kind = "documented_in"
value_pointer = "/document_id"
target_collection = "documents"
match_on = "id"

[[parser.collections]]
name = "entities"
path = "/v1/banks/{bank}/entities"

[parser.collections.nodes]
id_pointer = "/id"
local_prefix = "entity:"
node_type = "entity"

[parser.collections.nodes.title]
pointer = "/canonical_name"
fallback_prefix = "entity"

[[parser.collections]]
name = "documents"
path = "/v1/banks/{bank}/documents"

[parser.collections.nodes]
id_pointer = "/id"
local_prefix = "document:"
node_type = "document"

[parser.collections.nodes.title]
pointer = "/name"
fallback_prefix = "document"

[[parser.collections]]
name = "links"
path = "/v1/banks/{bank}/graph"
items_pointer = "/edges"

[parser.collections.edges]
source_pointer = "/data/source"
target_pointer = "/data/target"
kind_pointer = "/data/linkType"
include_kinds = ["temporal", "semantic", "caused_by"]
endpoints_collection = "memories"
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
                 "state": "valid", "document_id": "doc-1", "fact_type": "world"},
                {"id": "u2", "text": "Second fact", "entities": "Hydra",
                 "tags": [], "state": "valid", "document_id": "doc-1",
                 "fact_type": "opinion"},
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
fn doctype_rule_resolves_per_record_with_collection_fallback() {
    let result = mapped();
    let doctype = |local: &str| result.graph.nodes[&node_id(local)].meta.doctype.clone();
    let indexed_type = |local: &str| {
        result
            .search_documents
            .iter()
            .find(|document| document.node_id == node_id(local))
            .expect("indexed document")
            .fields["type"]
            .clone()
    };
    // Mapped value → the display label, on both the node and the facet.
    assert_eq!(doctype("u1").as_deref(), Some("World Fact"));
    assert_eq!(indexed_type("u1"), json!("World Fact"));
    // A value outside the declared vocabulary falls back to node_type.
    assert_eq!(doctype("u2").as_deref(), Some("memory"));
    assert_eq!(indexed_type("u2"), json!("memory"));
    // Collections without a doctype rule still type every node.
    assert_eq!(doctype("entity:e1").as_deref(), Some("entity"));
    assert_eq!(indexed_type("entity:e1"), json!("entity"));
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
    assert_eq!(document.fields["type"], json!("World Fact"));
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

/// PRIDE shape: an edge rule whose value_pointer names a JSON array of
/// scalars (projectFileNames) with no transform must yield one edge per
/// element, matched against the target collection's titles.
const PACKAGE_ARRAY_EDGES: &str = r#"
format_version = 3

[metadata]
id = "test.array-edges"
name = "Array edges"
version = "1.0.0"

[[schema.edge_types]]
key = "contains"
directed = true

[parser]
engine = "json"

[[parser.variables]]
name = "accession"
default = "PXD1"

[[parser.collections]]
name = "projects"
path = "/search/projects"
items_pointer = ""

[parser.collections.nodes]
id_pointer = "/accession"
node_type = "project"

[parser.collections.nodes.title]
pointer = "/title"
fallback_prefix = "project"

[[parser.collections.nodes.edges]]
kind = "contains"
value_pointer = "/projectFileNames"
target_collection = "files"
match_on = "title"

[[parser.collections]]
name = "files"
path = "/projects/{accession}/files"
items_pointer = ""

[parser.collections.nodes]
id_pointer = "/accession"
local_prefix = "file:"
node_type = "file"

[parser.collections.nodes.title]
pointer = "/fileName"
fallback_prefix = "file"
"#;

#[test]
fn array_valued_edge_pointer_yields_one_edge_per_element() {
    let package =
        ValidatedPackage::from_toml_bytes(PACKAGE_ARRAY_EDGES.as_bytes()).expect("validates");
    let mapper = ManifestMapper::new(
        package,
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    );
    let result = mapper
        .map(vec![
            record(
                "projects",
                json!([{"accession": "PXD1", "title": "A project",
                        "projectFileNames": ["a.raw", "b.raw", "missing.raw"]}]),
            ),
            record(
                "files",
                json!([{"accession": "f1", "fileName": "a.raw"},
                       {"accession": "f2", "fileName": "b.raw"}]),
            ),
        ])
        .expect("mapping succeeds");
    assert!(has_edge(&result, "PXD1", "file:f1"));
    assert!(has_edge(&result, "PXD1", "file:f2"));
    assert_eq!(result.graph.edge_count(), 2);
    assert_eq!(
        result.unresolved,
        ["files \"missing.raw\" referenced by PXD1"],
        "the one unmatched name must be reported unresolved"
    );
}

#[test]
fn root_array_items_pointer_maps_nodes() {
    // Same package: both collections read bare arrays at the document root
    // (`items_pointer = ""`).
    let package =
        ValidatedPackage::from_toml_bytes(PACKAGE_ARRAY_EDGES.as_bytes()).expect("validates");
    let mapper = ManifestMapper::new(
        package,
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    );
    let result = mapper
        .map(vec![
            record("projects", json!([{"accession": "PXD1", "title": "A project",
                                       "projectFileNames": []}])),
            record("files", json!([{"accession": "f1", "fileName": "a.raw"}])),
        ])
        .expect("mapping succeeds");
    assert_eq!(result.graph.node_count(), 2);
    assert!(result.graph.nodes.contains_key(&node_id("PXD1")));
    assert!(result.graph.nodes.contains_key(&node_id("file:f1")));
}

#[test]
fn duplicate_node_id_from_page_number_recollection_is_tolerated() {
    // DataCite's search ranking drifts between page fetches, so a
    // page-number walk can re-observe the same DOI on a later page. The
    // first occurrence must win without an error, exactly like the
    // limit_offset re-observation case.
    let package_src = PACKAGE_ARRAY_EDGES.replace(
        "path = \"/projects/{accession}/files\"\nitems_pointer = \"\"",
        "path = \"/projects/{accession}/files\"\npaginate = { style = \"page_number\" }\nitems_pointer = \"\"",
    );
    let package =
        ValidatedPackage::from_toml_bytes(package_src.as_bytes()).expect("validates");
    let mapper = ManifestMapper::new(
        package,
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    );
    let result = mapper
        .map(vec![
            record(
                "projects",
                json!([{"accession": "PXD1", "title": "A project",
                        "projectFileNames": ["a.raw"]}]),
            ),
            record("files", json!([{"accession": "f1", "fileName": "a.raw"}])),
            record(
                "files",
                json!([{"accession": "f1", "fileName": "a.raw, re-observed on a later page"}]),
            ),
        ])
        .expect("duplicate from page-number walk is tolerated, not an error");
    assert_eq!(result.graph.node_count(), 2);
    let kept = result
        .graph
        .nodes
        .get(&node_id("file:f1"))
        .expect("f1 present");
    assert_eq!(kept.meta.title, "a.raw", "the first occurrence must win");
}

const PACKAGE_PLUCK_EDGES: &str = r#"
format_version = 3

[metadata]
id = "test.pluck"
name = "Pluck edges"
version = "1.0.0"

[[schema.edge_types]]
key = "related"
directed = true

[parser]
engine = "json"

[[parser.collections]]
name = "dois"
path = "/dois"
items_pointer = ""

[parser.collections.nodes]
id_pointer = "/id"
node_type = "work"

[parser.collections.nodes.title]
pointer = "/title"

[[parser.collections.nodes.edges]]
kind = "related"
value_pointer = "/related"
transform = "pluck"
element_pointer = "/doi"
target_collection = "dois"
"#;

#[test]
fn pluck_edge_pointer_yields_one_edge_per_object_element() {
    // DataCite relatedIdentifiers shape: an array of objects, each reduced
    // to its DOI; in-set targets resolve, out-of-set are reported
    // unresolved, and elements lacking the plucked key are skipped.
    let package =
        ValidatedPackage::from_toml_bytes(PACKAGE_PLUCK_EDGES.as_bytes()).expect("validates");
    let mapper = ManifestMapper::new(
        package,
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    );
    let result = mapper
        .map(vec![record(
            "dois",
            json!([
                {"id": "10.1/a", "title": "A",
                 "related": [{"relationType": "Cites", "doi": "10.1/b"},
                             {"relationType": "Cites", "doi": "10.1/absent"},
                             {"relationType": "IsSupplementTo"}]},
                {"id": "10.1/b", "title": "B", "related": []},
            ]),
        )])
        .expect("mapping succeeds");
    assert!(has_edge(&result, "10.1/a", "10.1/b"));
    assert_eq!(result.graph.edge_count(), 1);
    assert_eq!(
        result.unresolved,
        ["dois \"10.1/absent\" referenced by 10.1/a"],
        "the out-of-set DOI must be reported unresolved"
    );
}

#[test]
fn pluck_requires_an_element_pointer_and_is_edge_only() {
    let no_element = PACKAGE_PLUCK_EDGES.replacen(
        "element_pointer = \"/doi\"\n",
        "",
        1,
    );
    assert!(matches!(
        ValidatedPackage::from_toml_bytes(no_element.as_bytes()),
        Err(crate::ImportError::JsonEngine(_))
    ));

    let element_without_pluck = PACKAGE_PLUCK_EDGES.replacen(
        "transform = \"pluck\"\n",
        "",
        1,
    );
    assert!(matches!(
        ValidatedPackage::from_toml_bytes(element_without_pluck.as_bytes()),
        Err(crate::ImportError::JsonEngine(_))
    ));

    let pluck_on_field = PACKAGE_PLUCK_EDGES.replace(
        "[[parser.collections.nodes.edges]]",
        "[[parser.collections.nodes.fields]]\nkey = \"doi\"\npointer = \"/related\"\ntransform = \"pluck\"\n\n[[parser.collections.nodes.edges]]",
    );
    // The field addition must declare its schema field; the pluck rejection
    // fires first regardless.
    assert!(matches!(
        ValidatedPackage::from_toml_bytes(pluck_on_field.as_bytes()),
        Err(crate::ImportError::JsonEngine(_))
    ));
}

#[test]
fn tags_element_pointer_plucks_object_subjects() {
    // DataCite subjects shape: an array of `{ "subject": "…" }` objects. The
    // element pointer reduces each to its string; scalars in the same array
    // are skipped rather than misread.
    let package_src = PACKAGE_PLUCK_EDGES.replacen(
        "node_type = \"work\"",
        "node_type = \"work\"\ntags_pointer = \"/subjects\"\ntags_element_pointer = \"/subject\"",
        1,
    );
    let package =
        ValidatedPackage::from_toml_bytes(package_src.as_bytes()).expect("validates");
    let mapper = ManifestMapper::new(
        package,
        Namespace::new(SOURCE_KIND, "omp").expect("namespace"),
    );
    let result = mapper
        .map(vec![record(
            "dois",
            json!([{"id": "10.1/a", "title": "A", "related": [],
                    "subjects": [{"subject": "Paleoproteomics"},
                                 {"subject": "Paleoproteomics"},
                                 {"subject": "  "},
                                 {"subject": "Silk Road"}]}]),
        )])
        .expect("mapping succeeds");
    let node = result
        .graph
        .nodes
        .get(&node_id("10.1/a"))
        .expect("node present");
    assert_eq!(
        node.meta.tags,
        vec!["Paleoproteomics".to_owned(), "Silk Road".to_owned()],
        "plucked, trimmed, deduplicated, blanks dropped"
    );
}

#[test]
fn tags_element_pointer_requires_a_tags_pointer() {
    let package_src = PACKAGE_PLUCK_EDGES.replacen(
        "node_type = \"work\"",
        "node_type = \"work\"\ntags_element_pointer = \"/subject\"",
        1,
    );
    assert!(matches!(
        ValidatedPackage::from_toml_bytes(package_src.as_bytes()),
        Err(crate::ImportError::JsonEngine(_))
    ));
}
