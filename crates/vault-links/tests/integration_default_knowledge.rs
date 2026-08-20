use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;
use vault_links::extract_vault;

#[test]
fn default_knowledge_base_is_connected_and_resolved() {
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../charts/jump-cannon/knowledge");
    let temp = TempDir::new().expect("create temporary vault");
    let managed = temp.path().join("Jump Cannon");
    fs::create_dir(&managed).expect("create managed knowledge directory");

    for entry in fs::read_dir(&source).expect("read chart knowledge directory") {
        let entry = entry.expect("read knowledge entry");
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("md") {
            fs::copy(entry.path(), managed.join(entry.file_name())).expect("copy knowledge note");
        }
    }
    fs::write(
        temp.path().join("Welcome.md"),
        "Begin with [[Start Here]]. See [[Performance]].\n",
    )
    .expect("write Welcome note");
    fs::write(
        temp.path().join("Performance.md"),
        "See [[Performance Engineering]], [[Scheduled Tests]], and [[Observability]].\n",
    )
    .expect("write Performance note");

    let result = extract_vault(temp.path());
    assert_eq!(
        result.graph.node_count(),
        39,
        "unexpected default node count"
    );
    assert!(
        result.unresolved.is_empty(),
        "unresolved default links: {:?}",
        result.unresolved
    );

    for required in [
        "Jump Cannon/Start Here",
        "Jump Cannon/Use Jump Cannon",
        "Jump Cannon/Development",
        "Jump Cannon/Operations",
        "Jump Cannon/Compute",
        "Jump Cannon/Agent Workflow",
    ] {
        let node_id = format!("obsidian:obsidian:{required}");
        assert!(
            result.graph.nodes.contains_key(&node_id),
            "missing default branch {node_id}"
        );
    }

    let mut outgoing = std::collections::HashMap::<&str, Vec<&str>>::new();
    for edge in &result.graph.edges {
        outgoing
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
    }
    let mut visited = HashSet::new();
    let mut queue = VecDeque::from(["obsidian:obsidian:Jump Cannon/Start Here"]);
    while let Some(id) = queue.pop_front() {
        if !visited.insert(id) {
            continue;
        }
        if let Some(targets) = outgoing.get(id) {
            queue.extend(targets.iter().copied());
        }
    }

    let unreachable: Vec<_> = result
        .graph
        .nodes
        .keys()
        .filter(|id| !visited.contains(id.as_str()))
        .cloned()
        .collect();
    assert!(
        unreachable.is_empty(),
        "default notes not reachable from Start Here: {unreachable:?}"
    );
}
