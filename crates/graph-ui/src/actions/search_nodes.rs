use bevy::prelude::*;
use std::collections::HashMap;

use crate::actions::{Action, ActionParam, ParamType};

pub struct SearchNodes;

impl Action for SearchNodes {
    fn id(&self) -> &'static str {
        "search-nodes"
    }

    fn label(&self) -> &'static str {
        "Search Nodes"
    }

    fn category(&self) -> &'static str {
        "Graph"
    }

    fn params(&self) -> Vec<ActionParam> {
        vec![ActionParam {
            name: "query",
            kind: ParamType::Text,
            required: true,
        }]
    }

    fn execute(&self, params: &HashMap<String, String>, _world: &mut World) {
        let query = params.get("query").map(|s| s.as_str()).unwrap_or("");
        info!("search: {query}");
    }
}
