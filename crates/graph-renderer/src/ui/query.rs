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

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
            ConnectorOp::And => "AND",
            ConnectorOp::Or => "OR",
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
}

impl Default for QueryModel {
    fn default() -> Self {
        Self {
            // Always start with the system search card.
            cards: vec![Card::Search {
                value: String::new(),
                regex: false,
            }],
        }
    }
}

/// Context passed to the evaluator. Only the id list is available in
/// the current phase — a real per-field index hooks in later.
pub struct EvalContext<'a> {
    pub ids: &'a [String],
    _placeholder: (),
}

impl<'a> EvalContext<'a> {
    pub fn new(ids: &'a [String]) -> Self {
        Self {
            ids,
            _placeholder: (),
        }
    }
}

impl QueryModel {
    /// Tokenize → AST → evaluate against per-field indices.
    ///
    /// Returns the set of matching node indices, or `None` if the query
    /// is empty / not yet wired (= match all). This is a stub for
    /// Phase F: node-set computation hooks into a real index in a
    /// follow-up phase.
    pub fn evaluate(&self, ctx: &EvalContext) -> Option<HashSet<u32>> {
        let _ = ctx;
        None
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
