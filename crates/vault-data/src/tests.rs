use crate::*;
use std::collections::HashMap;

#[test]
fn unit_node_meta_serde_roundtrip() {
    let meta = NodeMeta {
        title: "Test Note".into(),
        tags: vec!["it-ops/type/runbook".into()],
        frontmatter: HashMap::from([
            ("status".into(), serde_json::Value::String("active".into())),
        ]),
        mtime: 1700000000,
        path: "vault/30-Knowledge-Base/test.md".into(),
        doctype: Some("runbook".into()),
        folder: "30-Knowledge-Base".into(),
    };
    let json = serde_json::to_string(&meta).unwrap();
    let back: NodeMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(back.title, meta.title);
    assert_eq!(back.tags, meta.tags);
    assert_eq!(back.mtime, meta.mtime);
}

#[test]
fn unit_vault_graph_add_and_count() {
    let mut g = VaultGraph::new();
    g.add_node(VaultNode { id: "a".into(), ..Default::default() });
    g.add_node(VaultNode { id: "b".into(), ..Default::default() });
    g.add_edge(VaultEdge { source: "a".into(), target: "b".into() });
    assert_eq!(g.node_count(), 2);
    assert_eq!(g.edge_count(), 1);
}

#[test]
fn unit_vault_graph_serde_roundtrip() {
    let mut g = VaultGraph::new();
    g.add_node(VaultNode { id: "x".into(), ..Default::default() });
    g.density = 0.5;
    let json = serde_json::to_string(&g).unwrap();
    let back: VaultGraph = serde_json::from_str(&json).unwrap();
    assert!(back.nodes.contains_key("x"));
    assert!((back.density - 0.5).abs() < 1e-9);
}

#[test]
fn unit_field_schema_serde_roundtrip() {
    let schema = FieldSchema {
        name: "status".into(),
        field_type: FieldType::Select(vec!["active".into(), "archived".into()]),
        required: true,
        description: None,
    };
    let json = serde_json::to_string(&schema).unwrap();
    let back: FieldSchema = serde_json::from_str(&json).unwrap();
    assert_eq!(back.name, schema.name);
    assert_eq!(back.required, schema.required);
}

#[test]
fn unit_community_color_wraps() {
    let c0 = color::community_color(0);
    let c20 = color::community_color(20);
    assert_eq!(c0, c20); // palette wraps at 20
}
