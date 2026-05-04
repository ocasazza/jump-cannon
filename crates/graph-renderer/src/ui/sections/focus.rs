use eframe::egui;

use crate::ui::focus_set::FocusMode;
use crate::ui::state::AppState;

use super::{hint_label, reset_row, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if reset_row(ui) {
        state.focus = Default::default();
    }
    let f = &mut state.focus;

    // ---- Focus mode subgroup ----------------------------------------
    subgroup_label(ui, "Focus mode");
    ui.add_space(2.0);
    let current = f.focus_mode;
    egui::ComboBox::from_id_salt("focus-mode-combo")
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for &mode in FocusMode::ALL {
                let resp = ui.add_enabled(
                    mode.enabled(),
                    egui::SelectableLabel::new(current == mode, mode.label()),
                );
                if !mode.enabled() {
                    resp.clone().on_hover_text("(needs vault meta cache)");
                }
                if resp.clicked() && mode.enabled() {
                    f.focus_mode = mode;
                }
            }
        });
    hint_label(
        ui,
        "Hover or click a node → that node + its community light up; \
         everything else dims. Click empty canvas to clear.",
    );

    subgroup_separator(ui);

    // ---- DoF subgroup -----------------------------------------------
    subgroup_label(ui, "Depth of field");
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
