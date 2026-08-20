use super::{
    loader::ObsidianLoader,
    parser::{extract_wikilinks, parse_note},
};
use data_loader::{ImportError, Loader, TagHierarchySchema};
use std::{fs, path::Path};

#[test]
fn unit_extract_wikilinks_basic() {
    let text = "See [[Some Page]] and [[Other|alias]] and [[Heading#Section]].";
    let links = extract_wikilinks(text);
    assert_eq!(links, vec!["Some Page", "Other", "Heading"]);
}

#[test]
fn unit_extract_wikilinks_empty() {
    assert!(extract_wikilinks("no links here").is_empty());
}

#[test]
fn unit_parse_note_frontmatter() {
    let text =
        "---\ntags:\n  - it-ops/type/runbook\ndoctype: runbook\n---\n\nSee [[Linked Page]].\n";
    let note = parse_note(Path::new("vault/test.md"), text);
    assert_eq!(note.doctype.as_deref(), Some("runbook"));
    assert!(note.tags.contains(&"it-ops/type/runbook".to_string()));
    assert!(note.links.contains(&"Linked Page".to_string()));
}

#[test]
fn unit_parse_note_no_frontmatter() {
    let text = "Just a [[wikilink]] here.";
    let note = parse_note(Path::new("vault/bare.md"), text);
    assert!(note.tags.is_empty());
    assert_eq!(note.links, vec!["wikilink"]);
}

// Real-world Obsidian tag shapes the legacy parser silently dropped.
// Caught by the user: "WHERE ARE THE FUCKIN BADGES IN THE RIGHT HAND
// SIDEBAR FOR PAGE TAGS — OBSIDIAN PAGES HAVE TAGS — SHOW THEM AS
// BADGES." Root cause: `tags:` was only accepted as a YAML Array.

#[test]
fn unit_parse_note_tags_inline_array() {
    let text = "---\ntags: [alpha, beta]\n---\nbody\n";
    let note = parse_note(Path::new("vault/a.md"), text);
    assert!(note.tags.contains(&"alpha".to_string()));
    assert!(note.tags.contains(&"beta".to_string()));
}

#[test]
fn unit_parse_note_tags_comma_string() {
    let text = "---\ntags: alpha, beta, gamma\n---\nbody\n";
    let note = parse_note(Path::new("vault/b.md"), text);
    assert!(note.tags.contains(&"alpha".to_string()));
    assert!(note.tags.contains(&"beta".to_string()));
    assert!(note.tags.contains(&"gamma".to_string()));
}

#[test]
fn unit_parse_note_tags_single_scalar() {
    let text = "---\ntags: alpha\n---\nbody\n";
    let note = parse_note(Path::new("vault/c.md"), text);
    assert_eq!(note.tags, vec!["alpha".to_string()]);
}

#[test]
fn unit_parse_note_tag_singular_alias() {
    let text = "---\ntag: solo\n---\nbody\n";
    let note = parse_note(Path::new("vault/d.md"), text);
    assert_eq!(note.tags, vec!["solo".to_string()]);
}

#[test]
fn unit_parse_note_tags_strip_hash_and_quotes() {
    let text = "---\ntags: [\"#hashed\", '#quoted', plain]\n---\nbody\n";
    let note = parse_note(Path::new("vault/e.md"), text);
    assert!(note.tags.contains(&"hashed".to_string()));
    assert!(note.tags.contains(&"quoted".to_string()));
    assert!(note.tags.contains(&"plain".to_string()));
}

#[test]
fn unit_parse_note_tags_inline_body_hash_tokens() {
    let text = "no frontmatter.\n\nMentioning #project-x and #ops/runbook here.\n";
    let note = parse_note(Path::new("vault/f.md"), text);
    assert!(note.tags.contains(&"project-x".to_string()));
    assert!(note.tags.contains(&"ops/runbook".to_string()));
}

#[test]
fn unit_parse_note_tags_inline_skips_headings_and_fenced_code() {
    let text = "# Heading not a tag\n\n```\n#code-not-a-tag\n```\n\nReal #tag-here.\n";
    let note = parse_note(Path::new("vault/g.md"), text);
    assert!(note.tags.contains(&"tag-here".to_string()));
    assert!(!note.tags.contains(&"code-not-a-tag".to_string()));
    assert!(!note.tags.contains(&"Heading".to_string()));
}

#[test]
fn unit_parse_note_tags_inline_skips_numeric() {
    // `#1`, `#42` are usually issue numbers, not Obsidian tags. Require
    // at least one alphabetic char in the body-tag tokenizer.
    let text = "body with #1 and #42 and #v2-real.\n";
    let note = parse_note(Path::new("vault/h.md"), text);
    assert!(note.tags.contains(&"v2-real".to_string()));
    assert!(!note.tags.iter().any(|t| t == "1" || t == "42"));
}

#[test]
fn unit_parse_note_tags_dedup_across_sources() {
    // Same tag in frontmatter array AND inline body → appears once.
    let text = "---\ntags: [alpha]\n---\n\nAlso mentions #alpha here.\n";
    let note = parse_note(Path::new("vault/i.md"), text);
    let count = note.tags.iter().filter(|t| *t == "alpha").count();
    assert_eq!(count, 1, "expected dedup, got {:?}", note.tags);
}

#[test]
fn obsidian_schema_and_search_documents_satisfy_the_contract() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(
        fixture.path().join("page.md"),
        "---\ntags: [runbook]\ndescription: Recovery steps\nstatus: active\n---\nBody token.\n",
    )
    .unwrap();
    let loader = ObsidianLoader::new(fixture.path());
    let descriptor = data_loader::Importer::descriptor(&loader);
    let schema = loader.schema();
    let result = loader.load();

    descriptor.validate().unwrap();
    assert_eq!(descriptor.schema, schema);
    schema.validate_result(&result).unwrap();
    let keys = schema
        .searchable_fields()
        .map(|field| field.key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"body"));
    assert!(keys.contains(&"description"));
    assert!(keys.contains(&"tags"));
    assert_eq!(schema.tag_hierarchy, TagHierarchySchema::slash());

    let document = result.search_documents.first().unwrap();
    assert_eq!(document.fields["tags"], serde_json::json!(["runbook"]));
    assert_eq!(document.fields["body"], "Body token.\n");
    assert_eq!(document.fields["description"], "Recovery steps");
}

#[test]
fn obsidian_try_load_rejects_a_missing_vault() {
    let fixture = tempfile::tempdir().unwrap();
    let missing = fixture.path().join("missing");
    let error = ObsidianLoader::new(&missing).try_load().unwrap_err();

    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("missing"));
}

#[test]
fn obsidian_try_load_rejects_a_non_directory_vault() {
    let fixture = tempfile::tempdir().unwrap();
    let file = fixture.path().join("vault.txt");
    fs::write(&file, "not a vault").unwrap();

    let error = ObsidianLoader::new(&file).try_load().unwrap_err();

    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("not a directory"));
}

#[test]
fn obsidian_try_load_rejects_an_unreadable_note_instead_of_publishing_partial_data() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("valid.md"), "Valid body.\n").unwrap();
    fs::write(fixture.path().join("invalid.md"), [0xff, 0xfe, 0xfd]).unwrap();

    let error = ObsidianLoader::new(fixture.path()).try_load().unwrap_err();

    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("invalid.md"));
}

#[test]
fn obsidian_try_load_rejects_malformed_frontmatter() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(
        fixture.path().join("invalid.md"),
        "---\ntags: [unterminated\n---\nBody.\n",
    )
    .unwrap();

    let error = ObsidianLoader::new(fixture.path()).try_load().unwrap_err();

    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("parse YAML frontmatter"));
    assert!(error.to_string().contains("invalid.md"));
}

#[tokio::test]
async fn obsidian_importer_satisfies_the_shared_import_contract() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(
        fixture.path().join("alpha.md"),
        "---\ntags: [runbook]\n---\nSee [[beta]].\n",
    )
    .unwrap();
    fs::write(fixture.path().join("beta.md"), "Target body.\n").unwrap();

    let loader = ObsidianLoader::new(fixture.path());
    data_loader::testing::assert_import_contract(&loader).await;
}

#[test]
fn obsidian_ids_and_content_hashes_are_golden() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(
        fixture.path().join("page.md"),
        "---\ntags: [runbook]\n---\nGolden body.\n",
    )
    .unwrap();
    fs::write(fixture.path().join("linked.md"), "Target.\n").unwrap();
    fs::write(fixture.path().join("index.md"), "See [[linked]].\n").unwrap();

    let result = ObsidianLoader::new(fixture.path()).load();

    // Node IDs are exactly `{source_kind}:{source_id}:{vault-relative path}`.
    let page = result
        .graph
        .nodes
        .get("obsidian:obsidian:page")
        .expect("namespaced page node");
    assert_eq!(page.meta.source_id, "obsidian");
    assert_eq!(page.meta.path, "page");

    // The content hash covers the raw file bytes, truncated SHA-256.
    let document = result
        .search_documents
        .iter()
        .find(|document| document.node_id == "obsidian:obsidian:page")
        .expect("page document");
    assert_eq!(
        document.fields["content_hash"],
        "h256:80e57bef7e76518579e1f8d9c21a824e"
    );

    // Wikilink resolution emits namespaced endpoints.
    let edge = result
        .graph
        .edges
        .iter()
        .find(|edge| edge.source == "obsidian:obsidian:index")
        .expect("index edge");
    assert_eq!(edge.target, "obsidian:obsidian:linked");
}

#[test]
fn extract_notes_matches_the_on_disk_vault_byte_for_byte() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir_all(fixture.path().join("Knowledge")).unwrap();
    fs::write(
        fixture.path().join("Knowledge/Start Here.md"),
        "---\ntags: [runbook]\ndoctype: guide\n---\nSee [[Deep Note]].\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("Knowledge/Deep Note.md"),
        "Back to [[Start Here]].\n",
    )
    .unwrap();
    fs::write(fixture.path().join("root.md"), "Root body.\n").unwrap();

    let on_disk = crate::try_extract_vault(fixture.path()).unwrap();
    let files: Vec<(String, String)> = [
        "Knowledge/Start Here.md",
        "Knowledge/Deep Note.md",
        "root.md",
    ]
    .iter()
    .map(|rel| {
        (
            rel.to_string(),
            fs::read_to_string(fixture.path().join(rel)).unwrap(),
        )
    })
    .collect();
    let in_memory = crate::try_extract_notes(&files).unwrap();

    let disk_ids: std::collections::BTreeSet<_> = on_disk.graph.nodes.keys().collect();
    let memory_ids: std::collections::BTreeSet<_> = in_memory.graph.nodes.keys().collect();
    assert_eq!(disk_ids, memory_ids);

    let disk_edges: std::collections::BTreeSet<_> = on_disk
        .graph
        .edges
        .iter()
        .map(|edge| (&edge.source, &edge.target))
        .collect();
    let memory_edges: std::collections::BTreeSet<_> = in_memory
        .graph
        .edges
        .iter()
        .map(|edge| (&edge.source, &edge.target))
        .collect();
    assert_eq!(disk_edges, memory_edges);

    // Folder + title + search-document fields survive the transport change.
    let note = in_memory
        .graph
        .nodes
        .get("obsidian:obsidian:Knowledge/Start Here")
        .expect("namespaced in-memory node");
    assert_eq!(note.meta.folder, "Knowledge");
    assert_eq!(note.meta.mtime, 0);
    let document = in_memory
        .search_documents
        .iter()
        .find(|document| document.node_id == "obsidian:obsidian:Knowledge/Start Here")
        .expect("in-memory search document");
    assert_eq!(document.fields["folder"], "Knowledge");
    assert_eq!(document.fields["type"], "guide");
}

#[test]
fn extract_notes_applies_the_canonical_exclusions() {
    let files: Vec<(String, String)> = vec![
        ("page.md".to_string(), "Body.\n".to_string()),
        (".obsidian/config-note.md".to_string(), "Hidden.\n".to_string()),
        ("Ink/sketch.md".to_string(), "Hidden.\n".to_string()),
        ("canvas.canvas".to_string(), "{}".to_string()),
    ];
    let result = crate::extract_notes(&files);
    let ids: std::collections::BTreeSet<&str> = result
        .graph
        .nodes
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(ids.len(), 1);
    assert!(ids.contains("obsidian:obsidian:page"));
}

#[test]
fn try_extract_notes_rejects_malformed_frontmatter() {
    let files: Vec<(String, String)> = vec![(
        "invalid.md".to_string(),
        "---\ntags: [unterminated\n---\nBody.\n".to_string(),
    )];
    assert!(crate::try_extract_notes(&files).is_err());
    // The best-effort variant strips the metadata instead of failing.
    let result = crate::extract_notes(&files);
    assert_eq!(result.graph.nodes.len(), 1);
}
