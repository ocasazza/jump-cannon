use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use std::collections::HashMap;

use crate::actions::ActionRegistry;
use crate::state::ui::UiState;

/// Fired when the user confirms an action in the palette.
#[derive(Event)]
pub struct ExecuteAction(pub usize); // index into ActionRegistry::actions

pub fn palette_system(
    mut contexts: EguiContexts,
    mut state: ResMut<UiState>,
    mut query: Local<String>,
    mut selection: Local<usize>,
    registry: Res<ActionRegistry>,
    mut execute_events: EventWriter<ExecuteAction>,
) {
    if !state.palette_open {
        return;
    }

    let ctx = contexts.ctx_mut();
    let screen_rect = ctx.screen_rect();
    let palette_width = 500.0_f32;
    let offset_x = (screen_rect.width() - palette_width) / 2.0;
    let offset_y = screen_rect.height() * 0.2;

    // Compute matches before borrowing ctx for show()
    let matches: Vec<usize> = if query.is_empty() {
        (0..registry.actions.len()).collect()
    } else {
        registry.search(&query)
    };

    let mut open = true;
    egui::Window::new("Command Palette")
        .open(&mut open)
        .fixed_pos(egui::pos2(offset_x, offset_y))
        .fixed_size(egui::vec2(palette_width, 300.0))
        .collapsible(false)
        .show(ctx, |ui| {
            let response = ui.text_edit_singleline(&mut *query);
            response.request_focus();

            // Clamp selection to valid range
            let count = matches.len();
            if *selection >= count && count > 0 {
                *selection = count - 1;
            }

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.palette_open = false;
                query.clear();
                *selection = 0;
                return;
            }

            if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                if count > 0 {
                    *selection = (*selection + 1).min(count - 1);
                }
            }

            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                *selection = selection.saturating_sub(1);
            }

            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(&action_idx) = matches.get(*selection) {
                    execute_events.write(ExecuteAction(action_idx));
                }
                state.palette_open = false;
                query.clear();
                *selection = 0;
                return;
            }

            ui.separator();

            if matches.is_empty() {
                ui.label("No matching actions.");
                return;
            }

            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for (row, &action_idx) in matches.iter().enumerate() {
                        let action = &registry.actions[action_idx];
                        let label = format!("[{}]  {}", action.category(), action.label());
                        let selected = row == *selection;
                        if ui.selectable_label(selected, &label).clicked() {
                            *selection = row;
                            execute_events.write(ExecuteAction(action_idx));
                            state.palette_open = false;
                            query.clear();
                        }
                    }
                });
        });

    if !open {
        state.palette_open = false;
        query.clear();
        *selection = 0;
    }
}

/// Exclusive system: drains ExecuteAction events and calls action.execute().
pub fn dispatch_actions(world: &mut World) {
    // Drain all pending events first, then execute.
    let events: Vec<usize> = {
        let mut reader = world
            .resource_mut::<Events<ExecuteAction>>()
            .get_cursor_current();
        let events_res = world.resource::<Events<ExecuteAction>>();
        reader.read(events_res).map(|e| e.0).collect()
    };

    for action_idx in events {
        // We need to temporarily take the registry to call execute,
        // then put it back. Use a scope to avoid borrow conflicts.
        world.resource_scope(|world, registry: Mut<ActionRegistry>| {
            if let Some(action) = registry.actions.get(action_idx) {
                action.execute(&HashMap::new(), world);
            }
        });
    }
}
