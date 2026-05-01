use eframe::egui;

use crate::ui::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("FOCUS");
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            state.focus = Default::default();
        }
    });
    ui.add_space(4.0);

    let f = &mut state.focus;
    ui.add(egui::Slider::new(&mut f.distance, 0.0..=1000.0).text("distance"));
    ui.add(egui::Slider::new(&mut f.thickness, 1.0..=500.0).text("thickness"));
    ui.add(egui::Slider::new(&mut f.blur, 0.0..=4.0).text("blur"));
    ui.add(egui::Slider::new(&mut f.max_coc, 0.0..=32.0).text("max CoC"));
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("F + scroll = focal plane like a microscope knob")
            .italics()
            .color(egui::Color32::from_gray(150))
            .size(11.0),
    );
}
