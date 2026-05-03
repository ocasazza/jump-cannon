use eframe::egui;

use crate::ui::state::AppState;

use super::hint_label;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            state.focus = Default::default();
        }
    });
    let f = &mut state.focus;
    ui.checkbox(&mut f.dof_enabled, "Depth-of-field");
    ui.add_enabled_ui(f.dof_enabled, |ui| {
        ui.add(egui::Slider::new(&mut f.distance, 0.0..=1000.0).text("distance"));
        ui.add(egui::Slider::new(&mut f.thickness, 1.0..=500.0).text("thickness"));
        ui.add(egui::Slider::new(&mut f.blur, 0.0..=4.0).text("blur"));
        ui.add(egui::Slider::new(&mut f.max_coc, 0.0..=32.0).text("max CoC"));
    });
    ui.add_space(8.0);
    hint_label(ui, "DoF off → cosmograph-style sharp dots; on → microscope bokeh");
}
