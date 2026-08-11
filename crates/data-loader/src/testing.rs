//! Shared importer contract test harness.
//!
//! [`assert_import_contract`] is the single entry point every importer's test
//! suite invokes to prove it satisfies the unified identity/search contract:
//!
//! 1. The descriptor validates (discovery schema version 2, declared
//!    `source_kind`, core fields, capability agreement).
//! 2. Two consecutive imports succeed and produce the **identical ordered
//!    node-ID set** and **byte-identical search documents** (compared in
//!    `node_id` order so filesystem walk order cannot flake the check).
//! 3. Every import satisfies [`crate::ImporterSchema::validate_output`] —
//!    namespace conformance (`{source_kind}:{source_id}:{local}`) and fully
//!    resolved edge endpoints.

use crate::Importer;

/// Assert the unified identity/search contract for one importer.
///
/// Panics with a descriptive message on any violation; call it from a
/// `#[tokio::test]` in each importer crate.
pub async fn assert_import_contract(importer: &dyn Importer) {
    let descriptor = importer.descriptor();
    descriptor
        .validate()
        .expect("importer descriptor must satisfy the discovery contract");

    let first = importer.import().await.expect("first import must succeed");
    let second = importer.import().await.expect("second import must succeed");

    descriptor
        .schema
        .validate_result(&first)
        .expect("first import must satisfy validate_output");
    descriptor
        .schema
        .validate_result(&second)
        .expect("second import must satisfy validate_output");

    let mut first_ids: Vec<&String> = first.graph.nodes.keys().collect();
    let mut second_ids: Vec<&String> = second.graph.nodes.keys().collect();
    first_ids.sort();
    second_ids.sort();
    assert_eq!(
        first_ids, second_ids,
        "re-import must produce the identical ordered node-ID set"
    );

    let mut first_documents = first.search_documents;
    let mut second_documents = second.search_documents;
    first_documents.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    second_documents.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    assert_eq!(
        first_documents, second_documents,
        "re-import must produce byte-identical search documents"
    );
}
