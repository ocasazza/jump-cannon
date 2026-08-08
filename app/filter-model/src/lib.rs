//! Pure query model for Jump Cannon's visual filter builder.
//!
//! This crate deliberately has no Dioxus, browser, renderer, or HTTP
//! dependencies. The UI and every non-pointer command operate on the same
//! stable-ID Boolean tree, while native unit tests prove the reducer and set
//! algebra independently of WASM.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};

pub type NodeId = u64;
pub type FacetMap = HashMap<String, HashMap<String, Vec<u32>>>;
pub type SearchMatches = HashMap<NodeId, HashSet<u32>>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Combinator {
    #[default]
    Any,
    All,
}

impl Combinator {
    pub fn toggled(self) -> Self {
        match self {
            Self::Any => Self::All,
            Self::All => Self::Any,
        }
    }
}

/// Compatibility projection for v1 facet chips and imported shared state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveFieldFilters {
    pub by_field: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub insertion_order: Vec<String>,
    #[serde(default)]
    pub field_combinator: BTreeMap<String, Combinator>,
    #[serde(default = "default_cross_field_combinator")]
    pub cross_field_combinator: Combinator,
}

fn default_cross_field_combinator() -> Combinator {
    Combinator::All
}

impl Default for ActiveFieldFilters {
    fn default() -> Self {
        Self {
            by_field: BTreeMap::new(),
            insertion_order: Vec::new(),
            field_combinator: BTreeMap::new(),
            cross_field_combinator: Combinator::All,
        }
    }
}

impl ActiveFieldFilters {
    pub fn combinator_for(&self, field: &str) -> Combinator {
        self.field_combinator
            .get(field)
            .copied()
            .unwrap_or(Combinator::Any)
    }

    pub fn set_combinator_for(&mut self, field: &str, combinator: Combinator) {
        if combinator == Combinator::Any {
            self.field_combinator.remove(field);
        } else {
            self.field_combinator.insert(field.to_string(), combinator);
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Op {
    Eq,
    Neq,
    Contains,
    Matches,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectorOp {
    And,
    Or,
}

/// v1 scratchpad token. Kept only so existing AppState/share payloads decode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Card {
    Search { value: String, regex: bool },
    Filter { field: String, op: Op, value: String },
    Connector { op: ConnectorOp },
    ParenOpen,
    ParenClose,
    Not,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum GroupMode {
    #[default]
    All,
    Any,
}

impl GroupMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::All => Self::Any,
            Self::Any => Self::All,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleKind {
    Search { query: String },
    Field { field: String, op: Op, value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub id: NodeId,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub negated: bool,
    pub kind: RuleKind,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleGroup {
    pub id: NodeId,
    #[serde(default)]
    pub mode: GroupMode,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub negated: bool,
    #[serde(default)]
    pub children: Vec<Clause>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Clause {
    Rule(Rule),
    Group(RuleGroup),
}

impl Clause {
    pub fn id(&self) -> NodeId {
        match self {
            Self::Rule(rule) => rule.id,
            Self::Group(group) => group.id,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Self::Rule(rule) => rule.enabled,
            Self::Group(group) => group.enabled,
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        match self {
            Self::Rule(rule) => rule.enabled = enabled,
            Self::Group(group) => group.enabled = enabled,
        }
    }

    fn set_negated(&mut self, negated: bool) {
        match self {
            Self::Rule(rule) => rule.negated = negated,
            Self::Group(group) => group.negated = negated,
        }
    }

    fn negated(&self) -> bool {
        match self {
            Self::Rule(rule) => rule.negated,
            Self::Group(group) => group.negated,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryModel {
    /// Legacy, non-executing v1 scratchpad. Drained during normalization.
    #[serde(default)]
    pub cards: Vec<Card>,
    /// Compatibility projection consumed by Inspector/badge surfaces.
    #[serde(default)]
    pub active_filters: ActiveFieldFilters,
    /// Canonical v2 expression. `None` identifies an imported v1 payload.
    #[serde(default)]
    pub expression: Option<RuleGroup>,
    #[serde(default)]
    next_id: NodeId,
}

impl Default for QueryModel {
    fn default() -> Self {
        Self {
            cards: Vec::new(),
            active_filters: ActiveFieldFilters::default(),
            expression: Some(RuleGroup {
                id: 1,
                mode: GroupMode::All,
                enabled: true,
                negated: false,
                children: Vec::new(),
            }),
            next_id: 2,
        }
    }
}

impl QueryModel {
    /// Upgrade v1 state. Live facet selections stay active; the old card
    /// scratchpad is preserved as disabled rules because it never affected
    /// the graph and must not silently become active after an upgrade.
    pub fn normalize_imported(&mut self) {
        if self.expression.is_none() {
            let mut fresh = QueryModel::default();
            fresh.expression.as_mut().unwrap().mode = match self.active_filters.cross_field_combinator {
                Combinator::All => GroupMode::All,
                Combinator::Any => GroupMode::Any,
            };

            let mut ordered = self.active_filters.insertion_order.clone();
            for field in self.active_filters.by_field.keys() {
                if !ordered.iter().any(|candidate| candidate == field) {
                    ordered.push(field.clone());
                }
            }
            for field in ordered {
                let Some(values) = self.active_filters.by_field.get(&field) else {
                    continue;
                };
                let group_id = fresh.alloc_id();
                let mut group = RuleGroup {
                    id: group_id,
                    mode: match self.active_filters.combinator_for(&field) {
                        Combinator::All => GroupMode::All,
                        Combinator::Any => GroupMode::Any,
                    },
                    enabled: true,
                    negated: false,
                    children: Vec::new(),
                };
                for value in values {
                    group.children.push(Clause::Rule(Rule {
                        id: fresh.alloc_id(),
                        enabled: true,
                        negated: false,
                        kind: RuleKind::Field {
                            field: field.clone(),
                            op: Op::Eq,
                            value: value.clone(),
                        },
                    }));
                }
                fresh.expression.as_mut().unwrap().children.push(Clause::Group(group));
            }
            fresh.cards = std::mem::take(&mut self.cards);
            *self = fresh;
            self.absorb_legacy_cards(false);
        } else {
            self.repair_next_id();
            self.absorb_legacy_cards(false);
        }
        self.refresh_active_projection();
    }

    pub fn root(&self) -> &RuleGroup {
        self.expression.as_ref().expect("query model must be normalized")
    }

    pub fn root_mut(&mut self) -> &mut RuleGroup {
        self.expression.as_mut().expect("query model must be normalized")
    }

    pub fn is_empty(&self) -> bool {
        !has_enabled_leaf(self.root())
    }

    pub fn add_search(&mut self, group_id: NodeId) -> Option<NodeId> {
        let id = self.alloc_id();
        let group = find_group_mut(self.root_mut(), group_id)?;
        group.children.push(Clause::Rule(Rule {
            id,
            enabled: true,
            negated: false,
            kind: RuleKind::Search { query: String::new() },
        }));
        Some(id)
    }

    pub fn add_field(&mut self, group_id: NodeId, field: impl Into<String>) -> Option<NodeId> {
        let id = self.alloc_id();
        let group = find_group_mut(self.root_mut(), group_id)?;
        group.children.push(Clause::Rule(Rule {
            id,
            enabled: true,
            negated: false,
            kind: RuleKind::Field {
                field: field.into(),
                op: Op::Eq,
                value: String::new(),
            },
        }));
        self.refresh_active_projection();
        Some(id)
    }

    pub fn add_group(&mut self, parent_id: NodeId) -> Option<NodeId> {
        let id = self.alloc_id();
        let group = find_group_mut(self.root_mut(), parent_id)?;
        group.children.push(Clause::Group(RuleGroup {
            id,
            mode: GroupMode::All,
            enabled: true,
            negated: false,
            children: Vec::new(),
        }));
        Some(id)
    }

    pub fn rule_mut(&mut self, id: NodeId) -> Option<&mut Rule> {
        find_rule_mut(self.root_mut(), id)
    }

    pub fn group_mut(&mut self, id: NodeId) -> Option<&mut RuleGroup> {
        find_group_mut(self.root_mut(), id)
    }

    pub fn remove(&mut self, id: NodeId) -> bool {
        if id == self.root().id {
            return false;
        }
        let removed = extract_clause(self.root_mut(), id).is_some();
        if removed {
            prune_empty_groups(self.root_mut());
            self.refresh_active_projection();
        }
        removed
    }

    pub fn toggle_enabled(&mut self, id: NodeId) -> bool {
        let Some(clause) = find_clause_mut(self.root_mut(), id) else {
            return false;
        };
        clause.set_enabled(!clause.enabled());
        self.refresh_active_projection();
        true
    }

    pub fn toggle_negated(&mut self, id: NodeId) -> bool {
        let Some(clause) = find_clause_mut(self.root_mut(), id) else {
            return false;
        };
        clause.set_negated(!clause.negated());
        self.refresh_active_projection();
        true
    }

    pub fn move_before(&mut self, source_id: NodeId, target_id: NodeId) -> bool {
        if source_id == target_id || source_id == self.root().id {
            return false;
        }
        if clause_contains(self.root(), source_id, target_id) {
            return false;
        }
        let Some(source) = extract_clause(self.root_mut(), source_id) else {
            return false;
        };
        if insert_before(self.root_mut(), target_id, source.clone()) {
            self.refresh_active_projection();
            true
        } else {
            self.root_mut().children.push(source);
            false
        }
    }

    pub fn move_to_group(&mut self, source_id: NodeId, group_id: NodeId) -> bool {
        if source_id == self.root().id || source_id == group_id {
            return false;
        }
        if clause_contains(self.root(), source_id, group_id) {
            return false;
        }
        let Some(source) = extract_clause(self.root_mut(), source_id) else {
            return false;
        };
        if let Some(group) = find_group_mut(self.root_mut(), group_id) {
            group.children.push(source);
            self.refresh_active_projection();
            true
        } else {
            self.root_mut().children.push(source);
            false
        }
    }

    pub fn move_by(&mut self, id: NodeId, delta: isize) -> bool {
        let Some((parent_id, index)) = find_parent(self.root(), id) else {
            return false;
        };
        let Some(parent) = find_group_mut(self.root_mut(), parent_id) else {
            return false;
        };
        let target = if delta.is_negative() {
            index.checked_sub(delta.unsigned_abs())
        } else {
            index.checked_add(delta as usize)
        };
        let Some(target) = target.filter(|target| *target < parent.children.len()) else {
            return false;
        };
        parent.children.swap(index, target);
        self.refresh_active_projection();
        true
    }

    pub fn toggle_field_filter(&mut self, field: &str, value: &str) {
        if let Some(id) = find_positive_equality(self.root(), field, value) {
            self.remove(id);
        } else {
            let id = self.alloc_id();
            self.root_mut().children.push(Clause::Rule(Rule {
                id,
                enabled: true,
                negated: false,
                kind: RuleKind::Field {
                    field: field.to_string(),
                    op: Op::Eq,
                    value: value.to_string(),
                },
            }));
            self.refresh_active_projection();
        }
    }

    pub fn clear_field(&mut self, field: &str) {
        remove_field_rules(self.root_mut(), field);
        prune_empty_groups(self.root_mut());
        self.refresh_active_projection();
    }

    pub fn clear_all_filters(&mut self) {
        remove_all_field_rules(self.root_mut());
        prune_empty_groups(self.root_mut());
        self.refresh_active_projection();
    }

    pub fn is_filter_active(&self, field: &str, value: &str) -> bool {
        find_positive_equality(self.root(), field, value).is_some()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Convert cards appended by compatibility callers into active v2 leaves.
    pub fn absorb_appended_cards(&mut self) {
        self.absorb_legacy_cards(true);
        self.refresh_active_projection();
    }

    fn absorb_legacy_cards(&mut self, enabled: bool) {
        let cards = std::mem::take(&mut self.cards);
        for card in cards {
            let kind = match card {
                Card::Search { value, .. } if !value.trim().is_empty() => {
                    Some(RuleKind::Search { query: value })
                }
                Card::Filter { field, op, value } => Some(RuleKind::Field {
                    field: canonical_legacy_field(&field).to_string(),
                    op,
                    value,
                }),
                _ => None,
            };
            if let Some(kind) = kind {
                let id = self.alloc_id();
                self.root_mut().children.push(Clause::Rule(Rule {
                    id,
                    enabled,
                    negated: false,
                    kind,
                }));
            }
        }
    }

    fn alloc_id(&mut self) -> NodeId {
        if self.next_id < 2 {
            self.repair_next_id();
        }
        let id = self.next_id;
        self.next_id = id.saturating_add(1).max(2);
        id
    }

    fn repair_next_id(&mut self) {
        let max_id = self.expression.as_ref().map(max_group_id).unwrap_or(1);
        self.next_id = max_id.saturating_add(1).max(2);
    }

    fn refresh_active_projection(&mut self) {
        let mut projection = ActiveFieldFilters::default();
        collect_positive_equalities(self.root(), &mut projection);
        self.active_filters = projection;
    }
}

fn canonical_legacy_field(field: &str) -> &str {
    match field {
        "tag" => "tags",
        "name" => "title",
        other => other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub node_id: NodeId,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalOutput {
    /// `None` means no enabled query. `Some(empty)` is a valid zero-result query.
    pub matches: Option<HashSet<u32>>,
    pub counts: HashMap<NodeId, usize>,
}

pub fn validate(root: &RuleGroup, facets: &FacetMap, search_available: bool) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    validate_group(root, true, facets, search_available, &mut out);
    out
}

fn validate_group(
    group: &RuleGroup,
    is_root: bool,
    facets: &FacetMap,
    search_available: bool,
    out: &mut Vec<Diagnostic>,
) {
    if !group.enabled {
        return;
    }
    let enabled_children: Vec<&Clause> = group.children.iter().filter(|child| child.enabled()).collect();
    if !is_root && enabled_children.is_empty() {
        out.push(Diagnostic {
            node_id: group.id,
            message: "Add a rule to this group or turn it off.".into(),
        });
    }
    for child in enabled_children {
        match child {
            Clause::Group(nested) => validate_group(nested, false, facets, search_available, out),
            Clause::Rule(rule) => match &rule.kind {
                RuleKind::Search { query } => {
                    if query.trim().is_empty() {
                        out.push(Diagnostic {
                            node_id: rule.id,
                            message: "Enter a search query.".into(),
                        });
                    } else if !search_available {
                        out.push(Diagnostic {
                            node_id: rule.id,
                            message: "Indexed search requires a server-hosted graph.".into(),
                        });
                    }
                }
                RuleKind::Field { field, op, value } => {
                    if field.trim().is_empty() {
                        out.push(Diagnostic {
                            node_id: rule.id,
                            message: "Choose a field.".into(),
                        });
                    } else if !facets.contains_key(field) {
                        out.push(Diagnostic {
                            node_id: rule.id,
                            message: format!("{field:?} is not a filterable field in this graph."),
                        });
                    }
                    if value.trim().is_empty() {
                        out.push(Diagnostic {
                            node_id: rule.id,
                            message: "Enter or choose a value.".into(),
                        });
                    } else if *op == Op::Matches {
                        if let Err(error) = Regex::new(value) {
                            out.push(Diagnostic {
                                node_id: rule.id,
                                message: format!("Invalid regular expression: {error}"),
                            });
                        }
                    }
                }
            },
        }
    }
}

pub fn evaluate(
    root: &RuleGroup,
    facets: &FacetMap,
    searches: &SearchMatches,
    node_count: usize,
) -> EvalOutput {
    if !has_enabled_leaf(root) {
        return EvalOutput::default();
    }
    let universe: HashSet<u32> = (0..node_count.min(u32::MAX as usize))
        .map(|index| index as u32)
        .collect();
    let mut counts = HashMap::new();
    let matches = eval_group(root, facets, searches, &universe, &mut counts);
    EvalOutput {
        matches: Some(matches),
        counts,
    }
}

fn eval_group(
    group: &RuleGroup,
    facets: &FacetMap,
    searches: &SearchMatches,
    universe: &HashSet<u32>,
    counts: &mut HashMap<NodeId, usize>,
) -> HashSet<u32> {
    let mut sets = group
        .children
        .iter()
        .filter(|child| child.enabled())
        .map(|child| eval_clause(child, facets, searches, universe, counts));
    let mut result = match group.mode {
        GroupMode::All => sets.next().unwrap_or_else(|| universe.clone()),
        GroupMode::Any => sets.next().unwrap_or_default(),
    };
    for next in sets {
        match group.mode {
            GroupMode::All => result.retain(|node| next.contains(node)),
            GroupMode::Any => result.extend(next),
        }
    }
    if group.negated {
        result = complement(&result, universe);
    }
    counts.insert(group.id, result.len());
    result
}

fn eval_clause(
    clause: &Clause,
    facets: &FacetMap,
    searches: &SearchMatches,
    universe: &HashSet<u32>,
    counts: &mut HashMap<NodeId, usize>,
) -> HashSet<u32> {
    match clause {
        Clause::Group(group) => eval_group(group, facets, searches, universe, counts),
        Clause::Rule(rule) => {
            let mut set = match &rule.kind {
                RuleKind::Search { .. } => searches.get(&rule.id).cloned().unwrap_or_default(),
                RuleKind::Field { field, op, value } => {
                    eval_field(field, *op, value, facets, universe)
                }
            };
            if rule.negated {
                set = complement(&set, universe);
            }
            counts.insert(rule.id, set.len());
            set
        }
    }
}

fn eval_field(
    field: &str,
    op: Op,
    value: &str,
    facets: &FacetMap,
    universe: &HashSet<u32>,
) -> HashSet<u32> {
    let Some(values) = facets.get(field) else {
        return HashSet::new();
    };
    let mut result = HashSet::new();
    match op {
        Op::Eq | Op::Neq => {
            if let Some(nodes) = values.get(value) {
                result.extend(nodes.iter().copied());
            }
        }
        Op::Contains => {
            let needle = value.to_lowercase();
            for (candidate, nodes) in values {
                if candidate.to_lowercase().contains(&needle) {
                    result.extend(nodes.iter().copied());
                }
            }
        }
        Op::Matches => {
            if let Ok(regex) = Regex::new(value) {
                for (candidate, nodes) in values {
                    if regex.is_match(candidate) {
                        result.extend(nodes.iter().copied());
                    }
                }
            }
        }
    }
    if op == Op::Neq {
        complement(&result, universe)
    } else {
        result
    }
}

fn complement(set: &HashSet<u32>, universe: &HashSet<u32>) -> HashSet<u32> {
    universe.difference(set).copied().collect()
}

pub fn search_rules(root: &RuleGroup) -> Vec<(NodeId, String)> {
    let mut out = Vec::new();
    collect_search_rules(root, &mut out);
    out
}

fn collect_search_rules(group: &RuleGroup, out: &mut Vec<(NodeId, String)>) {
    if !group.enabled {
        return;
    }
    for child in &group.children {
        match child {
            Clause::Group(group) => collect_search_rules(group, out),
            Clause::Rule(Rule {
                id,
                enabled: true,
                kind: RuleKind::Search { query },
                ..
            }) if !query.trim().is_empty() => out.push((*id, query.clone())),
            _ => {}
        }
    }
}

fn has_enabled_leaf(group: &RuleGroup) -> bool {
    group.enabled
        && group.children.iter().any(|child| match child {
            Clause::Rule(rule) => rule.enabled,
            Clause::Group(group) => has_enabled_leaf(group),
        })
}

fn find_group_mut(group: &mut RuleGroup, id: NodeId) -> Option<&mut RuleGroup> {
    if group.id == id {
        return Some(group);
    }
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            if let Some(found) = find_group_mut(nested, id) {
                return Some(found);
            }
        }
    }
    None
}

fn find_rule_mut(group: &mut RuleGroup, id: NodeId) -> Option<&mut Rule> {
    for child in &mut group.children {
        match child {
            Clause::Rule(rule) if rule.id == id => return Some(rule),
            Clause::Group(nested) => {
                if let Some(found) = find_rule_mut(nested, id) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_clause_mut(group: &mut RuleGroup, id: NodeId) -> Option<&mut Clause> {
    for child in &mut group.children {
        if child.id() == id {
            return Some(child);
        }
        if let Clause::Group(nested) = child {
            if let Some(found) = find_clause_mut(nested, id) {
                return Some(found);
            }
        }
    }
    None
}

fn extract_clause(group: &mut RuleGroup, id: NodeId) -> Option<Clause> {
    if let Some(index) = group.children.iter().position(|child| child.id() == id) {
        return Some(group.children.remove(index));
    }
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            if let Some(found) = extract_clause(nested, id) {
                return Some(found);
            }
        }
    }
    None
}

fn insert_before(group: &mut RuleGroup, target_id: NodeId, clause: Clause) -> bool {
    if let Some(index) = group.children.iter().position(|child| child.id() == target_id) {
        group.children.insert(index, clause);
        return true;
    }
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            if insert_before(nested, target_id, clause.clone()) {
                return true;
            }
        }
    }
    false
}

fn clause_contains(root: &RuleGroup, clause_id: NodeId, needle: NodeId) -> bool {
    fn find(group: &RuleGroup, id: NodeId) -> Option<&Clause> {
        for child in &group.children {
            if child.id() == id {
                return Some(child);
            }
            if let Clause::Group(nested) = child {
                if let Some(found) = find(nested, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    fn contains(clause: &Clause, id: NodeId) -> bool {
        match clause {
            Clause::Rule(_) => false,
            Clause::Group(group) => group.children.iter().any(|child| {
                child.id() == id || contains(child, id)
            }),
        }
    }
    find(root, clause_id).is_some_and(|clause| contains(clause, needle))
}

fn find_parent(group: &RuleGroup, id: NodeId) -> Option<(NodeId, usize)> {
    for (index, child) in group.children.iter().enumerate() {
        if child.id() == id {
            return Some((group.id, index));
        }
        if let Clause::Group(nested) = child {
            if let Some(found) = find_parent(nested, id) {
                return Some(found);
            }
        }
    }
    None
}

fn max_group_id(group: &RuleGroup) -> NodeId {
    group.children.iter().fold(group.id, |max_id, child| {
        max_id.max(match child {
            Clause::Rule(rule) => rule.id,
            Clause::Group(group) => max_group_id(group),
        })
    })
}

fn prune_empty_groups(group: &mut RuleGroup) {
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            prune_empty_groups(nested);
        }
    }
    group.children.retain(|child| {
        !matches!(child, Clause::Group(nested) if nested.children.is_empty())
    });
}

fn remove_field_rules(group: &mut RuleGroup, field: &str) {
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            remove_field_rules(nested, field);
        }
    }
    group.children.retain(|child| {
        !matches!(child, Clause::Rule(Rule { kind: RuleKind::Field { field: candidate, .. }, .. }) if candidate == field)
    });
}

fn remove_all_field_rules(group: &mut RuleGroup) {
    for child in &mut group.children {
        if let Clause::Group(nested) = child {
            remove_all_field_rules(nested);
        }
    }
    group.children.retain(|child| {
        !matches!(child, Clause::Rule(Rule { kind: RuleKind::Field { .. }, .. }))
    });
}

fn find_positive_equality(group: &RuleGroup, field: &str, value: &str) -> Option<NodeId> {
    for child in &group.children {
        match child {
            Clause::Rule(Rule {
                id,
                enabled: true,
                negated: false,
                kind: RuleKind::Field { field: candidate, op: Op::Eq, value: candidate_value },
            }) if candidate == field && candidate_value == value => return Some(*id),
            Clause::Group(nested) if nested.enabled && !nested.negated => {
                if let Some(id) = find_positive_equality(nested, field, value) {
                    return Some(id);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_positive_equalities(group: &RuleGroup, out: &mut ActiveFieldFilters) {
    if !group.enabled || group.negated {
        return;
    }
    for child in &group.children {
        match child {
            Clause::Rule(Rule {
                enabled: true,
                negated: false,
                kind: RuleKind::Field { field, op: Op::Eq, value },
                ..
            }) if !field.is_empty() && !value.is_empty() => {
                if !out.by_field.contains_key(field) {
                    out.insertion_order.push(field.clone());
                }
                out.by_field.entry(field.clone()).or_default().insert(value.clone());
            }
            Clause::Group(nested) => collect_positive_equalities(nested, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facets() -> FacetMap {
        HashMap::from([
            ("tags".into(), HashMap::from([
                ("rust".into(), vec![0, 1, 3]),
                ("gpu".into(), vec![1, 2]),
            ])),
            ("status".into(), HashMap::from([
                ("active".into(), vec![0, 2]),
                ("archived".into(), vec![1, 3]),
            ])),
        ])
    }

    fn field(id: NodeId, name: &str, value: &str) -> Clause {
        Clause::Rule(Rule {
            id,
            enabled: true,
            negated: false,
            kind: RuleKind::Field {
                field: name.into(),
                op: Op::Eq,
                value: value.into(),
            },
        })
    }

    #[test]
    fn nested_boolean_tree_and_negation_are_exact() {
        let root = RuleGroup {
            id: 1,
            mode: GroupMode::All,
            enabled: true,
            negated: false,
            children: vec![
                Clause::Group(RuleGroup {
                    id: 2,
                    mode: GroupMode::Any,
                    enabled: true,
                    negated: false,
                    children: vec![field(3, "tags", "rust"), field(4, "tags", "gpu")],
                }),
                Clause::Rule(Rule {
                    id: 5,
                    enabled: true,
                    negated: true,
                    kind: RuleKind::Field {
                        field: "status".into(),
                        op: Op::Eq,
                        value: "archived".into(),
                    },
                }),
            ],
        };
        let result = evaluate(&root, &facets(), &HashMap::new(), 4);
        assert_eq!(result.matches, Some(HashSet::from([0, 2])));
        assert_eq!(result.counts.get(&2), Some(&4));
        assert_eq!(result.counts.get(&1), Some(&2));
    }

    #[test]
    fn multiple_searches_are_ordinary_leaves() {
        let root = RuleGroup {
            id: 1,
            mode: GroupMode::Any,
            enabled: true,
            negated: false,
            children: vec![
                Clause::Rule(Rule { id: 2, enabled: true, negated: false, kind: RuleKind::Search { query: "alpha".into() } }),
                Clause::Rule(Rule { id: 3, enabled: true, negated: false, kind: RuleKind::Search { query: "beta".into() } }),
            ],
        };
        let searches = HashMap::from([
            (2, HashSet::from([0, 1])),
            (3, HashSet::from([1, 2])),
        ]);
        assert_eq!(evaluate(&root, &facets(), &searches, 4).matches, Some(HashSet::from([0, 1, 2])));
    }

    #[test]
    fn zero_results_remain_an_active_result() {
        let root = RuleGroup {
            id: 1,
            mode: GroupMode::All,
            enabled: true,
            negated: false,
            children: vec![field(2, "tags", "missing")],
        };
        assert_eq!(evaluate(&root, &facets(), &HashMap::new(), 4).matches, Some(HashSet::new()));
    }

    #[test]
    fn validation_is_node_keyed_and_checks_regex() {
        let root = RuleGroup {
            id: 1,
            mode: GroupMode::All,
            enabled: true,
            negated: false,
            children: vec![Clause::Rule(Rule {
                id: 7,
                enabled: true,
                negated: false,
                kind: RuleKind::Field { field: "tags".into(), op: Op::Matches, value: "[".into() },
            })],
        };
        let diagnostics = validate(&root, &facets(), true);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].node_id, 7);
        assert!(diagnostics[0].message.starts_with("Invalid regular expression"));
    }

    #[test]
    fn move_reorders_and_rejects_descendant_cycles() {
        let mut query = QueryModel::default();
        let root = query.root().id;
        let a = query.add_field(root, "tags").unwrap();
        let group = query.add_group(root).unwrap();
        let b = query.add_search(group).unwrap();
        assert!(query.move_before(b, a));
        assert_eq!(query.root().children[0].id(), b);
        assert!(!query.move_to_group(group, b));
    }

    #[test]
    fn v1_live_facets_migrate_but_scratchpad_cards_stay_off() {
        let mut query = QueryModel {
            cards: vec![Card::Search { value: "draft".into(), regex: false }],
            active_filters: ActiveFieldFilters {
                by_field: [("tags".into(), BTreeSet::from(["rust".into()]))].into_iter().collect(),
                insertion_order: Vec::new(),
                ..Default::default()
            },
            expression: None,
            next_id: 0,
        };
        query.normalize_imported();
        assert!(query.is_filter_active("tags", "rust"));
        let searches = search_rules(query.root());
        assert!(searches.is_empty());
        assert!(query.root().children.iter().any(|child| matches!(child, Clause::Rule(Rule { enabled: false, kind: RuleKind::Search { query }, .. }) if query == "draft")));
    }

    /// Build the v1 payload shape: no expression, live facet selections only.
    fn v1(by_field: &[(&str, &[&str])]) -> QueryModel {
        QueryModel {
            cards: Vec::new(),
            active_filters: ActiveFieldFilters {
                by_field: by_field
                    .iter()
                    .map(|(field, values)| {
                        (
                            (*field).to_string(),
                            values.iter().map(|v| (*v).to_string()).collect(),
                        )
                    })
                    .collect(),
                insertion_order: Vec::new(),
                ..Default::default()
            },
            expression: None,
            next_id: 0,
        }
    }

    fn v1_matches(query: &mut QueryModel) -> Option<HashSet<u32>> {
        query.normalize_imported();
        evaluate(query.root(), &facets(), &HashMap::new(), 4).matches
    }

    // The four cases below were the v1 facet evaluator's contract. They now run
    // through migration + `evaluate` so the upgrade is pinned to the same node
    // sets the old direct-index path produced.

    #[test]
    fn v1_defaults_union_values_and_intersect_fields() {
        let mut query = v1(&[("tags", &["rust", "gpu"]), ("status", &["archived"])]);
        assert_eq!(v1_matches(&mut query), Some(HashSet::from([1, 3])));
    }

    #[test]
    fn v1_all_within_a_multivalue_field_intersects_buckets() {
        let mut query = v1(&[("tags", &["rust", "gpu"])]);
        query
            .active_filters
            .set_combinator_for("tags", Combinator::All);
        assert_eq!(v1_matches(&mut query), Some(HashSet::from([1])));
    }

    #[test]
    fn v1_cross_field_any_unions_field_results() {
        let mut query = v1(&[("tags", &["rust"]), ("status", &["active"])]);
        query.active_filters.cross_field_combinator = Combinator::Any;
        assert_eq!(v1_matches(&mut query), Some(HashSet::from([0, 1, 2, 3])));
    }

    /// A value that vanished from the graph must drop out of its own OR arm,
    /// not sink the whole field to the empty set.
    #[test]
    fn v1_unknown_persisted_values_do_not_destroy_known_matches() {
        let mut query = v1(&[("tags", &["rust", "removed-tag"])]);
        assert_eq!(v1_matches(&mut query), Some(HashSet::from([0, 1, 3])));
    }

    #[test]
    fn serde_round_trip_preserves_tree_and_ids() {
        let mut query = QueryModel::default();
        let root = query.root().id;
        query.add_search(root);
        let encoded = serde_json::to_string(&query).unwrap();
        let decoded: QueryModel = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, query);
    }
}
