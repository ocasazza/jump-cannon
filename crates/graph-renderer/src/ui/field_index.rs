//! Inverted-index helper for active-filter chips. Built once from the
//! server's `/graph/meta_summary` payload; queried per-frame to fold the
//! UI's active (field, value) selections into a node-idx set.
//!
//! Semantics: within-field OR (the union of all value-buckets selected
//! for that field), across-field AND (intersect each field's union).

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

use crate::proto;
use crate::ui::query::ActiveFieldFilters;

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
            let mut union: HashSet<u32> = HashSet::new();
            for v in values {
                if let Some(idxs) = buckets.get(v) {
                    union.extend(idxs.iter().copied());
                }
            }
            per_field.push(union);
        }
        if per_field.is_empty() { return None; }
        // Intersect smallest first.
        per_field.sort_by_key(|s| s.len());
        let mut iter = per_field.into_iter();
        let mut acc = iter.next().unwrap();
        for next in iter {
            acc.retain(|x| next.contains(x));
        }
        Some(acc)
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

