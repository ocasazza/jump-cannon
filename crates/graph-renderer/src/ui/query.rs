//! Card-stream query model.
//!
//! The Filter sidebar section renders a horizontally-flowing strip of
//! `Card`s. The first card is always a system `Search` card and cannot
//! be deleted. The user appends `Filter`, `Connector` (AND/OR),
//! `ParenOpen`/`ParenClose`, and `Not` cards to build up a query.
//!
//! `QueryModel` is part of `AppState` and therefore persisted via
//! `eframe::Storage` (serde JSON) for free.
//!
//! The full evaluator (intersection / union per AND/OR, paren grouping,
//! NOT, search via the backend `/search?q=` proxy) is out of scope here.
//! `QueryModel::evaluate` is a stub that returns `None` (= no filter
//! applied) so the rest of the renderer keeps working unchanged.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Active per-field filter selections driven by badge clicks. Within a
/// single field, multiple values OR; across fields, AND.
///
/// `insertion_order` exists so the on-screen chip-strip can render
/// fields in the order the user added them (BTreeMap orders by name,
/// which would shuffle the strip every time a new field is touched).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveFieldFilters {
    pub by_field: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub insertion_order: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Op {
    /// `=`
    Eq,
    /// `≠`
    Neq,
    /// `~`
    Contains,
    /// `~/regex/`
    Matches,
}

impl Op {
    pub const ALL: &'static [Op] = &[Op::Eq, Op::Neq, Op::Contains, Op::Matches];

    pub fn label(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Neq => "≠",
            Op::Contains => "~",
            Op::Matches => "~/r/",
        }
    }

    pub fn cycle(self) -> Op {
        match self {
            Op::Eq => Op::Neq,
            Op::Neq => Op::Contains,
            Op::Contains => Op::Matches,
            Op::Matches => Op::Eq,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectorOp {
    And,
    Or,
}

impl ConnectorOp {
    pub fn label(self) -> &'static str {
        match self {
            ConnectorOp::And => "and",
            ConnectorOp::Or => "or",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Card {
    /// System search card; no delete button.
    Search { value: String, regex: bool },
    Filter {
        field: String,
        op: Op,
        value: String,
    },
    Connector { op: ConnectorOp },
    ParenOpen,
    ParenClose,
    Not,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryModel {
    pub cards: Vec<Card>,
    /// Badge-driven (field, value) toggles. Folded into evaluate() when
    /// a `FieldIndex` is available via `EvalContext::field_index`.
    #[serde(default)]
    pub active_filters: ActiveFieldFilters,
}

impl Default for QueryModel {
    fn default() -> Self {
        Self {
            // Always start with the system search card.
            cards: vec![Card::Search {
                value: String::new(),
                regex: false,
            }],
            active_filters: ActiveFieldFilters::default(),
        }
    }
}

impl QueryModel {
    /// Toggle inclusion of `(field, value)` in the active filter set.
    pub fn toggle_field_filter(&mut self, field: &str, value: &str) {
        let entry = self
            .active_filters
            .by_field
            .entry(field.to_string())
            .or_default();
        if entry.contains(value) {
            entry.remove(value);
            if entry.is_empty() {
                self.active_filters.by_field.remove(field);
                self.active_filters
                    .insertion_order
                    .retain(|f| f != field);
            }
        } else {
            entry.insert(value.to_string());
            if !self.active_filters.insertion_order.iter().any(|f| f == field) {
                self.active_filters.insertion_order.push(field.to_string());
            }
        }
    }

    pub fn clear_field(&mut self, field: &str) {
        self.active_filters.by_field.remove(field);
        self.active_filters.insertion_order.retain(|f| f != field);
    }

    pub fn clear_all_filters(&mut self) {
        self.active_filters = ActiveFieldFilters::default();
    }

    /// Returns true if `(field, value)` is currently selected.
    pub fn is_filter_active(&self, field: &str, value: &str) -> bool {
        self.active_filters
            .by_field
            .get(field)
            .map(|set| set.contains(value))
            .unwrap_or(false)
    }
}

/// Context passed to the evaluator. The query model resolves Search
/// cards through the cached results map (populated by App as async
/// `/search?q=` fetches complete). Filter cards are no-ops for now —
/// they hook into a per-field index in a later phase.
pub struct EvalContext<'a> {
    pub ids: &'a [String],
    pub id_to_idx: &'a HashMap<String, u32>,
    pub search_results: &'a HashMap<String, HashSet<u32>>,
    /// Optional inverted index used to resolve `active_filters` into a
    /// node-idx set. When `None`, badge-driven filters are dormant.
    pub field_index: Option<&'a crate::ui::field_index::FieldIndex>,
}

impl<'a> EvalContext<'a> {
    pub fn new(
        ids: &'a [String],
        id_to_idx: &'a HashMap<String, u32>,
        search_results: &'a HashMap<String, HashSet<u32>>,
    ) -> Self {
        Self {
            ids,
            id_to_idx,
            search_results,
            field_index: None,
        }
    }

    pub fn with_field_index(
        mut self,
        idx: Option<&'a crate::ui::field_index::FieldIndex>,
    ) -> Self {
        self.field_index = idx;
        self
    }
}

/// One leaf clause + its connector to the previous clause. Negation is
/// applied to the leaf result. Paren grouping is left for a later
/// pass — Phase G keeps a flat AND/OR fold.
#[derive(Debug, Clone)]
enum Leaf {
    /// A search term that resolved (or didn't yet) — `None` means
    /// the cache hasn't returned for this query, so we treat as
    /// "still loading" and skip rather than zero out the graph.
    Search(String),
    /// Filter cards are not yet evaluable (no field index).
    Unsupported,
}

#[derive(Debug, Clone)]
struct Clause {
    connector: ConnectorOp,
    negate: bool,
    leaf: Leaf,
}

impl QueryModel {
    /// Tokenize the card stream into a flat sequence of clauses joined
    /// by AND/OR (the connector for the first clause is meaningless and
    /// ignored). Then resolve each leaf and fold.
    ///
    /// Returns the set of matching node indices, or `None` if the query
    /// has no resolvable constraint (= no filter applied).
    pub fn evaluate(&self, ctx: &EvalContext) -> Option<HashSet<u32>> {
        // Fold-in active badge filters first; they intersect (AND) with
        // whatever the card-stream produces below.
        let badge_set: Option<HashSet<u32>> = match ctx.field_index {
            Some(fi) => fi.matches(&self.active_filters),
            None => None,
        };
        let clauses = self.collect_clauses();
        if clauses.is_empty() {
            return badge_set;
        }

        // If every clause is "unsupported" we have nothing to apply.
        if clauses.iter().all(|c| matches!(c.leaf, Leaf::Unsupported)) {
            return None;
        }

        // Resolve each clause to a Some(set) (matched), or None (loading
        // / unsupported — treated as "match all" for the purpose of the
        // fold so we don't blank the graph mid-load).
        let resolved: Vec<(ConnectorOp, bool, Option<HashSet<u32>>)> = clauses
            .iter()
            .map(|c| {
                let set = match &c.leaf {
                    Leaf::Search(q) => ctx.search_results.get(q).cloned(),
                    Leaf::Unsupported => None,
                };
                (c.connector, c.negate, set)
            })
            .collect();

        // If nothing has resolved yet, hold off filtering this frame.
        if resolved.iter().all(|(_, _, s)| s.is_none()) {
            return None;
        }

        // Fold left-to-right. AND intersects, OR unions. A "loading"
        // (None) leaf is treated as the universe so the partial query
        // doesn't go dark while a fetch is in flight.
        let universe: HashSet<u32> = (0..ctx.ids.len() as u32).collect();
        let materialise = |s: Option<HashSet<u32>>, neg: bool| -> HashSet<u32> {
            let base = s.unwrap_or_else(|| universe.clone());
            if neg {
                universe.difference(&base).copied().collect()
            } else {
                base
            }
        };

        let mut iter = resolved.into_iter();
        let (_, neg0, s0) = iter.next().unwrap();
        let mut acc = materialise(s0, neg0);
        for (op, neg, s) in iter {
            let next = materialise(s, neg);
            acc = match op {
                ConnectorOp::And => acc.intersection(&next).copied().collect(),
                ConnectorOp::Or => acc.union(&next).copied().collect(),
            };
        }
        // Intersect the badge-driven filter set on top.
        if let Some(badges) = badge_set {
            acc = acc.intersection(&badges).copied().collect();
        }
        Some(acc)
    }

    /// Walk `cards` and collapse into a flat clause list. Paren cards
    /// are silently dropped (group-aware folding is a follow-up).
    fn collect_clauses(&self) -> Vec<Clause> {
        let mut out: Vec<Clause> = Vec::new();
        let mut connector = ConnectorOp::And;
        let mut negate = false;
        for c in &self.cards {
            match c {
                Card::Connector { op } => {
                    connector = *op;
                }
                Card::Not => {
                    negate = !negate;
                }
                Card::ParenOpen | Card::ParenClose => {}
                Card::Search { value, .. } => {
                    let v = value.trim();
                    if v.is_empty() {
                        continue;
                    }
                    out.push(Clause {
                        connector,
                        negate,
                        leaf: Leaf::Search(v.to_string()),
                    });
                    connector = ConnectorOp::And;
                    negate = false;
                }
                Card::Filter { value, .. } => {
                    let v = value.trim();
                    if v.is_empty() {
                        continue;
                    }
                    out.push(Clause {
                        connector,
                        negate,
                        leaf: Leaf::Unsupported,
                    });
                    connector = ConnectorOp::And;
                    negate = false;
                }
            }
        }
        out
    }

    /// Collect the set of search-card values that should be resolved
    /// against the backend. Used by App to spawn missing fetches.
    pub fn pending_searches(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.cards {
            if let Card::Search { value, .. } = c {
                let v = value.trim();
                if !v.is_empty() && !out.iter().any(|x| x == v) {
                    out.push(v.to_string());
                }
            }
        }
        out
    }

    /// Reset to the default model: just the system search card.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// If `idx` points at a `ParenOpen`, remove the matching `ParenClose`
/// (the next unmatched `)` to the right). Indices passed in are pre-
/// removal; the open card has already been removed by the caller, so
/// callers must adjust accordingly. We expect the caller to call this
/// AFTER removing the open paren — we then walk forward from `idx`
/// (the position previously occupied by the open) tracking nesting.
pub fn remove_matching_paren_close(cards: &mut Vec<Card>, idx: usize) {
    let mut depth: i32 = 0;
    let mut found: Option<usize> = None;
    for (i, c) in cards.iter().enumerate().skip(idx) {
        match c {
            Card::ParenOpen => depth += 1,
            Card::ParenClose => {
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if let Some(i) = found {
        cards.remove(i);
    }
}

/// Mirror of `remove_matching_paren_close` for a removed `ParenClose`:
/// walk LEFT from `idx-1` to find the matching unmatched `(`.
pub fn remove_matching_paren_open(cards: &mut Vec<Card>, idx: usize) {
    if idx == 0 {
        return;
    }
    let mut depth: i32 = 0;
    let mut found: Option<usize> = None;
    for i in (0..idx).rev() {
        match cards[i] {
            Card::ParenClose => depth += 1,
            Card::ParenOpen => {
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if let Some(i) = found {
        cards.remove(i);
    }
}
