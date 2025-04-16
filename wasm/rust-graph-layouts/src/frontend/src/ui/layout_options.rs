use crate::app::App;
use eframe::egui;
use shared::types::LayoutAlgorithm;

/// Render the layout options section
pub fn render(app: &mut App, ui: &mut egui::Ui) {
    ui.collapsing("Layout Options", |ui| {
        ui.horizontal(|ui| {
            ui.label("Layout Algorithm:");
            let combo_response = egui::ComboBox::from_id_source("layout_algorithm")
                .selected_text(match &app.layout {
                    shared::types::LayoutAlgorithm::Fcose(_) => "fCoSE",
                    shared::types::LayoutAlgorithm::CoseBilkent(_) => "CoSE Bilkent",
                    shared::types::LayoutAlgorithm::Cise(_) => "CiSE",
                    shared::types::LayoutAlgorithm::Concentric(_) => "Concentric",
                    shared::types::LayoutAlgorithm::KlayLayered(_) => "KLay Layered",
                    shared::types::LayoutAlgorithm::Dagre(_) => "Dagre",
                })
                .show_ui(ui, |ui| {
                    let mut changed = false;

                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::Fcose(_)),
                            "fCoSE",
                        )
                        .clicked()
                    {
                        app.layout = shared::types::LayoutAlgorithm::Fcose(Default::default());
                        changed = true;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::CoseBilkent(_)),
                            "CoSE Bilkent",
                        )
                        .clicked()
                    {
                        app.layout =
                            shared::types::LayoutAlgorithm::CoseBilkent(Default::default());
                        changed = true;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::Cise(_)),
                            "CiSE",
                        )
                        .clicked()
                    {
                        app.layout = shared::types::LayoutAlgorithm::Cise(Default::default());
                        changed = true;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::Concentric(_)),
                            "Concentric",
                        )
                        .clicked()
                    {
                        app.layout = shared::types::LayoutAlgorithm::Concentric(Default::default());
                        changed = true;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::KlayLayered(_)),
                            "KLay Layered",
                        )
                        .clicked()
                    {
                        app.layout =
                            shared::types::LayoutAlgorithm::KlayLayered(Default::default());
                        changed = true;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.layout, shared::types::LayoutAlgorithm::Dagre(_)),
                            "Dagre",
                        )
                        .clicked()
                    {
                        app.layout = shared::types::LayoutAlgorithm::Dagre(Default::default());
                        changed = true;
                    }

                    changed
                });

            if combo_response.inner.unwrap_or(false) {
                app.schedule_layout_update();
            }
        });

        // Dynamic layout options based on selected algorithm
        let mut changed = false;

        match &mut app.layout {
            shared::types::LayoutAlgorithm::Fcose(options) => {
                changed |= render_fcose_options(ui, options);
            }
            shared::types::LayoutAlgorithm::CoseBilkent(options) => {
                changed |= render_cose_bilkent_options(ui, options);
            }
            shared::types::LayoutAlgorithm::Cise(options) => {
                changed |= render_cise_options(ui, options);
            }
            shared::types::LayoutAlgorithm::Concentric(options) => {
                changed |= render_concentric_options(ui, options);
            }
            shared::types::LayoutAlgorithm::KlayLayered(options) => {
                changed |= render_klay_options(ui, options);
            }
            shared::types::LayoutAlgorithm::Dagre(options) => {
                changed |= render_dagre_options(ui, options);
            }
        }

        // Common layout options
        if let Some(base_options) = match &mut app.layout {
            shared::types::LayoutAlgorithm::Fcose(options) => Some(&mut options.base),
            shared::types::LayoutAlgorithm::CoseBilkent(options) => Some(&mut options.base),
            shared::types::LayoutAlgorithm::Cise(options) => Some(&mut options.base),
            shared::types::LayoutAlgorithm::Concentric(options) => Some(&mut options.base),
            shared::types::LayoutAlgorithm::KlayLayered(options) => Some(&mut options.base),
            shared::types::LayoutAlgorithm::Dagre(options) => Some(&mut options.base),
        } {
            changed |= render_common_options(ui, base_options);
        }

        if changed {
            app.schedule_layout_update();
        }

        if ui.button("Apply Layout").clicked() {
            app.apply_layout();
        }
    });
}

/// Render fCoSE layout options
fn render_fcose_options(
    ui: &mut egui::Ui,
    options: &mut shared::types::FcoseLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(
            egui::Slider::new(&mut options.node_repulsion, 1000.0..=10000.0).text("Node Repulsion"),
        )
        .changed();
    changed |= ui
        .add(
            egui::Slider::new(&mut options.ideal_edge_length, 10.0..=200.0)
                .text("Ideal Edge Length"),
        )
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.node_overlap, 0.0..=20.0).text("Node Overlap"))
        .changed();

    let combo_response = egui::ComboBox::from_id_source("fcose_quality")
        .selected_text(&options.quality)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut options.quality, "draft".to_string(), "Draft")
                .clicked()
                || ui
                    .selectable_value(&mut options.quality, "default".to_string(), "Default")
                    .clicked()
                || ui
                    .selectable_value(&mut options.quality, "proof".to_string(), "Proof")
                    .clicked()
        });

    changed |= combo_response.inner.unwrap_or(false);

    changed
}

/// Render CoSE Bilkent layout options
fn render_cose_bilkent_options(
    ui: &mut egui::Ui,
    options: &mut shared::types::CoseBilkentLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(
            egui::Slider::new(&mut options.node_repulsion, 1000.0..=10000.0).text("Node Repulsion"),
        )
        .changed();
    changed |= ui
        .add(
            egui::Slider::new(&mut options.ideal_edge_length, 10.0..=200.0)
                .text("Ideal Edge Length"),
        )
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.node_overlap, 0.0..=20.0).text("Node Overlap"))
        .changed();

    changed
}

/// Render CiSE layout options
fn render_cise_options(ui: &mut egui::Ui, options: &mut shared::types::CiseLayoutOptions) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(egui::Slider::new(&mut options.circle_spacing, 5.0..=50.0).text("Circle Spacing"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.node_spacing, 5.0..=30.0).text("Node Spacing"))
        .changed();

    changed
}

/// Render Concentric layout options
fn render_concentric_options(
    ui: &mut egui::Ui,
    options: &mut shared::types::ConcentricLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(egui::Slider::new(&mut options.min_node_spacing, 5.0..=50.0).text("Min Node Spacing"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.level_width, 50.0..=200.0).text("Level Width"))
        .changed();

    let combo_response = egui::ComboBox::from_id_source("concentric_by")
        .selected_text(&options.concentric_by)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut options.concentric_by, "degree".to_string(), "Degree")
                .clicked()
                || ui
                    .selectable_value(&mut options.concentric_by, "id".to_string(), "ID")
                    .clicked()
        });

    changed |= combo_response.inner.unwrap_or(false);

    changed
}

/// Render KLay layout options
fn render_klay_options(
    ui: &mut egui::Ui,
    options: &mut shared::types::KlayLayeredLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(egui::Slider::new(&mut options.layer_spacing, 20.0..=100.0).text("Layer Spacing"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.node_spacing, 10.0..=50.0).text("Node Spacing"))
        .changed();

    let placement_response = egui::ComboBox::from_id_source("klay_node_placement")
        .selected_text(&options.node_placement)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut options.node_placement, "SIMPLE".to_string(), "Simple")
                .clicked()
                || ui
                    .selectable_value(
                        &mut options.node_placement,
                        "LINEAR_SEGMENTS".to_string(),
                        "Linear Segments",
                    )
                    .clicked()
                || ui
                    .selectable_value(
                        &mut options.node_placement,
                        "BRANDES_KOEPF".to_string(),
                        "Brandes-Koepf",
                    )
                    .clicked()
        });

    changed |= placement_response.inner.unwrap_or(false);

    let routing_response = egui::ComboBox::from_id_source("klay_edge_routing")
        .selected_text(&options.edge_routing)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut options.edge_routing,
                "ORTHOGONAL".to_string(),
                "Orthogonal",
            )
            .clicked()
                || ui
                    .selectable_value(&mut options.edge_routing, "SPLINES".to_string(), "Splines")
                    .clicked()
                || ui
                    .selectable_value(
                        &mut options.edge_routing,
                        "POLYLINE".to_string(),
                        "Polyline",
                    )
                    .clicked()
        });

    changed |= routing_response.inner.unwrap_or(false);
    changed |= ui
        .checkbox(&mut options.merge_edges, "Merge Parallel Edges")
        .changed();

    changed
}

/// Render Dagre layout options
fn render_dagre_options(
    ui: &mut egui::Ui,
    options: &mut shared::types::DagreLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui
        .add(egui::Slider::new(&mut options.node_separation, 20.0..=100.0).text("Node Separation"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut options.rank_separation, 20.0..=100.0).text("Rank Separation"))
        .changed();

    let direction_response = egui::ComboBox::from_id_source("dagre_rank_direction")
        .selected_text(&options.rank_direction)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut options.rank_direction,
                "TB".to_string(),
                "Top to Bottom",
            )
            .clicked()
                || ui
                    .selectable_value(
                        &mut options.rank_direction,
                        "BT".to_string(),
                        "Bottom to Top",
                    )
                    .clicked()
                || ui
                    .selectable_value(
                        &mut options.rank_direction,
                        "LR".to_string(),
                        "Left to Right",
                    )
                    .clicked()
                || ui
                    .selectable_value(
                        &mut options.rank_direction,
                        "RL".to_string(),
                        "Right to Left",
                    )
                    .clicked()
        });

    changed |= direction_response.inner.unwrap_or(false);

    let align_response = egui::ComboBox::from_id_source("dagre_align")
        .selected_text(&options.align)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut options.align, "UL".to_string(), "Up-Left")
                .clicked()
                || ui
                    .selectable_value(&mut options.align, "UR".to_string(), "Up-Right")
                    .clicked()
                || ui
                    .selectable_value(&mut options.align, "DL".to_string(), "Down-Left")
                    .clicked()
                || ui
                    .selectable_value(&mut options.align, "DR".to_string(), "Down-Right")
                    .clicked()
        });

    changed |= align_response.inner.unwrap_or(false);

    let ranker_response = egui::ComboBox::from_id_source("dagre_ranker")
        .selected_text(&options.ranker)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut options.ranker,
                "network-simplex".to_string(),
                "Network Simplex",
            )
            .clicked()
                || ui
                    .selectable_value(&mut options.ranker, "tight-tree".to_string(), "Tight Tree")
                    .clicked()
                || ui
                    .selectable_value(
                        &mut options.ranker,
                        "longest-path".to_string(),
                        "Longest Path",
                    )
                    .clicked()
        });

    changed |= ranker_response.inner.unwrap_or(false);
    changed |= ui.checkbox(&mut options.acyclic, "Remove Cycles").changed();

    changed
}

/// Render common layout options
fn render_common_options(
    ui: &mut egui::Ui,
    base_options: &mut shared::types::BaseLayoutOptions,
) -> bool {
    // Track changes to trigger layout update
    let mut changed = false;

    changed |= ui.checkbox(&mut base_options.animate, "Animate").changed();
    if base_options.animate {
        changed |= ui
            .add(
                egui::Slider::new(&mut base_options.animation_duration, 100..=2000)
                    .text("Animation Duration (ms)"),
            )
            .changed();
    }
    changed |= ui
        .checkbox(&mut base_options.fit, "Fit to Screen")
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut base_options.padding, 0..=100).text("Padding"))
        .changed();

    changed
}
