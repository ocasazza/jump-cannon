mod app;
mod app_graph;
mod utils;
mod ui;
mod renderer;
mod layout;
mod file_parser;

use eframe::egui;

fn main() -> eframe::Result<()> {
    env_logger::init();
    
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(1200.0, 800.0)),
        ..Default::default()
    };
    
    eframe::run_native(
        "Graph Visualization",
        options,
        Box::new(|_cc| Box::new(app::App::default())),
    )
}
