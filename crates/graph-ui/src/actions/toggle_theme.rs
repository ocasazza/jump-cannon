use bevy::prelude::*;
use bevy_egui::{egui, EguiContext};
use std::collections::HashMap;

use crate::actions::{Action, ActionParam};

pub struct ToggleTheme;

impl Action for ToggleTheme {
    fn id(&self) -> &'static str {
        "toggle-theme"
    }

    fn label(&self) -> &'static str {
        "Toggle Theme"
    }

    fn category(&self) -> &'static str {
        "Settings"
    }

    fn params(&self) -> Vec<ActionParam> {
        vec![]
    }

    fn execute(&self, _params: &HashMap<String, String>, world: &mut World) {
        let mut q = world.query::<&mut EguiContext>();
        if let Some(mut ctx) = q.iter_mut(world).next() {
            let egui_ctx = ctx.get_mut();
            let is_dark = egui_ctx.style().visuals.dark_mode;
            if is_dark {
                egui_ctx.set_visuals(egui::Visuals::light());
            } else {
                egui_ctx.set_visuals(egui::Visuals::dark());
            }
        }
    }
}
