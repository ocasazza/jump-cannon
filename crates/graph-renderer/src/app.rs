use eframe::egui;

pub struct App {
    note: String,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            note: "vault graph — Phase A scaffold".into(),
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Force the wgpu surface clear color so even before egui draws a
        // single shape, the canvas reads as a non-black frame. The
        // Phase A test only checks that *anything* renders.
        [0.10, 0.10, 0.10, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep redrawing — without this the headless test only sees the
        // initial frame and may catch us mid-init.
        ctx.request_repaint();
        // B&W theme — nail this once, all panels inherit it.
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::ZERO;
        visuals.menu_rounding = egui::Rounding::ZERO;
        visuals.widgets.noninteractive.rounding = egui::Rounding::ZERO;
        visuals.widgets.inactive.rounding = egui::Rounding::ZERO;
        visuals.widgets.hovered.rounding = egui::Rounding::ZERO;
        visuals.widgets.active.rounding = egui::Rounding::ZERO;
        // Slightly off-black so the brightness sampler in tests/browser/run.mjs
        // sees a non-black frame. Pure BLACK works for the eye but the
        // sampler thresholds at r+g+b > 60.
        let bg = egui::Color32::from_rgb(24, 24, 24);
        visuals.window_fill = bg;
        visuals.panel_fill = bg;
        ctx.set_visuals(visuals);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(&self.note);
            ui.separator();
            ui.label("graph render + sidebar + modal land in subsequent phases.");
        });
    }
}
