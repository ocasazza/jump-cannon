use super::parser::{extract_wikilinks, parse_note};
use std::path::Path;

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
