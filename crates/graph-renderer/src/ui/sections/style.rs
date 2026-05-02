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

    ui.add_space(12.0);
    subgroup_label(ui, "Edge color");
    let mut rgba = egui::Rgba::from_rgba_unmultiplied(
        state.style.edge_color[0],
        state.style.edge_color[1],
        state.style.edge_color[2],
        state.style.edge_color[3],
    );
    if egui::color_picker::color_edit_button_rgba(
        ui,
        &mut rgba,
        egui::color_picker::Alpha::OnlyBlend,
    )
    .changed()
    {
        state.style.edge_color = [rgba.r(), rgba.g(), rgba.b(), rgba.a()];
    }

    ui.add_space(8.0);
    subgroup_label(ui, "Edge density");
    ui.add(egui::Slider::new(&mut state.style.edge_alpha_mul, 0.0..=2.0).text("α×"));

    ui.add_space(8.0);
    subgroup_label(ui, "Edge distance range");
    ui.add(egui::Slider::new(&mut state.style.edge_dist_min, 0.0..=200.0).text("min"));
    ui.add(egui::Slider::new(&mut state.style.edge_dist_max, 50.0..=2000.0).text("max"));

    ui.add_space(8.0);
    subgroup_label(ui, "Edge min visibility");
    ui.add(egui::Slider::new(&mut state.style.edge_min_transparency, 0.0..=1.0).text(""));
}
