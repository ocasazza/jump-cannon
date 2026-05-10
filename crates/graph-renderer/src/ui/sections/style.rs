use eframe::egui;

use crate::data::PaletteId;
use crate::ui::state::{AppState, ColorBy, EdgeColorBy, SizeBy};

use super::{reset_row, row};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if reset_row(ui) {
        state.style = Default::default();
    }

    row(ui, "Size by", |ui| {
        egui::ComboBox::from_id_salt("style-size-by")
            .selected_text(state.style.size_by.label())
            .show_ui(ui, |ui| {
                for &v in SizeBy::ALL {
                    ui.selectable_value(&mut state.style.size_by, v, v.label());
                }
            });
    });

    row(ui, "Color by", |ui| {
        egui::ComboBox::from_id_salt("style-color-by")
            .selected_text(state.style.color_by.label())
            .show_ui(ui, |ui| {
                for &v in ColorBy::ALL {
                    ui.selectable_value(&mut state.style.color_by, v, v.label());
                }
            });
    });

    row(ui, "Palette", |ui| {
        egui::ComboBox::from_id_salt("style-palette")
            .selected_text(state.style.palette.label())
            .show_ui(ui, |ui| {
                for &p in PaletteId::ALL {
                    ui.selectable_value(&mut state.style.palette, p, p.label());
                }
            });
    });

    row(ui, "Node size multiplier", |ui| {
        ui.add(egui::Slider::new(&mut state.style.size_mul, 0.25..=4.0).text("×"));
    });

    row(ui, "Edge size multiplier", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_size_mul, 0.25..=4.0).text("×"));
    });

    row(ui, "Log scale (10^(v−1))", |ui| {
        ui.checkbox(&mut state.style.log_scale_size, "");
    });

    row(ui, "Shader intensity", |ui| {
        ui.add(egui::Slider::new(&mut state.style.shader_intensity, 0.0..=4.0).text("×"));
    });

    row(ui, "Edge color by", |ui| {
        egui::ComboBox::from_id_salt("style-edge-color-by")
            .selected_text(state.style.edge_color_by.label())
            .show_ui(ui, |ui| {
                for &v in EdgeColorBy::ALL {
                    ui.selectable_value(&mut state.style.edge_color_by, v, v.label());
                }
            });
    });

    row(ui, "Edge color", |ui| {
        let uniform_active = state.style.edge_color_by == EdgeColorBy::None;
        let mut rgba = egui::Rgba::from_rgba_unmultiplied(
            state.style.edge_color[0],
            state.style.edge_color[1],
            state.style.edge_color[2],
            state.style.edge_color[3],
        );
        let resp = ui
            .add_enabled_ui(uniform_active, |ui| {
                egui::color_picker::color_edit_button_rgba(
                    ui,
                    &mut rgba,
                    egui::color_picker::Alpha::OnlyBlend,
                )
            })
            .inner;
        if resp.changed() {
            state.style.edge_color = [rgba.r(), rgba.g(), rgba.b(), rgba.a()];
        }
    });

    row(ui, "Edge width (px)", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_width, 0.5..=8.0).text("px"));
    });

    row(ui, "Edge density", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_alpha_mul, 0.0..=2.0).text("α×"));
    });

    // Two sliders share one logical "Edge distance range" label — keep
    // them on separate rows so each slider gets its own grow space.
    row(ui, "Edge distance min", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_dist_min, 0.0..=200.0).text("min"));
    });
    row(ui, "Edge distance max", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_dist_max, 50.0..=2400.0).text("max"));
    });

    row(ui, "Edge min visibility", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_min_transparency, 0.0..=1.0));
    });

    row(ui, "Long-distance fade floor", |ui| {
        ui.add(egui::Slider::new(&mut state.style.edge_fade_floor, 0.0..=0.5));
    });
}
