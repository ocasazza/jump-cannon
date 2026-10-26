//! Algebraic laws of the three-way snapshot merge and diff.
//!
//! `PROPTEST_CASES` scales the run (`just test fuzz`).

use graph_vcs::model::NodeId;
use graph_vcs::{diff_snapshots, merge_snapshots, EdgeId, MergeOutcome, Snapshot, VaultNode};
use proptest::prelude::*;
use std::collections::BTreeMap;

const IDS: [&str; 4] = ["a", "b", "c", "d"];

fn node(id: &str, title: u8, frontmatter: &BTreeMap<u8, u8>, x: f32) -> VaultNode {
    let mut node = VaultNode { id: id.into(), x, ..Default::default() };
    node.meta.title = format!("t{title}");
    node.meta.frontmatter = frontmatter
        .iter()
        .map(|(k, v)| (format!("k{k}"), serde_json::json!(v)))
        .collect();
    node
}

/// Small id and value domains so independent edits collide often.
fn snapshot() -> impl Strategy<Value = Snapshot> {
    let attrs = (
        0u8..3,
        prop::collection::btree_map(0u8..3, 0u8..3, 0..3),
        prop_oneof![Just(0.0f32), Just(1.5f32)],
    );
    (
        prop::collection::btree_map(0usize..IDS.len(), attrs, 0..=IDS.len()),
        prop::collection::btree_set((0usize..IDS.len(), 0usize..IDS.len()), 0..6),
    )
        .prop_map(|(nodes, edges)| Snapshot {
            nodes: nodes
                .into_iter()
                .map(|(i, (title, fm, x))| (NodeId(IDS[i].into()), node(IDS[i], title, &fm, x)))
                .collect(),
            edges: edges
                .into_iter()
                .map(|(s, t)| EdgeId { source: IDS[s].into(), target: IDS[t].into() })
                .collect(),
        })
}

/// A descendant of `base`: some nodes replaced or deleted, edges optionally replaced.
fn edit(base: Snapshot) -> impl Strategy<Value = Snapshot> {
    (snapshot(), prop::collection::vec(any::<bool>(), IDS.len()), any::<bool>()).prop_map(
        move |(other, replace, replace_edges)| {
            let mut next = base.clone();
            for (id, _) in IDS.iter().zip(&replace).filter(|(_, replace)| **replace) {
                let key = NodeId((*id).into());
                match other.nodes.get(&key) {
                    Some(node) => next.nodes.insert(key, node.clone()),
                    None => next.nodes.remove(&key),
                };
            }
            if replace_edges {
                next.edges = other.edges.clone();
            }
            next
        },
    )
}

fn base_ours_theirs() -> impl Strategy<Value = (Snapshot, Snapshot, Snapshot)> {
    snapshot().prop_flat_map(|base| (Just(base.clone()), edit(base.clone()), edit(base)))
}

fn conflict_ids(outcome: &MergeOutcome) -> Vec<NodeId> {
    outcome.conflicts.iter().map(|c| c.node_id.clone()).collect()
}

fn json(node: Option<&VaultNode>) -> Option<serde_json::Value> {
    node.map(|n| serde_json::to_value(n).unwrap())
}

proptest! {
    #[test]
    fn unchanged_side_yields_the_other((base, ours, _) in base_ours_theirs()) {
        for outcome in [merge_snapshots(&base, &ours, &base), merge_snapshots(&base, &base, &ours)] {
            prop_assert!(outcome.conflicts.is_empty());
            prop_assert_eq!(&outcome.merged, &ours);
        }
    }

    #[test]
    fn identical_edits_merge_cleanly((base, ours, _) in base_ours_theirs()) {
        let outcome = merge_snapshots(&base, &ours, &ours);
        prop_assert!(outcome.conflicts.is_empty());
        prop_assert_eq!(&outcome.merged, &ours);
    }

    #[test]
    fn swapping_sides_preserves_conflicts((base, ours, theirs) in base_ours_theirs()) {
        let forward = merge_snapshots(&base, &ours, &theirs);
        let backward = merge_snapshots(&base, &theirs, &ours);
        prop_assert_eq!(conflict_ids(&forward), conflict_ids(&backward));
        if forward.conflicts.is_empty() {
            prop_assert_eq!(&forward.merged, &backward.merged);
        }
    }

    #[test]
    fn one_sided_node_changes_survive_merge((base, ours, theirs) in base_ours_theirs()) {
        let outcome = merge_snapshots(&base, &ours, &theirs);
        for id in IDS.map(|id| NodeId(id.into())) {
            let (b, o, t) = (json(base.nodes.get(&id)), json(ours.nodes.get(&id)), json(theirs.nodes.get(&id)));
            if o != b && t == b {
                prop_assert_eq!(json(outcome.merged.nodes.get(&id)), o);
            }
        }
    }

    #[test]
    fn applying_a_diff_reaches_its_target((base, target, _) in base_ours_theirs()) {
        prop_assert_eq!(&base.apply(&diff_snapshots(&base, &target)), &target);
        prop_assert!(diff_snapshots(&target, &target).is_empty());
    }
}
