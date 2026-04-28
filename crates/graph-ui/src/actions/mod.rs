mod filter_nodes;
mod search_nodes;
mod sort_nodes;
mod toggle_theme;
mod top_nodes;

pub use filter_nodes::FilterNodes;
pub use search_nodes::SearchNodes;
pub use sort_nodes::SortNodes;
pub use toggle_theme::ToggleTheme;
pub use top_nodes::TopNodes;

use bevy::prelude::*;
use std::collections::HashMap;

// Parameter types an action can declare
#[derive(Clone, Debug)]
pub enum ParamType {
    Text,
    Number,
    Bool,
    Select(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct ActionParam {
    pub name: &'static str,
    pub kind: ParamType,
    pub required: bool,
}

// A registered action
pub trait Action: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn category(&self) -> &'static str;
    fn params(&self) -> Vec<ActionParam> {
        vec![]
    }
    fn execute(&self, params: &HashMap<String, String>, world: &mut World);
}

// Registry stored as a Bevy resource
#[derive(Resource, Default)]
pub struct ActionRegistry {
    pub actions: Vec<Box<dyn Action>>,
}

impl ActionRegistry {
    pub fn register(&mut self, action: impl Action) {
        self.actions.push(Box::new(action));
    }

    pub fn search(&self, query: &str) -> Vec<usize> {
        // returns indices of matching actions, ordered by fuzzy score
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(usize, i64)> = self
            .actions
            .iter()
            .enumerate()
            .filter_map(|(i, a)| matcher.fuzzy_match(a.label(), query).map(|s| (i, s)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(i, _)| i).collect()
    }
}
