use eframe::egui;

use crate::ui::focus_set::FocusMode;
use crate::ui::state::AppState;
use crate::ui::theme::accent;

use super::{hint_label, reset_row, row, subgroup_label, subgroup_separator};

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    state.snapshot_source = Some("Camera".into());
    if reset_row(ui) {
        state.camera = Default::default();
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let _ = ui.button("Fit");
        let _ = ui.button("Reset");
    });
    ui.add_space(4.0);

    subgroup_separator(ui);

    let c = &mut state.camera;
    row(ui, "Invert mouse X", |ui| { ui.checkbox(&mut c.invert_mouse_x, ""); });
    row(ui, "Invert mouse Y", |ui| { ui.checkbox(&mut c.invert_mouse_y, ""); });
    row(ui, "Invert A/D",     |ui| { ui.checkbox(&mut c.invert_ad, ""); });
    row(ui, "Invert Q/E",     |ui| { ui.checkbox(&mut c.invert_qe, ""); });

    ui.add_space(10.0);

    // Follow centroid: blue tint on the row label when active.
    let _ = accent::BLUE;
    row(ui, "Follow centroid", |ui| { ui.checkbox(&mut c.follow_centroid, ""); });
    row(ui, "Fit to window",   |ui| { ui.checkbox(&mut c.fit_to_window, ""); });

    // ---- Focus subgroup (merged from former Section::Focus) ---------
    subgroup_separator(ui);
    subgroup_label(ui, "Focus");

    let f = &mut state.focus;

    row(ui, "Focus mode", |ui| {
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
    });
    hint_label(
        ui,
        "Hover or click a node → that node + its community light up; \
         everything else dims. Click empty canvas to clear.",
    );

    subgroup_separator(ui);

    // ---- DoF subgroup -----------------------------------------------
    subgroup_label(ui, "Depth of field");
    row(ui, "Enabled", |ui| {
        ui.checkbox(&mut f.dof_enabled, "");
    });
    ui.add_enabled_ui(f.dof_enabled, |ui| {
        row(ui, "distance", |ui| {
            ui.add(egui::Slider::new(&mut f.distance, 0.0..=1000.0));
        });
        row(ui, "thickness", |ui| {
            ui.add(egui::Slider::new(&mut f.thickness, 1.0..=500.0));
        });
        row(ui, "blur", |ui| {
            ui.add(egui::Slider::new(&mut f.blur, 0.0..=4.0));
        });
        row(ui, "max CoC", |ui| {
            ui.add(egui::Slider::new(&mut f.max_coc, 0.0..=32.0));
        });
    });
    ui.add_space(8.0);
    hint_label(ui, "DoF off → cosmograph-style sharp dots; on → microscope bokeh");
}
