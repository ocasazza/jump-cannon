use eframe::egui;

use crate::ui::state::{AppState, ColorBy, SizeBy};

use super::subgroup_label;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        let avail = ui.available_size_before_wrap();
        ui.add_space(avail.x - 58.0);
        if ui.small_button("↺ Reset").clicked() {
            state.style = Default::default();
        }
    });
    subgroup_label(ui, "Size by");
    egui::ComboBox::from_id_salt("style-size-by")
        .selected_text(state.style.size_by.label())
        .show_ui(ui, |ui| {
            for &v in SizeBy::ALL {
                ui.selectable_value(&mut state.style.size_by, v, v.label());
            }
        });

    ui.add_space(8.0);
    subgroup_label(ui, "Color by");
    egui::ComboBox::from_id_salt("style-color-by")
        .selected_text(state.style.color_by.label())
        .show_ui(ui, |ui| {
            for &v in ColorBy::ALL {
                ui.selectable_value(&mut state.style.color_by, v, v.label());
            }
        });

    ui.add_space(8.0);
    subgroup_label(ui, "Size multiplier");
    ui.add(egui::Slider::new(&mut state.style.size_mul, 0.25..=4.0).text("×"));
}
