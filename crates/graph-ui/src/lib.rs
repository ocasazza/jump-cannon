pub mod graph;
pub mod query;
pub mod actions;
pub mod state;
pub mod systems;
pub mod vault;

use bevy::prelude::*;
use actions::{ActionRegistry, FilterNodes, SearchNodes, SortNodes, ToggleTheme, TopNodes};

pub fn register_actions(mut registry: ResMut<ActionRegistry>) {
    registry.register(ToggleTheme);
    registry.register(SearchNodes);
    registry.register(FilterNodes);
    registry.register(SortNodes);
    registry.register(TopNodes);
}
