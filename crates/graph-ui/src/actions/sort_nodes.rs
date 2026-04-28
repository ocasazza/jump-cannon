use bevy::prelude::*;
use std::collections::HashMap;

use crate::actions::{Action, ActionParam, ParamType};

pub struct SortNodes;

impl Action for SortNodes {
    fn id(&self) -> &'static str {
        "sort-nodes"
    }

    fn label(&self) -> &'static str {
        "Sort Nodes"
    }

    fn category(&self) -> &'static str {
        "Graph"
    }

    fn params(&self) -> Vec<ActionParam> {
        vec![
            ActionParam {
                name: "field",
                kind: ParamType::Select(vec![
                    "pagerank".into(),
                    "betweenness".into(),
                    "degree".into(),
                ]),
                required: true,
            },
            ActionParam {
                name: "order",
                kind: ParamType::Select(vec!["asc".into(), "desc".into()]),
                required: true,
            },
        ]
    }

    fn execute(&self, params: &HashMap<String, String>, _world: &mut World) {
        let field = params.get("field").map(|s| s.as_str()).unwrap_or("");
        let order = params.get("order").map(|s| s.as_str()).unwrap_or("");
        info!("sort: {field} {order}");
    }
}
