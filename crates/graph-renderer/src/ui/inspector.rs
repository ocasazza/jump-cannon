//! Right-hand collapsible inspector sidebar.
//!
//! Shows the currently-selected node's metadata (id, degree, pagerank,
//! community, kcore — whatever lives in the per-node `metrics` map),
//! plus a clickable list of community siblings and a clickable list of
//! direct neighbors derived from the raw edge list.
//!
//! The panel sits to the right of the central dock area. Two states:
//!   - Collapsed: a thin 24px strip with a chevron to expand.
//!   - Expanded: 320px panel with sections.
//!
//! Communication back to `App` flows through `InspectorData::requested_selection`:
//! clicking a row in either list writes that node's idx; `App::update`
//! drains it on the next frame and applies the same selection-change
//! path the canvas click uses.

use eframe::egui;
use std::collections::HashMap;

use super::state::AppState;

const PANEL_W: f32 = 320.0;
const COLLAPSED_W: f32 = 24.0;

/// Read-only context the inspector uses to resolve node info, plus a
/// single mutable out-channel for click-to-select.
pub struct InspectorData<'a> {
    pub ids: &'a [String],
    pub metrics: &'a HashMap<String, Vec<f32>>,
    pub edges: &'a [u32], // packed [src, tgt, src, tgt, ...]
    pub selected_idx: Option<u32>,
    pub requested_selection: &'a mut Option<u32>,
}

pub fn show(ctx: &egui::Context, state: &mut AppState, data: &mut InspectorData) {
    if state.inspector_open {
        show_expanded(ctx, state, data);
    } else {
        show_collapsed(ctx, state);
    }
}

fn show_collapsed(ctx: &egui::Context, state: &mut AppState) {
    egui::SidePanel::right("inspector")
        .exact_width(COLLAPSED_W)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::BLACK)
                .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
                .inner_margin(egui::Margin::ZERO),
        )
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                let resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new("\u{2039}").color(egui::Color32::WHITE),
                    )
                    .frame(false)
                    .min_size(egui::vec2(COLLAPSED_W, 22.0)),
                )
                .on_hover_text("Open inspector");
                if resp.clicked() {
                    state.inspector_open = true;
                }
            });
        });
}

fn show_expanded(ctx: &egui::Context, state: &mut AppState, data: &mut InspectorData) {
    egui::SidePanel::right("inspector")
        .exact_width(PANEL_W)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(egui::Color32::BLACK)
                .stroke(egui::Stroke::new(1.0, egui::Color32::WHITE))
                .inner_margin(egui::Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 12.0,
                    bottom: 12.0,
                }),
        )
        .show(ctx, |ui| {
            // Header: title + collapse chevron.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Inspector")
                        .color(egui::Color32::WHITE)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("\u{203A}")
                                    .color(egui::Color32::WHITE),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Collapse")
                        .clicked()
                    {
                        state.inspector_open = false;
                    }
                });
            });
            ui.separator();

            let Some(idx) = data.selected_idx else {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Click a node to inspect")
                        .color(egui::Color32::from_gray(160))
                        .italics(),
                );
                return;
            };

            let n = data.ids.len();
            if (idx as usize) >= n {
                ui.label("(invalid selection)");
                return;
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    show_metadata(ui, idx, data);
                    ui.add_space(8.0);
                    show_community(ui, idx, data);
                    ui.add_space(8.0);
                    show_neighbors(ui, idx, data);
                });
        });
}

fn show_metadata(ui: &mut egui::Ui, idx: u32, data: &InspectorData) {
    let id = data.ids.get(idx as usize).cloned().unwrap_or_default();
    ui.add(
        egui::Label::new(
            egui::RichText::new(&id)
                .color(egui::Color32::WHITE)
                .strong()
                .monospace(),
        )
        .wrap(),
    );
    ui.add_space(4.0);

    egui::Grid::new("inspector-meta-grid")
        .num_columns(2)
        .spacing([10.0, 3.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("idx").color(egui::Color32::from_gray(170)));
            ui.label(format!("{}", idx));
            ui.end_row();

            for key in ["degree", "pagerank", "community", "kcore", "recency"] {
                if let Some(vec) = data.metrics.get(key) {
                    if let Some(&v) = vec.get(idx as usize) {
                        ui.label(
                            egui::RichText::new(key).color(egui::Color32::from_gray(170)),
                        );
                        let text = if key == "community" || key == "degree" || key == "kcore" {
                            format!("{}", v as i64)
                        } else {
                            format!("{:.4}", v)
                        };
                        ui.label(text);
                        ui.end_row();
                    }
                }
            }
        });
}

fn show_community(ui: &mut egui::Ui, idx: u32, data: &mut InspectorData) {
    let Some(comm_vec) = data.metrics.get("community") else {
        ui.label(
            egui::RichText::new("(no community metric)")
                .color(egui::Color32::from_gray(140))
                .italics(),
        );
        return;
    };
    let Some(&my_comm_f) = comm_vec.get(idx as usize) else { return };
    let my_comm = my_comm_f as i64;

    // Collect siblings (same community id, excluding self).
    let mut siblings: Vec<u32> = comm_vec
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| {
            if v as i64 == my_comm && i as u32 != idx {
                Some(i as u32)
            } else {
                None
            }
        })
        .collect();
    // Stable order, capped to keep the UI snappy on huge communities.
    siblings.sort_unstable();

    let header = format!("Community {} ({} members)", my_comm, siblings.len());
    egui::CollapsingHeader::new(
        egui::RichText::new(header).color(egui::Color32::WHITE),
    )
    .default_open(true)
    .show(ui, |ui| {
        if siblings.is_empty() {
            ui.label(
                egui::RichText::new("(no siblings)")
                    .color(egui::Color32::from_gray(140))
                    .italics(),
            );
            return;
        }
        clickable_list(ui, "comm-list", &siblings, data);
    });
}

fn show_neighbors(ui: &mut egui::Ui, idx: u32, data: &mut InspectorData) {
    // Walk the packed [src, tgt] edge list, collecting unique neighbors.
    use std::collections::HashSet;
    let mut set: HashSet<u32> = HashSet::new();
    for chunk in data.edges.chunks_exact(2) {
        let (s, t) = (chunk[0], chunk[1]);
        if s == idx {
            set.insert(t);
        } else if t == idx {
            set.insert(s);
        }
    }
    set.remove(&idx);
    let mut neighbors: Vec<u32> = set.into_iter().collect();
    neighbors.sort_unstable();

    let header = format!("Neighbors ({})", neighbors.len());
    egui::CollapsingHeader::new(
        egui::RichText::new(header).color(egui::Color32::WHITE),
    )
    .default_open(true)
    .show(ui, |ui| {
        if neighbors.is_empty() {
            ui.label(
                egui::RichText::new("(no neighbors)")
                    .color(egui::Color32::from_gray(140))
                    .italics(),
            );
            return;
        }
        clickable_list(ui, "neighbor-list", &neighbors, data);
    });
}

fn clickable_list(
    ui: &mut egui::Ui,
    id_source: &str,
    items: &[u32],
    data: &mut InspectorData,
) {
    egui::ScrollArea::vertical()
        .id_salt(id_source)
        .max_height(220.0)
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Cap entries rendered per pass; very large communities would
            // otherwise hand egui thousands of widgets per frame.
            const MAX: usize = 500;
            let truncated = items.len() > MAX;
            for &i in items.iter().take(MAX) {
                let label = data
                    .ids
                    .get(i as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                let resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new(label)
                            .monospace()
                            .color(egui::Color32::WHITE),
                    )
                    .frame(false)
                    .wrap(),
                );
                if resp.clicked() {
                    *data.requested_selection = Some(i);
                }
            }
            if truncated {
                ui.label(
                    egui::RichText::new(format!("… {} more not shown", items.len() - MAX))
                        .color(egui::Color32::from_gray(140))
                        .italics(),
                );
            }
        });
}
