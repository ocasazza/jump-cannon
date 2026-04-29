use bevy::prelude::*;
use bevy_egui::EguiPlugin;

use graph_ui::actions::ActionRegistry;
use graph_ui::graph::render::draw_edges;
use graph_ui::graph::simulation::{spawn_graph, run_fcose_layout, SimParams};
use graph_ui::graph::interaction::{HoverState, SelectionState, hover_system, click_select_system, highlight_hover_system, focus_mode_system};
use graph_ui::state::ui::UiState;
use graph_ui::systems::camera::{setup_camera, keyboard_camera_system, mouse_pan_system, mouse_zoom_system};
use graph_ui::systems::input::handle_input;
use graph_ui::systems::modal::modal_system;
use graph_ui::systems::palette::{dispatch_actions, palette_system, ExecuteAction};
use graph_ui::systems::search::search_system;
use graph_ui::systems::sidebar::sidebar_system;
use graph_ui::systems::status_bar::status_bar_system;
use graph_ui::systems::persistence::{restore_view_state, save_view_on_exit};
use graph_ui::register_actions;
use graph_ui::vault::{VaultGraphResource, load_vault_system};

fn main() {
    // First positional arg is the vault root path
    let vault_root = std::env::args().nth(1).map(std::path::PathBuf::from);

    let mut vault_resource = VaultGraphResource::default();
    if let Some(root) = vault_root {
        vault_resource.vault_root = root;
    }

    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(EguiPlugin { enable_multipass_for_primary_context: false })
        .init_resource::<UiState>()
        .init_resource::<ActionRegistry>()
        .insert_resource(vault_resource)
        .init_resource::<SimParams>()
        .init_resource::<HoverState>()
        .init_resource::<SelectionState>()
        .add_event::<ExecuteAction>()
        .add_systems(Startup, (register_actions, setup_camera, restore_view_state))
        .add_systems(
            Update,
            (
                load_vault_system,
                // Layout must happen after spawn, render after layout
                spawn_graph.after(load_vault_system),
                run_fcose_layout.after(spawn_graph),
                draw_edges.after(run_fcose_layout),
                // Node interaction
                hover_system,
                click_select_system,
                highlight_hover_system,
                // Search and focus
                search_system,
                focus_mode_system,
                // UI systems
                handle_input,
                sidebar_system,
                palette_system,
                status_bar_system,
                modal_system,
                dispatch_actions,
                // Camera controls
                keyboard_camera_system,
                mouse_pan_system,
                mouse_zoom_system,
                // Persistence
                save_view_on_exit,
            ),
        )
        .run();
}
