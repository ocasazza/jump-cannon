use bevy::prelude::*;
use std::collections::HashMap;

use crate::actions::{Action, ActionParam, ParamType};

pub struct FilterNodes;

impl Action for FilterNodes {
    fn id(&self) -> &'static str {
        "filter-nodes"
    }

    fn label(&self) -> &'static str {
        "Filter Nodes"
    }

    fn category(&self) -> &'static str {
        "Graph"
    }

    fn params(&self) -> Vec<ActionParam> {
        vec![
            ActionParam {
                name: "field",
                kind: ParamType::Text,
                required: true,
            },
            ActionParam {
                name: "value",
                kind: ParamType::Text,
                required: true,
            },
        ]
    }

    fn execute(&self, params: &HashMap<String, String>, _world: &mut World) {
        let field = params.get("field").map(|s| s.as_str()).unwrap_or("");
        let value = params.get("value").map(|s| s.as_str()).unwrap_or("");
        info!("filter: {field}={value}");
    }
}
