use super::{
    loader::ObsidianLoader,
    parser::{extract_wikilinks, parse_note},
};
use data_loader::{ImportError, Loader};
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
