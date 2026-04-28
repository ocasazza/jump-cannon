use bevy::prelude::*;
use bevy_egui::EguiPlugin;

use graph_ui::actions::ActionRegistry;
use graph_ui::state::ui::UiState;
use graph_ui::systems::input::handle_input;
use graph_ui::systems::palette::{dispatch_actions, palette_system, ExecuteAction};
use graph_ui::systems::sidebar::sidebar_system;
use graph_ui::systems::status_bar::status_bar_system;
use graph_ui::register_actions;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(EguiPlugin { enable_multipass_for_primary_context: false })
        .init_resource::<UiState>()
        .init_resource::<ActionRegistry>()
        .add_event::<ExecuteAction>()
        .add_systems(Startup, register_actions)
        .add_systems(
            Update,
            (
                handle_input,
                sidebar_system,
                palette_system,
                status_bar_system,
                dispatch_actions,
            ),
        )
        .run();
}
