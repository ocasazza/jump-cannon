//! Focus-set computation. Given a focused node + a [`FocusMode`]
//! criterion, return the set of node indices that belong to the
//! "focused community" (always inclusive of the focused node itself).
//!
//! The render path multiplies a per-node `dim_alpha` over the regular
//! colors-buffer alpha, where in-set nodes stay at 1.0 and out-of-set
//! nodes drop to ~0.25; edges are alpha-multiplied based on whether
//! both / one / neither endpoint sit in the set.
//!
//! Step-1 ships `None`, `SameCommunityId`, and `SharedEdge`. The
//! `SharedTag` and `Filter` arms are stubbed (return only `{focused}`)
//! pending the vault `NodeMeta` cache landing in the renderer.

use std::collections::HashMap;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ui::query::QueryModel;

/// Membership criterion for the focused community.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FocusMode {
    /// Focus disabled — only the focused node lights up; community
    /// dimming is off (the renderer uses an empty member set).
    None,
    /// `metrics["community"][i] == focused community id`.
    #[default]
    SameCommunityId,
    /// Direct neighbors via the CSR adjacency (`edges` is `[s,t,…]`).
    SharedEdge,
    /// Any tag in `NodeMeta.tags` overlaps with the focused node's.
    /// Stub — returns `{focused}` until the renderer caches NodeMeta.
    SharedTag,
    /// Matches the active QueryModel selection. Stub for the same
    /// reason: the query path lives downstream of the focus pipeline.
    Filter,
}

impl FocusMode {
    pub const ALL: &'static [FocusMode] = &[
        FocusMode::None,
        FocusMode::SameCommunityId,
        FocusMode::SharedEdge,
        FocusMode::SharedTag,
        FocusMode::Filter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FocusMode::None => "None (single node)",
            FocusMode::SameCommunityId => "Same community id",
            FocusMode::SharedEdge => "Shared edge",
            FocusMode::SharedTag => "Shared tag",
            FocusMode::Filter => "Active filter",
        }
    }

    /// Step-1 ships `None`, `SameCommunityId`, `SharedEdge`. The other
    /// two are surfaced in the UI but disabled with a tooltip.
    pub fn enabled(self) -> bool {
        // All modes enabled now that the field_index plumb-through lands.
        true
    }
}

/// Borrow bag for [`compute`]. Carries the ambient data the criteria
/// need — passed by reference so we never copy a 100k metric vec.
pub struct FocusCtx<'a> {
    pub n_nodes: u32,
    /// `metrics["community"]` etc. Per-metric f32 vec of length n_nodes.
    pub metrics: &'a HashMap<String, Vec<f32>>,
    /// CSR-flat edge list `[src,tgt, src,tgt, …]`.
    pub edges: &'a [u32],
    /// Vault-side per-node meta. Step 1 leaves this `None`; SharedTag
    /// will read it once the renderer caches the proto NodeMeta.
    pub node_meta: Option<&'a HashMap<u32, crate::proto::NodeMeta>>,
    /// Active query model — Filter mode pulls its membership from here
    /// once the wiring lands.
    pub query: Option<&'a QueryModel>,
    /// Inverted index for active-filter lookups + SharedTag fallback.
    pub field_index: Option<&'a crate::ui::field_index::FieldIndex>,
}

/// Return the node-index set that belongs to the focused community
/// under the supplied criterion. The focused node itself is always
/// included. Returns an empty set when `mode == FocusMode::None`
/// — callers treat the empty set as "single-node focus, no community
/// dimming" (the renderer still highlights the one node).
pub fn compute(focused_idx: u32, mode: FocusMode, ctx: &FocusCtx<'_>) -> HashSet<u32> {
    let mut set = HashSet::new();
    if focused_idx >= ctx.n_nodes {
        return set;
    }
    match mode {
        FocusMode::None => {
            set.insert(focused_idx);
            set
        }
        FocusMode::SameCommunityId => {
            set.insert(focused_idx);
            let Some(comm) = ctx.metrics.get("community") else {
                return set;
            };
            let i = focused_idx as usize;
            let Some(&target) = comm.get(i) else {
                return set;
            };
            for (j, &v) in comm.iter().enumerate() {
                if (v - target).abs() < 0.5 {
                    set.insert(j as u32);
                }
            }
            set
        }
        FocusMode::SharedEdge => {
            set.insert(focused_idx);
            let f = focused_idx;
            for chunk in ctx.edges.chunks_exact(2) {
                let s = chunk[0];
                let t = chunk[1];
                if s == f {
                    set.insert(t);
                } else if t == f {
                    set.insert(s);
                }
            }
            set
        }
        FocusMode::SharedTag => {
            set.insert(focused_idx);
            // Reverse-lookup via field_index: union every `tags` bucket
            // that contains `focused_idx`.
            if let Some(fi) = ctx.field_index {
                if let Some(buckets) = fi.by_field.get("tags") {
                    for (_, idxs) in buckets {
                        if idxs.binary_search(&focused_idx).is_ok() {
                            for &i in idxs { set.insert(i); }
                        }
                    }
                }
            }
            set
        }
        FocusMode::Filter => {
            set.insert(focused_idx);
            if let (Some(fi), Some(q)) = (ctx.field_index, ctx.query) {
                if let Some(matched) = fi.matches(&q.active_filters) {
                    for &i in &matched { set.insert(i); }
                }
            }
            set
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_returns_just_focused() {
        let ctx = FocusCtx {
            n_nodes: 4,
            metrics: &HashMap::new(),
            edges: &[],
            node_meta: None,
            query: None,
            field_index: None,
        };
        let s = compute(2, FocusMode::None, &ctx);
        assert_eq!(s.len(), 1);
        assert!(s.contains(&2));
    }

    #[test]
    fn shared_edge_finds_neighbors() {
        let edges = vec![0, 1, 0, 2, 1, 3, 2, 3];
        let ctx = FocusCtx {
            n_nodes: 4,
            metrics: &HashMap::new(),
            edges: &edges,
            node_meta: None,
            query: None,
            field_index: None,
        };
        let s = compute(0, FocusMode::SharedEdge, &ctx);
        assert!(s.contains(&0));
        assert!(s.contains(&1));
        assert!(s.contains(&2));
        assert!(!s.contains(&3));
    }
}
