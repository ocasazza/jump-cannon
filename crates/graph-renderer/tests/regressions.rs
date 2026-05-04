//! Headless egui regression unit tests.
//!
//! Each test below pins a UI bug we already paid for once — if the
//! assertion fires, the named regression is back. Test names use the
//! issue tag from the original report so the failure message reads
//! like the bug's own headline.
//!
//! Driver: `egui_kittest = "0.30"` (matches the workspace's `egui` 0.30
//! pin). The harness runs a real egui pass headlessly, exposes the
//! AccessKit tree for hit-testing, and lets us read the cursor +
//! widget rects after layout. Wasm target ignores dev-deps cleanly,
//! so this file is native-only by construction.
//!
//! Compat note: enabling `egui_kittest` 0.30 forces egui's `accesskit`
//! feature on; eframe's `accesskit` feature has been added to the
//! workspace's eframe pin so egui-winit unifies cleanly. No production
//! behaviour change — the feature is dormant unless an accesskit
//! consumer is wired up at runtime.

use std::cell::Cell;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use graph_renderer::perf::PerfCollector;
use graph_renderer::ui::actions::ActionRegistry;
use graph_renderer::ui::focus_set::{self, FocusCtx, FocusMode};
use graph_renderer::ui::inspector::{self, InspectorData};
use graph_renderer::ui::layout::registry::LayoutRegistry;
use graph_renderer::ui::sections::{self, reset_row};
use graph_renderer::ui::state::{AppState, Section};

// ---------------------------------------------------------------------------
// 1. reset_row_does_not_eat_panel_height
// ---------------------------------------------------------------------------

#[test]
fn reset_row_does_not_eat_panel_height() {
    let visible_count = Cell::new(0_usize);
    let panel_bottom = Cell::new(f32::INFINITY);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(800.0, 600.0))
        .build(|ctx| {
            egui::SidePanel::left("test-panel")
                .exact_width(280.0)
                .show(ctx, |ui| {
                    let rect = ui.max_rect();
                    panel_bottom.set(rect.bottom());
                    let _ = reset_row(ui);
                    let mut count = 0_usize;
                    for i in 0..10 {
                        let resp = ui.label(format!("filler {i}"));
                        if resp.rect.bottom() <= panel_bottom.get() + 0.5 {
                            count += 1;
                        }
                    }
                    visible_count.set(count);
                });
        });

    harness.run();

    let n = visible_count.get();
    assert!(
        n >= 9,
        "reset_row regression: only {n}/10 fillers fit inside the \
         panel after the reset row — the right_to_left layout is \
         eating panel height again. panel_bottom={}",
        panel_bottom.get(),
    );
}

// ---------------------------------------------------------------------------
// 2. section_panel_renders_each_section
// ---------------------------------------------------------------------------

#[test]
fn section_panel_renders_each_section() {
    for section in Section::ALL.iter().copied() {
        let mut state = AppState::default();
        let mut registry = ActionRegistry::new();
        let layout_registry = LayoutRegistry::seed_default();
        let perf = PerfCollector::default();

        let start_y = Cell::new(0.0_f32);
        let end_y = Cell::new(0.0_f32);

        // Borrowing trick: move the whole context into the closure via a
        // RefCell so we can mutate state across multiple harness runs
        // without re-borrowing across the build() boundary.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build(|ctx| {
                egui::SidePanel::left("section-panel")
                    .exact_width(320.0)
                    .show(ctx, |ui| {
                        start_y.set(ui.cursor().min.y);
                        sections::show(
                            ui,
                            section,
                            &mut state,
                            &mut registry,
                            &layout_registry,
                            &perf,
                        );
                        end_y.set(ui.cursor().min.y);
                    });
            });
        harness.run();

        let advanced = end_y.get() - start_y.get();
        // Instances renders only a one-line hint when no command-palette
        // instance has been recorded yet (~40px including the section
        // header). Every other section has sliders / pickers / labels
        // and clears 100px easily. We assert > 30 for Instances and
        // >= 100 for everything else.
        let threshold = if matches!(section, Section::Instances) { 30.0 } else { 100.0 };
        assert!(
            advanced >= threshold,
            "section {:?} only advanced {advanced}px (threshold {threshold}) \
             — the section panel is collapsing. start_y={} end_y={}",
            section,
            start_y.get(),
            end_y.get(),
        );
    }
}

// ---------------------------------------------------------------------------
// 3. inspector_hidden_when_no_selection
// ---------------------------------------------------------------------------

#[test]
fn inspector_hidden_when_no_selection() {
    // Drive the inspector with selected_idx=None and assert no
    // SidePanel is registered under the "inspector" id. We use
    // `area_rect("inspector")` after a settled frame — None means no
    // panel ever mounted, which is what the early-return guarantees.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(800.0, 600.0))
        .build(|ctx| {
            let mut state = AppState::default();
            let ids: Vec<String> = vec!["a".into(), "b".into()];
            let metrics = std::collections::HashMap::new();
            let edges: Vec<u32> = Vec::new();
            let mut requested: Option<u32> = None;
            let mut req_toggle: Option<(String, String)> = None;
            let mut data = InspectorData {
                ids: &ids,
                metrics: &metrics,
                edges: &edges,
                selected_idx: None,
                requested_selection: &mut requested,
                node_meta: None,
                requested_filter_toggle: &mut req_toggle,
            };
            inspector::show(ctx, &mut state, &mut data);
        });
    harness.run();
    harness.run();

    let state = egui::containers::panel::PanelState::load(
        &harness.ctx,
        egui::Id::new("inspector"),
    );
    assert!(
        state.is_none(),
        "inspector regression: panel mounted with selected_idx=None \
         (panel_state = {state:?})",
    );
}

// ---------------------------------------------------------------------------
// 4. inspector_shown_when_selection
// ---------------------------------------------------------------------------

#[test]
fn inspector_shown_when_selection() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(800.0, 600.0))
        .build(|ctx| {
            let mut state = AppState::default();
            state.inspector_open = true;
            let ids: Vec<String> = vec!["alpha".into(), "beta".into()];
            let metrics = std::collections::HashMap::new();
            let edges: Vec<u32> = Vec::new();
            let mut requested: Option<u32> = None;
            let mut req_toggle: Option<(String, String)> = None;
            let mut data = InspectorData {
                ids: &ids,
                metrics: &metrics,
                edges: &edges,
                selected_idx: Some(0),
                requested_selection: &mut requested,
                node_meta: None,
                requested_filter_toggle: &mut req_toggle,
            };
            inspector::show(ctx, &mut state, &mut data);
        });
    harness.run();
    harness.run();

    let state = egui::containers::panel::PanelState::load(
        &harness.ctx,
        egui::Id::new("inspector"),
    )
    .expect(
        "inspector regression: panel did not mount when selected_idx=Some \
         and inspector_open=true",
    );
    assert!(
        state.rect.width() > 0.0,
        "inspector regression: panel mounted with zero width: {:?}",
        state.rect,
    );
}

// ---------------------------------------------------------------------------
// 5. gpu_force_defaults_match_spec
// ---------------------------------------------------------------------------

#[test]
fn gpu_force_defaults_match_spec() {
    use graph_layouts::{GpuForceOptions, RepulsionMode};
    let d = GpuForceOptions::default();
    assert_eq!(d.repulsion, 4000.0, "repulsion default drifted");
    assert_eq!(d.spring_k, 1.0, "spring_k default drifted");
    assert_eq!(d.spring_len, 400.0, "spring_len default drifted");
    assert!(
        (d.damping - 0.90).abs() < 1e-6,
        "damping default drifted: got {}",
        d.damping
    );
    assert!(
        (d.dt - 0.10).abs() < 1e-6,
        "dt default drifted: got {}",
        d.dt
    );
    assert_eq!(d.steps_per_call, 8, "steps_per_call default drifted");
    assert_eq!(
        d.repulsion_mode,
        RepulsionMode::NegativeSampling,
        "repulsion_mode default drifted"
    );
}

// ---------------------------------------------------------------------------
// 6. reset_row_button_clickable
// ---------------------------------------------------------------------------

#[test]
fn reset_row_button_clickable() {
    let clicked = Cell::new(false);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 200.0))
        .build(|ctx| {
            egui::SidePanel::left("reset-row-test")
                .exact_width(280.0)
                .show(ctx, |ui| {
                    if reset_row(ui) {
                        clicked.set(true);
                    }
                });
        });
    harness.run();

    // Locate the button by its accessible label and synthesize a click.
    harness.get_by_label("↺ Reset").click();
    harness.run();

    assert!(
        clicked.get(),
        "reset_row signature drift: the '↺ Reset' button no longer \
         reports clicks via the helper return value",
    );
}

// ---------------------------------------------------------------------------
// 7. focus_set_same_community
// ---------------------------------------------------------------------------

#[test]
fn focus_set_same_community() {
    // Fixture: 6 nodes in 2 communities — {0,1,2,5} = comm 0, {3,4} = comm 1.
    // compute(idx=0, mode=SameCommunityId) must return exactly that set.
    use std::collections::HashMap;
    let mut metrics: HashMap<String, Vec<f32>> = HashMap::new();
    metrics.insert("community".into(), vec![0.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
    let edges: Vec<u32> = vec![];
    let ctx = FocusCtx {
        n_nodes: 6,
        metrics: &metrics,
        edges: &edges,
        node_meta: None,
        query: None,
        field_index: None,
    };
    let set = focus_set::compute(0, FocusMode::SameCommunityId, &ctx);
    let mut got: Vec<u32> = set.into_iter().collect();
    got.sort();
    assert_eq!(
        got,
        vec![0, 1, 2, 5],
        "focus_set::compute(SameCommunityId) returned the wrong set",
    );
}

// ---------------------------------------------------------------------------
// 8. field_index_matches_within_field_or
// ---------------------------------------------------------------------------

#[test]
fn field_index_matches_within_field_or() {
    use graph_renderer::ui::field_index::FieldIndex;
    use graph_renderer::ui::query::ActiveFieldFilters;
    use std::collections::HashMap;

    let mut by_field: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
    let mut tags = HashMap::new();
    tags.insert("rust".to_string(), vec![0, 1, 2]);
    tags.insert("egui".to_string(), vec![2, 3, 4]);
    by_field.insert("tags".to_string(), tags);
    let fi = FieldIndex { by_field };

    let mut filters = ActiveFieldFilters::default();
    let entry = filters.by_field.entry("tags".into()).or_default();
    entry.insert("rust".into());
    entry.insert("egui".into());
    filters.insertion_order.push("tags".into());

    let got = fi.matches(&filters).expect("Some(set)");
    let mut v: Vec<u32> = got.into_iter().collect();
    v.sort();
    assert_eq!(v, vec![0, 1, 2, 3, 4], "within-field OR should union both buckets");
}

// ---------------------------------------------------------------------------
// 9. field_index_matches_across_field_and
// ---------------------------------------------------------------------------

#[test]
fn field_index_matches_across_field_and() {
    use graph_renderer::ui::field_index::FieldIndex;
    use graph_renderer::ui::query::ActiveFieldFilters;
    use std::collections::HashMap;

    let mut by_field: HashMap<String, HashMap<String, Vec<u32>>> = HashMap::new();
    let mut tags = HashMap::new();
    tags.insert("rust".to_string(), vec![0, 1, 2, 3]);
    by_field.insert("tags".to_string(), tags);
    let mut folder = HashMap::new();
    folder.insert("notes".to_string(), vec![2, 3, 4, 5]);
    by_field.insert("folder".to_string(), folder);
    let fi = FieldIndex { by_field };

    let mut filters = ActiveFieldFilters::default();
    filters.by_field.entry("tags".into()).or_default().insert("rust".into());
    filters.by_field.entry("folder".into()).or_default().insert("notes".into());
    filters.insertion_order.push("tags".into());
    filters.insertion_order.push("folder".into());

    let got = fi.matches(&filters).expect("Some(set)");
    let mut v: Vec<u32> = got.into_iter().collect();
    v.sort();
    assert_eq!(v, vec![2, 3], "across-field AND should intersect per-field unions");
}

// ---------------------------------------------------------------------------
// 10. badge_toggle_updates_query_active_filters
// ---------------------------------------------------------------------------

#[test]
fn badge_toggle_updates_query_active_filters() {
    use graph_renderer::ui::badge::{Badge, BadgeAction, BadgeKind};
    use graph_renderer::ui::query::QueryModel;
    use std::cell::RefCell;

    let model: RefCell<QueryModel> = RefCell::new(QueryModel::default());

    // Build a harness that renders one Badge in a SidePanel, then
    // synthesise a click via egui_kittest's accessibility tree.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 100.0))
        .build(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let b = Badge::new("tags", "rust", BadgeKind::Tag);
                let action = b.show(ui);
                if let BadgeAction::Toggle { field, value } = action {
                    model.borrow_mut().toggle_field_filter(&field, &value);
                }
            });
        });

    // Two settled frames so the badge has a stable rect, then click via
    // the accessible label exposed by Badge::show.
    harness.run();
    harness.run();
    harness.get_by_label("badge:tags=rust").click();
    harness.run();
    harness.run();

    let m = model.borrow();
    assert!(
        m.is_filter_active("tags", "rust"),
        "Badge click did not toggle (tags, rust) into QueryModel.active_filters; \
         current filters: {:?}",
        m.active_filters,
    );
}
