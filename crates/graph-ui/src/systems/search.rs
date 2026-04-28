use bevy::prelude::*;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use crate::state::ui::UiState;
use crate::vault::VaultGraphResource;

/// Runs whenever the search query changes. Updates ui_state.search_results.
pub fn search_system(
    mut ui_state: ResMut<UiState>,
    vault: Res<VaultGraphResource>,
) {
    if !vault.loaded { return; }

    if ui_state.search_query.is_empty() {
        ui_state.search_results.clear();
        return;
    }

    let matcher = SkimMatcherV2::default();
    let query = ui_state.search_query.clone();

    let mut scored: Vec<(i64, String)> = vault.graph.nodes.values()
        .filter_map(|node| {
            // Score against title + tags
            let candidate = format!("{} {}", node.meta.title, node.meta.tags.join(" "));
            matcher.fuzzy_match(&candidate, &query)
                .map(|score| (score, node.id.clone()))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    ui_state.search_results = scored.into_iter().take(50).map(|(_, id)| id).collect();
}
