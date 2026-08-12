//! Pure, store-independent three-way merge over graph snapshots.
//!
//! Nodes merge attribute-by-attribute: each node is serialized to a
//! `serde_json::Value` object and every top-level key is merged three-way
//! (recursing into nested objects such as `meta` and `meta.frontmatter`), so
//! independent edits to different frontmatter keys on both sides auto-merge.
//! When both sides change the same attribute differently the whole node is
//! recorded as a [`Conflict`] and the merged snapshot keeps the `ours` value,
//! so publication never blocks.
//!
//! Edges carry no attributes, so edge merging is set-based: an edge added on
//! either side is present, an edge deleted on one side and unchanged on the
//! other is absent, and add/add dedupes by [`EdgeId`]. No edge conflicts can
//! arise in this model.

use crate::model::{Conflict, GraphOp, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use vault_data::{EdgeId, VaultNode};

/// The full node/edge state of a graph at one commit.
///
/// `BTreeMap`/`BTreeSet` give a deterministic iteration order, which makes
/// [`Snapshot::canonical_json`] stable for content addressing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub nodes: BTreeMap<NodeId, VaultNode>,
    pub edges: BTreeSet<EdgeId>,
}

/// `VaultNode` has no `PartialEq`, so snapshots compare by canonical JSON.
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_json() == other.canonical_json()
    }
}

impl Snapshot {
    /// Apply user-facing ops to this snapshot, returning the new state.
    pub fn apply(&self, ops: &[GraphOp]) -> Snapshot {
        let mut next = self.clone();
        for op in ops {
            match op {
                GraphOp::UpsertNode(node) => {
                    next.nodes.insert(NodeId(node.id.clone()), node.clone());
                }
                GraphOp::DeleteNode(id) => {
                    next.nodes.remove(id);
                }
                GraphOp::UpsertEdge(edge) => {
                    next.edges.insert(EdgeId::from(edge));
                }
                GraphOp::DeleteEdge(id) => {
                    next.edges.remove(id);
                }
            }
        }
        next
    }

    /// Canonical JSON used for content addressing.
    ///
    /// Round-tripping through `serde_json::Value` sorts object keys (the
    /// workspace does not enable serde_json's `preserve_order`), so the hash
    /// is stable across processes even though `VaultNode` frontmatter is a
    /// `HashMap`.
    pub fn canonical_json(&self) -> String {
        canonical_json(self)
    }
}

/// Canonical JSON for any serializable value: object keys are sorted by the
/// `Value` round-trip, so the output is deterministic.
pub(crate) fn canonical_json<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    serde_json::to_string(&value).unwrap_or_default()
}

fn node_json(node: &VaultNode) -> serde_json::Value {
    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
}

/// The result of a three-way snapshot merge.
#[derive(Debug, Clone, Default)]
pub struct MergeOutcome {
    pub merged: Snapshot,
    pub conflicts: Vec<Conflict>,
}

enum ValueMerge {
    Clean(Option<serde_json::Value>),
    Conflict,
}

/// Three-way merge of optional JSON values. `None` means the key/node is
/// absent on that side.
fn merge_value(
    base: Option<&serde_json::Value>,
    ours: Option<&serde_json::Value>,
    theirs: Option<&serde_json::Value>,
) -> ValueMerge {
    if ours == theirs {
        // Both sides agree (including both-absent and both-deleted).
        return ValueMerge::Clean(ours.cloned());
    }
    if ours == base {
        // Only theirs changed (ours may be absent = deleted).
        return ValueMerge::Clean(theirs.cloned());
    }
    if theirs == base {
        // Only ours changed.
        return ValueMerge::Clean(ours.cloned());
    }
    // Both sides changed differently. Recurse into objects so independent
    // edits to different keys still auto-merge.
    if let (Some(serde_json::Value::Object(b)), Some(serde_json::Value::Object(o)), Some(serde_json::Value::Object(t))) =
        (base, ours, theirs)
    {
        let mut merged = serde_json::Map::new();
        let keys: BTreeSet<&String> = b.keys().chain(o.keys()).chain(t.keys()).collect();
        for key in keys {
            match merge_value(b.get(key), o.get(key), t.get(key)) {
                ValueMerge::Clean(Some(value)) => {
                    merged.insert(key.clone(), value);
                }
                ValueMerge::Clean(None) => {}
                ValueMerge::Conflict => return ValueMerge::Conflict,
            }
        }
        return ValueMerge::Clean(Some(serde_json::Value::Object(merged)));
    }
    ValueMerge::Conflict
}

/// Three-way merge of two divergent snapshots against their common ancestor.
pub fn merge_snapshots(base: &Snapshot, ours: &Snapshot, theirs: &Snapshot) -> MergeOutcome {
    let mut merged = Snapshot::default();
    let mut conflicts = Vec::new();

    let node_ids: BTreeSet<&NodeId> = base
        .nodes
        .keys()
        .chain(ours.nodes.keys())
        .chain(theirs.nodes.keys())
        .collect();
    for id in node_ids {
        let b = base.nodes.get(id).map(node_json);
        let o = ours.nodes.get(id).map(node_json);
        let t = theirs.nodes.get(id).map(node_json);
        match merge_value(b.as_ref(), o.as_ref(), t.as_ref()) {
            ValueMerge::Clean(Some(value)) => match serde_json::from_value::<VaultNode>(value) {
                Ok(node) => {
                    merged.nodes.insert(id.clone(), node);
                }
                // A clean attribute merge should always deserialize back into
                // a VaultNode; if it somehow does not, fall back to recording
                // a conflict and keeping ours rather than losing data.
                Err(_) => {
                    conflicts.push(conflict(id, base, ours, theirs));
                    if let Some(node) = ours.nodes.get(id) {
                        merged.nodes.insert(id.clone(), node.clone());
                    }
                }
            },
            ValueMerge::Clean(None) => {}
            ValueMerge::Conflict => {
                conflicts.push(conflict(id, base, ours, theirs));
                // Keep ours so the merged snapshot is always publishable.
                if let Some(node) = ours.nodes.get(id) {
                    merged.nodes.insert(id.clone(), node.clone());
                }
            }
        }
    }

    // Set-based edge merge: present iff kept by both sides, or added by one.
    merged.edges = ours
        .edges
        .intersection(&theirs.edges)
        .chain(ours.edges.difference(&base.edges))
        .chain(theirs.edges.difference(&base.edges))
        .cloned()
        .collect();

    MergeOutcome { merged, conflicts }
}

fn conflict(
    id: &NodeId,
    base: &Snapshot,
    ours: &Snapshot,
    theirs: &Snapshot,
) -> Conflict {
    Conflict {
        node_id: id.clone(),
        base: base.nodes.get(id).cloned(),
        ours: ours.nodes.get(id).cloned(),
        theirs: theirs.nodes.get(id).cloned(),
    }
}

/// The user-facing delta between two snapshots, in deterministic order.
pub fn diff_snapshots(a: &Snapshot, b: &Snapshot) -> Vec<GraphOp> {
    let mut ops = Vec::new();
    for (id, node) in &b.nodes {
        let changed = a
            .nodes
            .get(id)
            .map(|old| node_json(old) != node_json(node))
            .unwrap_or(true);
        if changed {
            ops.push(GraphOp::UpsertNode(node.clone()));
        }
    }
    for id in a.nodes.keys() {
        if !b.nodes.contains_key(id) {
            ops.push(GraphOp::DeleteNode(id.clone()));
        }
    }
    for edge in &b.edges {
        if !a.edges.contains(edge) {
            ops.push(GraphOp::UpsertEdge(vault_data::VaultEdge {
                source: edge.source.clone(),
                target: edge.target.clone(),
            }));
        }
    }
    for edge in &a.edges {
        if !b.edges.contains(edge) {
            ops.push(GraphOp::DeleteEdge(edge.clone()));
        }
    }
    ops
}
