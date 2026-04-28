use bevy_egui::{egui, EguiContexts};

pub fn status_bar_system(mut contexts: EguiContexts) {
    let ctx = contexts.ctx_mut();

    egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label("jump-cannon");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label("Ctrl+P: command palette");
            });
        });
    });
}
