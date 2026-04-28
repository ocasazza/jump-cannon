use std::fs;
use tempfile::TempDir;
use vault_links::extract_vault;

#[test]
fn integration_extract_small_vault() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    fs::write(
        root.join("Note A.md"),
        "---\ntags:\n  - test\n---\n\nSee [[Note B]].\n",
    )
    .unwrap();
    fs::write(root.join("Note B.md"), "Links back to [[Note A]].\n").unwrap();

    // Should be excluded:
    fs::create_dir_all(root.join(".obsidian")).unwrap();
    fs::write(root.join(".obsidian/config.json"), "{}").unwrap();

    let result = extract_vault(root);
    let g = &result.graph;
    assert_eq!(g.node_count(), 2, "should have 2 nodes");
    assert_eq!(g.edge_count(), 2, "should have 2 edges (A→B and B→A)");
    // .obsidian files should not appear as nodes
    assert!(!g.nodes.contains_key(".obsidian/config"));
}
