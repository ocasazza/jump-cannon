//! Inverted-index helper for active-filter chips. Built once from the
//! server's `/graph/meta_summary` payload; queried per-frame to fold the
//! UI's active (field, value) selections into a node-idx set.
//!
//! Semantics: each field combines its selected value-buckets according
//! to its per-field [`Combinator`] (default `Any` = OR/union, `All` =
//! AND/intersection). Field-level results combine according to the
//! filter set's `cross_field_combinator` (default `All` = AND).

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

use crate::proto;
use crate::ui::query::{ActiveFieldFilters, Combinator};

#[derive(Debug, Default, Clone)]
pub struct FieldIndex {
    /// field -> value -> sorted `Vec<u32>` of node indices.
    pub by_field: HashMap<String, HashMap<String, Vec<u32>>>,
}

impl FieldIndex {
    /// Decode a [`proto::MetaSummary`] into a FieldIndex. `_ids` is
    /// passed for symmetry with the spec; current decoder doesn't need
    /// it because the server already produces dense indices.
    pub fn from_proto(p: &proto::MetaSummary, _ids: &[String]) -> Self {
        let mut by_field: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
        for b in &p.buckets {
            let field = match p.fields.get(b.field_idx as usize) {
                Some(s) => s.clone(),
                None => continue,
            };
            let mut v = b.node_idx.clone();
            v.sort_unstable();
            v.dedup();
            by_field.entry(field).or_default().insert(b.value.clone(), v);
        }
        Self { by_field }
    }

    /// `Some(set)` when at least one filter is set AND at least one
    /// (field, value) pair resolves to a known bucket; `None` otherwise
    /// (= no filter active).
    pub fn matches(&self, filters: &ActiveFieldFilters) -> Option<HashSet<u32>> {
        if filters.by_field.is_empty() {
            return None;
        }
        let mut per_field: Vec<HashSet<u32>> = Vec::new();
        for (field, values) in &filters.by_field {
            if values.is_empty() { continue; }
            let Some(buckets) = self.by_field.get(field) else { continue };
            let combinator = filters.combinator_for(field);
            // Resolve each selected value to its bucket. Skip unknown
            // values rather than treating them as the empty set —
            // otherwise stale persisted state would tank intersections.
            let mut buckets_for_field: Vec<&Vec<u32>> = Vec::new();
            for v in values {
                if let Some(idxs) = buckets.get(v) {
                    buckets_for_field.push(idxs);
                }
            }
            if buckets_for_field.is_empty() { continue; }
            let combined: HashSet<u32> = match combinator {
                Combinator::Any => {
                    let mut union: HashSet<u32> = HashSet::new();
                    for b in &buckets_for_field {
                        union.extend(b.iter().copied());
                    }
                    union
                }
                Combinator::All => {
                    // Intersect smallest first.
                    let mut sorted = buckets_for_field.clone();
                    sorted.sort_by_key(|b| b.len());
                    let mut acc: HashSet<u32> = sorted[0].iter().copied().collect();
                    for b in sorted.iter().skip(1) {
                        let bset: HashSet<u32> = b.iter().copied().collect();
                        acc.retain(|x| bset.contains(x));
                    }
                    acc
                }
            };
            per_field.push(combined);
        }
        if per_field.is_empty() { return None; }
        let acc: HashSet<u32> = match filters.cross_field_combinator {
            Combinator::All => {
                // Intersect smallest first.
                per_field.sort_by_key(|s| s.len());
                let mut iter = per_field.into_iter();
                let mut acc = iter.next().unwrap();
                for next in iter {
                    acc.retain(|x| next.contains(x));
                }
                acc
            }
            Combinator::Any => {
                let mut acc: HashSet<u32> = HashSet::new();
                for s in per_field {
                    acc.extend(s.into_iter());
                }
                acc
            }
        };
        Some(acc)
    }

    /// Build a per-node categorical f32 metric where each node's value is
    /// the bucket id derived from its **primary tag** — the first tag in
    /// the node's `tags` set after sorting the tag strings
    /// lexicographically (case-sensitive byte order, the same order
    /// `BTreeMap` iteration would yield). This is the deterministic
    /// tiebreaker for nodes with multiple tags.
    ///
    /// The bucket id is `hash(primary_tag) as u32` truncated to non-negative
    /// f32-representable range (the palette consumer `% palette.len()`
    /// cycles it anyway). Untagged nodes get bucket `0`.
    ///
    /// Returned vec has length `n_nodes`. Returns `None` if the index
    /// has no `tags` field at all (no node carries any tag).
    pub fn tag_primary_metric(&self, n_nodes: usize) -> Option<Vec<f32>> {
        let tags = self.by_field.get("tags")?;
        // Walk the (value -> [node_idx]) buckets in **sorted value order**
        // so the first bucket that claims a node defines that node's
        // primary tag — matching the "first sorted tag" tiebreaker.
        let mut sorted: Vec<(&String, &Vec<u32>)> = tags.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = vec![0.0_f32; n_nodes];
        let mut assigned = vec![false; n_nodes];
        for (value, idxs) in sorted {
            // Hash the tag string to a u32 bucket id. Default Hasher is
            // non-portable across rustc versions but stable within a
            // process run — fine, we never persist the metric.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            value.hash(&mut h);
            let bucket = (h.finish() as u32) as f32;
            for &i in idxs {
                let i = i as usize;
                if i < n_nodes && !assigned[i] {
                    out[i] = bucket;
                    assigned[i] = true;
                }
            }
        }
        Some(out)
    }

    pub fn values_for<'a>(&'a self, field: &str) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match self.by_field.get(field) {
            Some(map) => Box::new(map.keys().map(|s| s.as_str())),
            None => Box::new(std::iter::empty()),
        }
    }

    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.by_field.keys().map(|s| s.as_str())
    }
}

/// Trivial-fixture helper for tests — build a FieldIndex from a
/// nested map literal without going through proto encode/decode.
#[cfg(test)]
pub fn from_fixture(m: BTreeMap<&str, BTreeMap<&str, Vec<u32>>>) -> FieldIndex {
    let mut by_field: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
    for (field, vmap) in m {
        let mut h: HashMap<String, Vec<u32>> = HashMap::new();
        for (val, mut v) in vmap {
            v.sort_unstable();
            v.dedup();
            h.insert(val.to_string(), v);
        }
        by_field.insert(field.to_string(), h);
    }
    FieldIndex { by_field }
}

