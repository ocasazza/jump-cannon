use bevy::prelude::*;
use std::collections::HashMap;

use crate::actions::{Action, ActionParam, ParamType};

pub struct TopNodes;

impl Action for TopNodes {
    fn id(&self) -> &'static str {
        "top-nodes"
    }

    fn label(&self) -> &'static str {
        "Top Nodes"
    }

    fn category(&self) -> &'static str {
        "Graph"
    }

    fn params(&self) -> Vec<ActionParam> {
        vec![ActionParam {
            name: "n",
            kind: ParamType::Number,
            required: true,
        }]
    }

    fn execute(&self, params: &HashMap<String, String>, _world: &mut World) {
        let n = params.get("n").map(|s| s.as_str()).unwrap_or("0");
        info!("top: {n}");
    }
}
