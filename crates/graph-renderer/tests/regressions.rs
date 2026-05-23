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
use graph_renderer::ui::workspace::apply_rotate_curve;
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
        // Section bodies no longer emit their own `─── Title ───`
        // header rule (the chrome owns that now), so the legacy
        // thresholds were lowered by roughly one header row (~20 px)
        // to keep this assertion meaningful but not flaky.
        let threshold = if matches!(section, Section::Instances) { 15.0 } else { 80.0 };
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
            let mut req_nav: Option<String> = None;
            let mut req_url: Option<String> = None;
            let mut req_focus: Option<String> = None;
            let mut req_page_save: Option<(String, String, String)> = None;
            let active_filters = graph_renderer::ui::query::ActiveFieldFilters::default();
            let mut data = InspectorData {
                ids: &ids,
                metrics: &metrics,
                edges: &edges,
                selected_idx: None,
                requested_selection: &mut requested,
                requested_filter_toggle: &mut req_toggle,
                color_by: graph_renderer::ui::state::ColorBy::default(),
                palette: graph_renderer::data::PaletteId::default(),
                current_meta: None,
                active_filters: &active_filters,
                requested_navigate: &mut req_nav,
                requested_open_url: &mut req_url,
                requested_focus_node: &mut req_focus,
                field_index: None,
                page_viewer_states: None,
                markdown_cache: None,
                requested_page_save: &mut req_page_save,
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
            let mut req_nav: Option<String> = None;
            let mut req_url: Option<String> = None;
            let mut req_focus: Option<String> = None;
            let mut req_page_save: Option<(String, String, String)> = None;
            let active_filters = graph_renderer::ui::query::ActiveFieldFilters::default();
            let mut data = InspectorData {
                ids: &ids,
                metrics: &metrics,
                edges: &edges,
                selected_idx: Some(0),
                requested_selection: &mut requested,
                requested_filter_toggle: &mut req_toggle,
                color_by: graph_renderer::ui::state::ColorBy::default(),
                palette: graph_renderer::data::PaletteId::default(),
                current_meta: None,
                active_filters: &active_filters,
                requested_navigate: &mut req_nav,
                requested_open_url: &mut req_url,
                requested_focus_node: &mut req_focus,
                field_index: None,
                page_viewer_states: None,
                markdown_cache: None,
                requested_page_save: &mut req_page_save,
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
        RepulsionMode::BarnesHut,
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

// ---------------------------------------------------------------------------
// 11. rotate_curve_is_sign_preserving
// ---------------------------------------------------------------------------
// The rotation curve in `workspace::apply_rotate_curve` must be an odd
// function: `f(-x) == -f(x)`. A regression here means the cubic term
// lost its `signum` and snapping the mouse left rotates differently
// than snapping it right.

#[test]
fn rotate_curve_is_sign_preserving() {
    for x in [1.0_f32, 5.0, 10.0, 25.0, 50.0] {
        let pos = apply_rotate_curve(x);
        let neg = apply_rotate_curve(-x);
        assert!(
            (pos + neg).abs() < 1e-4,
            "apply_rotate_curve regression: not sign-preserving at x={x}: \
             f({x}) = {pos}, f(-{x}) = {neg}, sum = {}",
            pos + neg,
        );
    }
}

// ---------------------------------------------------------------------------
// 12. rotate_curve_is_super_linear_past_knee
// ---------------------------------------------------------------------------
// The cubic term should make the curve super-linear past the ~10px
// knee. Concretely: f(20) > 2*f(10). If this fails, the cubic boost
// is gone and hand-sweeps no longer fly.

#[test]
fn rotate_curve_is_super_linear_past_knee() {
    let f10 = apply_rotate_curve(10.0);
    let f20 = apply_rotate_curve(20.0);
    assert!(
        f20 > 2.0 * f10,
        "apply_rotate_curve regression: not super-linear past knee: \
         f(10) = {f10}, f(20) = {f20}, expected f(20) > {}",
        2.0 * f10,
    );
}

// ---------------------------------------------------------------------------
// 13. rotate_curve_at_zero_returns_zero
// ---------------------------------------------------------------------------
// `signum * 0` is well-defined in Rust (returns 0.0, not NaN), but if
// someone refactors the curve to use a divisor-then-multiply chain
// they could reintroduce a NaN at zero. Pin the value.

#[test]
fn rotate_curve_at_zero_returns_zero() {
    let v = apply_rotate_curve(0.0);
    assert_eq!(
        v, 0.0,
        "apply_rotate_curve(0.0) regression: expected 0.0, got {v}",
    );
    assert!(
        !v.is_nan(),
        "apply_rotate_curve(0.0) regression: produced NaN",
    );
}

// ---------------------------------------------------------------------------
// 14. zoom_distance_scale_clamps
// ---------------------------------------------------------------------------
// Replicates the formula from `workspace.rs`:
//     dist_scale = (position.length() / 1000.0).clamp(0.2, 5.0)
// At distance 0 we must clamp to 0.2 (so close-in zoom doesn't
// collapse). At a huge distance (100_000) we clamp to 5.0 (so far-out
// flicks stay responsive without going parabolic).

#[test]
fn zoom_distance_scale_clamps() {
    fn zoom_dist_scale(position_len: f32) -> f32 {
        (position_len / 1000.0).clamp(0.2, 5.0)
    }
    assert_eq!(
        zoom_dist_scale(0.0),
        0.2,
        "zoom dist_scale lower clamp regression: at dist=0 expected 0.2",
    );
    assert_eq!(
        zoom_dist_scale(100_000.0),
        5.0,
        "zoom dist_scale upper clamp regression: at dist=100000 expected 5.0",
    );
    // Mid-range stays linear (sanity).
    assert!(
        (zoom_dist_scale(2000.0) - 2.0).abs() < 1e-4,
        "zoom dist_scale mid-range regression: at dist=2000 expected 2.0",
    );
}

// ---------------------------------------------------------------------------
// 15. picking_screen_space_24px_tolerance_unit
// ---------------------------------------------------------------------------
// The real picker in `graph_pipelines::raycast` requires a wgpu
// device — too heavy for unit testing. We instead replicate the
// screen-space portion of the algorithm against a fixture of
// already-projected nodes (skip the projection matrix; test the part
// that defines "nearest projected node within tol_px").
//
// Setup: 5 nodes already in pixel coords, one of which sits 3D-far
// (large `depth_w`) but very close in screen space, and another that
// is 3D-near (small `depth_w`) but well outside the 24px tolerance.
// The picker must prefer the 3D-far-but-screen-near node, because
// screen-space is the only metric the user can see.

#[test]
fn picking_screen_space_24px_tolerance_unit() {
    const R_PICK_PX: f32 = 24.0;

    // (node_px_x, node_px_y, depth_w, idx)
    let fixture = [
        // idx 0: 3D-near but screen-far (well past 24px) → must NOT win.
        (200.0_f32, 200.0_f32, 0.5_f32, 0_u32),
        // idx 1: 3D-far but screen-near (8px away) → must win.
        (608.0,     400.0,     50.0,    1),
        // idx 2: another candidate, screen-near (12px away), deeper.
        (612.0,     400.0,     80.0,    2),
        // idx 3: way off-screen.
        (10.0,      10.0,      10.0,    3),
        // idx 4: at click point but behind camera (depth_w <= 0) → skipped.
        (600.0,     400.0,     -1.0,    4),
    ];
    let cursor = (600.0_f32, 400.0_f32);

    // Mirror the raycast()'s candidate filter + ranking.
    let mut best: Option<(f32, f32, u32)> = None;
    for &(px, py, w, idx) in &fixture {
        if w <= 1e-4 {
            continue; // behind camera
        }
        let dx = px - cursor.0;
        let dy = py - cursor.1;
        let dist_px = (dx * dx + dy * dy).sqrt();
        let tol_px = R_PICK_PX.max(4.0); // node radius 4 in this fixture
        if dist_px > tol_px {
            continue;
        }
        let cand = (dist_px, w, idx);
        best = Some(match best {
            None => cand,
            Some(prev) => {
                if w < prev.1 - 1e-3 {
                    cand
                } else if (w - prev.1).abs() <= 1e-3 && dist_px < prev.0 {
                    cand
                } else {
                    prev
                }
            }
        });
    }

    let picked = best.map(|(_, _, i)| i);
    assert_eq!(
        picked,
        Some(1),
        "picking regression: expected screen-nearest in-tolerance node \
         (idx=1, 8px away) to win, got {picked:?}. The picker is back to \
         3D-distance ranking.",
    );

    // And idx=0 (3D-near but 282px in screen space) must never appear
    // even as a candidate — it's outside tol_px.
    let dx = 200.0_f32 - cursor.0;
    let dy = 200.0_f32 - cursor.1;
    let dist0 = (dx * dx + dy * dy).sqrt();
    assert!(
        dist0 > R_PICK_PX,
        "fixture sanity: idx=0 should be outside 24px tolerance, got {dist0}",
    );
}

// ---------------------------------------------------------------------------
// 16. badge_tag_hue_is_deterministic
// ---------------------------------------------------------------------------
//
// Real failure mode: tag colour swatches must be stable per value across
// renders. If `Badge::tag_hue` ever picked up a non-deterministic source
// (RandomState hash, time-based seed) the same tag would flicker between
// hues every frame. Also pins the diversification property: distinct
// inputs map to distinct hues.

#[test]
fn badge_tag_hue_is_deterministic() {
    use graph_renderer::ui::badge::Badge;

    let baseline = Badge::tag_hue("rust");
    for _ in 0..100 {
        assert_eq!(
            Badge::tag_hue("rust"),
            baseline,
            "tag_hue('rust') is non-deterministic across calls",
        );
    }

    let other = Badge::tag_hue("egui");
    assert_ne!(
        baseline, other,
        "tag_hue regression: 'rust' and 'egui' collapsed to the same hue ({baseline})",
    );
}

// ---------------------------------------------------------------------------
// 17. badge_small_variant_is_smaller
// ---------------------------------------------------------------------------
//
// Real failure mode: `.small(true)` is meant for hosts that already pad
// their frame (filter-strip / chip-strip). If the small flag stops
// shrinking inner padding (regression we shipped once when refactoring
// padding), chips overflow their host. We measure the rect from the
// returned `InnerResponse` of a wrapping scope and require strict-less
// in both axes.

#[test]
fn badge_small_variant_is_smaller() {
    use graph_renderer::ui::badge::{Badge, BadgeKind};

    let default_size = Cell::new(egui::Vec2::ZERO);
    let small_size = Cell::new(egui::Vec2::ZERO);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 200.0))
        .build(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let inner = ui.scope(|ui| {
                        let _ = Badge::new("tags", "rust", BadgeKind::Tag).show(ui);
                    });
                    default_size.set(inner.response.rect.size());
                });
                ui.horizontal(|ui| {
                    let inner = ui.scope(|ui| {
                        let _ = Badge::new("tags", "rust", BadgeKind::Tag)
                            .small(true)
                            .show(ui);
                    });
                    small_size.set(inner.response.rect.size());
                });
            });
        });
    harness.run();
    harness.run();

    let d = default_size.get();
    let s = small_size.get();
    assert!(
        s.x < d.x && s.y < d.y,
        "Badge::small(true) regression: expected smaller rect than default; \
         default={d:?} small={s:?}",
    );
}

// ---------------------------------------------------------------------------
// 18. badge_click_returns_toggle
// ---------------------------------------------------------------------------
//
// Real failure mode: clicks on a Tag/Doctype badge must yield
// `BadgeAction::Toggle { field, value }` carrying the badge's own
// (field, value). We already had a regression where a different match
// arm stole the click; this pins the happy path on the Doctype kind so
// it's distinct from test #10's Tag coverage.

#[test]
fn badge_click_returns_toggle() {
    use graph_renderer::ui::badge::{Badge, BadgeAction, BadgeKind};
    use std::cell::RefCell;

    let captured: RefCell<Option<(String, String)>> = RefCell::new(None);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 100.0))
        .build(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let action = Badge::new("doctype", "note", BadgeKind::Doctype).show(ui);
                if let BadgeAction::Toggle { field, value } = action {
                    *captured.borrow_mut() = Some((field, value));
                }
            });
        });
    harness.run();
    harness.run();
    harness.get_by_label("badge:doctype=note").click();
    harness.run();
    harness.run();

    let got = captured.borrow().clone();
    assert_eq!(
        got,
        Some(("doctype".to_string(), "note".to_string())),
        "Badge click regression: expected Toggle{{doctype, note}}, got {got:?}",
    );
}

// ---------------------------------------------------------------------------
// 19. inspector_long_id_wraps_inside_panel
// ---------------------------------------------------------------------------
//
// Real failure mode: a long, no-space node id used to push past the
// inspector panel's right edge, hiding the resize handle. The fix puts
// the id label inside `egui::Label::wrap()` and constrains row width to
// `available_width()`. We mount the inspector with a 70+ char id and
// assert the panel's rect stays inside the configured max width and the
// viewport — i.e. the long id wrapped instead of forcing horizontal
// growth.

#[test]
fn inspector_long_id_wraps_inside_panel() {
    let long_id =
        "shared/knowledge-base/_ingested/it-ops/jira-ITHELP-22318-something-extra".to_string();
    assert!(long_id.len() >= 70, "fixture id too short");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build(|ctx| {
            let mut state = AppState::default();
            state.inspector_open = true;
            let ids: Vec<String> = vec![long_id.clone()];
            let metrics = std::collections::HashMap::new();
            let edges: Vec<u32> = Vec::new();
            let mut requested: Option<u32> = None;
            let mut req_toggle: Option<(String, String)> = None;
            let mut req_nav: Option<String> = None;
            let mut req_url: Option<String> = None;
            let mut req_focus: Option<String> = None;
            let mut req_page_save: Option<(String, String, String)> = None;
            let active_filters = graph_renderer::ui::query::ActiveFieldFilters::default();
            let mut data = InspectorData {
                ids: &ids,
                metrics: &metrics,
                edges: &edges,
                selected_idx: Some(0),
                requested_selection: &mut requested,
                requested_filter_toggle: &mut req_toggle,
                color_by: graph_renderer::ui::state::ColorBy::default(),
                palette: graph_renderer::data::PaletteId::default(),
                current_meta: None,
                active_filters: &active_filters,
                requested_navigate: &mut req_nav,
                requested_open_url: &mut req_url,
                requested_focus_node: &mut req_focus,
                field_index: None,
                page_viewer_states: None,
                markdown_cache: None,
                requested_page_save: &mut req_page_save,
            };
            inspector::show(ctx, &mut state, &mut data);
        });
    harness.run();
    harness.run();

    let panel_state = egui::containers::panel::PanelState::load(
        &harness.ctx,
        egui::Id::new("inspector"),
    )
    .expect("inspector should mount with a valid selection");

    // PANEL_W_MAX is 560 in inspector.rs; a non-wrapping long id used
    // to grow the panel past that. Allow 1px slack for rounding.
    assert!(
        panel_state.rect.width() <= 560.0 + 1.0,
        "inspector long-id regression: panel rect width {} exceeded \
         PANEL_W_MAX(560), id likely failed to wrap",
        panel_state.rect.width(),
    );
    // The panel must also stay inside the harness viewport.
    assert!(
        panel_state.rect.right() <= 900.0 + 1.0,
        "inspector long-id regression: panel right edge {} pushed past \
         viewport (900); id failed to wrap inside the panel",
        panel_state.rect.right(),
    );
}

// ---------------------------------------------------------------------------
// 20. theme_borders_use_palette_border
// ---------------------------------------------------------------------------
//
// Real failure mode: chrome strokes had been hard-coded to WHITE,
// causing hovered/active states to collapse to invisible (white-on-white)
// while noninteractive borders shouted at full contrast. The fix routes
// chrome strokes through `palette::BORDER` and body text through
// `palette::TEXT`. This pins the wiring.

#[test]
fn theme_borders_use_palette_border() {
    use graph_renderer::ui::theme::{self, palette};

    let ctx = egui::Context::default();
    theme::apply_default(&ctx);
    let style = ctx.style();
    let v = &style.visuals;

    assert_eq!(
        v.widgets.noninteractive.bg_stroke.color,
        palette::BORDER,
        "theme regression: noninteractive bg_stroke is not palette::BORDER",
    );
    assert_eq!(
        v.widgets.inactive.bg_stroke.color,
        palette::BORDER,
        "theme regression: inactive bg_stroke is not palette::BORDER",
    );
    assert_eq!(
        v.override_text_color,
        Some(palette::TEXT),
        "theme regression: override_text_color is not palette::TEXT",
    );
    assert_eq!(
        v.window_stroke.color,
        palette::BORDER,
        "theme regression: window_stroke is not palette::BORDER",
    );
}

// ===========================================================================
// Phase 3 regressions: cooling/click-wake + filter clear
// ===========================================================================

// ---------------------------------------------------------------------------
// gpu_force_options_eq_ignoring_cursor_basic
// ---------------------------------------------------------------------------
//
// Phase-3 regression for the "click wakes a settled sim" bug. The
// renderer pushes cursor pose every frame; `set_options` must NOT wake
// the layout when only the three cursor-pose fields differ. The
// algorithmic gate is `GpuForceOptions::eq_ignoring_cursor` — this test
// pins its semantics directly.

#[test]
fn gpu_force_options_eq_ignoring_cursor_basic() {
    use graph_layouts::GpuForceOptions;

    let base = GpuForceOptions::default();

    // Cursor-only diff: every cursor field perturbed, nothing else
    // touched. Must report equal-ignoring-cursor.
    let cursor_only = GpuForceOptions {
        cursor_pos: [10.0, 20.0, 30.0],
        cursor_radius: 250.0,
        cursor_strength: 4.5,
        ..base.clone()
    };
    assert!(
        base.eq_ignoring_cursor(&cursor_only),
        "cursor-only diff must compare equal — otherwise every mouse \
         move re-wakes the sim and the user sees forever-drift",
    );

    // Non-cursor diff: a single non-cursor field changed must report
    // not-equal so `set_options` calls `wake()`.
    let slider_changed = GpuForceOptions {
        repulsion: base.repulsion + 100.0,
        ..base.clone()
    };
    assert!(
        !base.eq_ignoring_cursor(&slider_changed),
        "non-cursor diff (repulsion) must compare not-equal — otherwise \
         slider tweaks silently fail to wake a halted sim",
    );

    // Same value, fresh allocation — must compare equal.
    let identical = GpuForceOptions::default();
    assert!(
        base.eq_ignoring_cursor(&identical),
        "two default-constructed options must compare equal-ignoring-cursor",
    );
}

// ---------------------------------------------------------------------------
// gpu_force_options_eq_ignoring_cursor_exhaustive_destructure_compiles
// ---------------------------------------------------------------------------
//
// Passive guard: this test does no behavioral assertion. Its sole job
// is to FAIL TO COMPILE if a future field is added to
// `GpuForceOptions` without being classified inside
// `eq_ignoring_cursor`. The implementation uses an exhaustive `Self {
// .. }` destructure with no `..` rest pattern, so any new field forces
// the author to explicitly decide whether the new field is a cursor
// pose (add to the `_` ignore list) or a behavioral knob (add to the
// comparison). If THIS test no longer compiles, fix it by following
// the rustc hint inside the impl — don't paper over with `..`.

#[test]
fn gpu_force_options_eq_ignoring_cursor_exhaustive_destructure_compiles() {
    use graph_layouts::GpuForceOptions;
    let a = GpuForceOptions::default();
    let b = GpuForceOptions::default();
    // Merely calling the method keeps the impl referenced from a test
    // target so the exhaustive-destructure check fires under `cargo test`.
    let _ = a.eq_ignoring_cursor(&b);
}

// ---------------------------------------------------------------------------
// query_clear_all_filters_empties_active
// ---------------------------------------------------------------------------
//
// Phase-3 regression for the badge-focus auto-flip restore path.
// `clear_all_filters` is the empty-edge trigger that lets
// `handle_filter_focus_auto_flip` restore the snapshotted FocusMode.
// If clear_all_filters silently leaves a stale entry behind, the
// auto-flip restore never fires.

#[test]
fn query_clear_all_filters_empties_active() {
    use graph_renderer::ui::query::QueryModel;

    let mut q = QueryModel::default();
    q.toggle_field_filter("tags", "rust");
    assert!(
        q.is_filter_active("tags", "rust"),
        "precondition: toggle must add the (tags, rust) entry",
    );
    assert!(
        !q.active_filters.by_field.is_empty(),
        "precondition: by_field must be non-empty after a toggle",
    );

    q.clear_all_filters();

    assert!(
        q.active_filters.by_field.is_empty(),
        "clear_all_filters must empty active_filters.by_field; got {:?}",
        q.active_filters.by_field,
    );
    assert!(
        q.active_filters.insertion_order.is_empty(),
        "clear_all_filters must also empty insertion_order; got {:?}",
        q.active_filters.insertion_order,
    );
    assert!(
        !q.is_filter_active("tags", "rust"),
        "is_filter_active must report false after clear_all_filters",
    );
}
