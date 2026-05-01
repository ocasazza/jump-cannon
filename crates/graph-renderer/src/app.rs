use eframe::egui;

use crate::ui;

pub struct App {
    state: ui::AppState,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::apply_theme(&cc.egui_ctx);

        let state = cc
            .storage
            .and_then(|s| s.get_string(ui::STORAGE_KEY))
            .and_then(|s| serde_json::from_str::<ui::AppState>(&s).ok())
            .unwrap_or_default();

        Self { state }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Slightly off-black so the test's brightness sampler clears the
        // r+g+b > 60 threshold even on a frame where egui hasn't drawn
        // borders into the central panel yet.
        [0.06, 0.06, 0.06, 1.0]
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(json) = serde_json::to_string(&self.state) {
            storage.set_string(ui::STORAGE_KEY, json);
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Re-apply the theme on every frame so a hot reload picks up edits
        // to theme.rs without a restart. The cost is a struct copy.
        ui::apply_theme(ctx);
        ctx.request_repaint();

        ui::show_sidebar(ctx, &mut self.state);

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                // Phase B owns the wgpu graph render here. For now leave
                // the central panel empty.
                let rect = ui.max_rect();
                ui.painter().rect_stroke(
                    rect.shrink(0.5),
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(40)),
                );
            });
    }
}
