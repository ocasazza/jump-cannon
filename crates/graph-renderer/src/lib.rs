use bevy::prelude::*;
pub mod components;
pub mod systems;
pub mod setup;
pub mod config;

use config::{GraphConfig, RegenerateGraphEvent};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use web_sys::console; // For logging
#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;
#[cfg(target_arch = "wasm32")]
use lazy_static::lazy_static;

// Removed AppState struct and BEVY_WORLD static as they are replaced by the new mechanism

#[cfg(target_arch = "wasm32")]
lazy_static! {
    static ref SHARED_GRAPH_CONFIG: Mutex<GraphConfig> = Mutex::new(GraphConfig::default());
    static ref REGENERATION_REQUESTED: Mutex<bool> = Mutex::new(false);
}

// JS calls this
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn trigger_regeneration(num_nodes: usize, num_edges: usize) {
    console::log_1(&format!("[WASM] trigger_regeneration called with nodes: {}, edges: {}", num_nodes, num_edges).into());
    if let Ok(mut config) = SHARED_GRAPH_CONFIG.lock() {
        config.num_nodes = num_nodes;
        config.num_edges = num_edges;
        console::log_1(&format!("[WASM] SHARED_GRAPH_CONFIG updated: {:?}", *config).into());
    } else {
        console::log_1(&"[WASM] Failed to lock SHARED_GRAPH_CONFIG".into());
    }
    if let Ok(mut req) = REGENERATION_REQUESTED.lock() {
        *req = true;
        console::log_1(&"[WASM] REGENERATION_REQUESTED set to true".into());
    } else {
        console::log_1(&"[WASM] Failed to lock REGENERATION_REQUESTED".into());
    }
}

// Bevy system to check the static signal and update internal resources/send events
pub fn poll_regeneration_request_system(
    mut graph_config_res: ResMut<GraphConfig>,
    mut event_writer: EventWriter<RegenerateGraphEvent>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        use web_sys::console;
        console::log_1(&"[BEVY] poll_regeneration_request_system: System is running!".into());
    }
    
    #[cfg(target_arch = "wasm32")]
    {
        use web_sys::console;
        
        console::log_1(&"[BEVY] poll_regeneration_request_system: Entered system.".into());

        let mut requested = false;
        if let Ok(mut req_guard) = REGENERATION_REQUESTED.lock() {
            if *req_guard {
                console::log_1(&"[BEVY] poll_regeneration_request_system: Request detected.".into());
                requested = true;
                *req_guard = false; // Reset the flag
            }
        }

        if requested {
            if let Ok(shared_config) = SHARED_GRAPH_CONFIG.lock() {
                console::log_1(&format!("[BEVY] Updating GraphConfig resource from shared: {:?}", *shared_config).into());
                graph_config_res.num_nodes = shared_config.num_nodes;
                graph_config_res.num_edges = shared_config.num_edges;
                event_writer.write(RegenerateGraphEvent);
                console::log_1(&"[BEVY] RegenerateGraphEvent sent.".into());
            } else {
                console::log_1(&"[BEVY] Failed to lock SHARED_GRAPH_CONFIG in poll system.".into());
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn simple_update_logger(
    mut graph_config_res: ResMut<GraphConfig>,
    mut event_writer: EventWriter<RegenerateGraphEvent>,
) {
    // This log should appear on every frame if the Update schedule is running.
    console::log_1(&"[BEVY_DEBUG] Update loop is ticking.".into());
    
    // Also handle regeneration polling here
    let mut requested = false;
    if let Ok(mut req_guard) = REGENERATION_REQUESTED.lock() {
        if *req_guard {
            console::log_1(&"[BEVY] Regeneration request detected in simple_update_logger!".into());
            requested = true;
            *req_guard = false; // Reset the flag
        }
    }

    if requested {
        if let Ok(shared_config) = SHARED_GRAPH_CONFIG.lock() {
            console::log_1(&format!("[BEVY] Updating GraphConfig resource from shared: {:?}", *shared_config).into());
            graph_config_res.num_nodes = shared_config.num_nodes;
            graph_config_res.num_edges = shared_config.num_edges;
            event_writer.write(RegenerateGraphEvent);
            console::log_1(&"[BEVY] RegenerateGraphEvent sent from simple_update_logger!".into());
        } else {
            console::log_1(&"[BEVY] Failed to lock SHARED_GRAPH_CONFIG in simple_update_logger.".into());
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn simple_update_logger() {
    // This log should appear on every frame if the Update schedule is running.
    println!("[BEVY_DEBUG] Update loop is ticking.");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn run() {
    console_error_panic_hook::set_once();
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .insert_resource(ClearColor(Color::srgb(0.2, 0.2, 0.3)))
        .insert_resource(GraphConfig::default())
        .add_event::<RegenerateGraphEvent>()
        .add_systems(Startup, (setup::graph::setup, simple_update_logger)) // Added logger to Startup
        // .add_systems(PreUpdate, poll_regeneration_request_system) // Reverted from PreUpdate
        .add_systems(Update, (
            simple_update_logger, // Logger also in Update
            poll_regeneration_request_system,
            systems::camera_controls::keyboard_input_system,
            systems::camera_controls::mouse_pan_system,
            systems::camera_controls::touch_pan_system,
            systems::camera_controls::mouse_zoom_system,
            systems::graph_rendering::graph_rendering_system,
            systems::ui_systems::handle_regeneration_event_system,
        ));
    app.run();
}

// This is the main app runner for native
pub fn run_app() {
    let mut app = App::new();
     app.add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .insert_resource(ClearColor(Color::srgb(0.2, 0.2, 0.3)))
        .insert_resource(GraphConfig::default())
        .add_event::<RegenerateGraphEvent>()
        .add_systems(Startup, (setup::graph::setup, simple_update_logger)) // Added logger to Startup for native too for consistency
        .add_systems(Update, (
            simple_update_logger, // Logger also in Update for native
            systems::camera_controls::keyboard_input_system,
            systems::camera_controls::mouse_pan_system,
            systems::camera_controls::touch_pan_system,
            systems::camera_controls::mouse_zoom_system,
            systems::graph_rendering::graph_rendering_system,
            systems::ui_systems::handle_regeneration_event_system,
        ));
    app.run();
}
