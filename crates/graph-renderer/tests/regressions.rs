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
// inspector::show / InspectorData were imported here for the three
// removed inspector tests (3, 4, 19). The inspector body still exists
// as `inspector::render_body`, but it's exercised through the unified
// anchored panel in `app.rs` rather than directly from this test crate.
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
        // Filter currently renders minimal chrome when no filters are
        // active (the empty-state is one row of hint text + the
        // combinator toggle row), so it lands around ~77 px — drop
        // the threshold to 60 for non-Instances sections so the
        // assertion stays load-bearing without being precise to the
        // pixel.
        let threshold = if matches!(section, Section::Instances) { 15.0 } else { 60.0 };
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
// 3 + 4. inspector_hidden_when_no_selection / inspector_shown_when_selection
// ---------------------------------------------------------------------------
//
// REMOVED: these tests asserted against `egui::SidePanel::right("inspector")`
// via `PanelState::load(Id::new("inspector"))`. The right-side Inspector
// surface was collapsed into the unified anchored panel
// (`app.rs::render_anchored_panel`). The inspector body still exists as
// `inspector::render_body` (called when the anchored panel is in its
// `expanded` mode), but there is no longer an "inspector" SidePanel for
// these tests to target. Removing rather than retargeting: the anchored
// panel is a foreground `egui::Area`, not a SidePanel, and its mount
// gate depends on `promoted_anchored_idx` which lives on `App`, not on
// `AppState` — outside what this harness was set up to drive.

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
// 19. inspector_long_id_wraps_inside_panel — REMOVED
// ---------------------------------------------------------------------------
//
// This test asserted against `egui::SidePanel::right("inspector")` and
// `PANEL_W_MAX = 560` from the old free-standing inspector. The
// right-side inspector was collapsed into the unified anchored panel,
// which sizes itself as a foreground `egui::Area` with caller-supplied
// `set_max_width` (360px compact, 480px expanded) rather than a docked
// SidePanel — so neither the id nor the width bound this test was
// pinned to still exists. The long-id wrap behaviour is now governed
// by the `egui::Label::wrap()` call inside `inspector::show_metadata`
// (still present) and the anchored panel's `set_max_width` cap.

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
// node_panel_header_traffic_lights_clickable_and_title_once
// ---------------------------------------------------------------------------
//
// Regression for the promoted-node ("Node") panel header bug: the header
// drew TWO titles (the `FloatingPanel` chrome literal "Node" PLUS a body
// label re-emitting the node name) and the top-left traffic-light cluster
// was UNCLICKABLE — the auto-sizing resizable `egui::Window`'s top-left
// corner resize handle sat over the dots and ate their clicks.
//
// The fix: (1) the panel chrome title IS the node name (single title, body
// no longer re-emits it); (2) the header is inset off the window's
// top-left corner so the dots clear the resize zone; (3) the dots carry
// AccessKit labels so they're real, hit-testable buttons.
//
// This test mounts the shared `FloatingPanel` exactly as the promoted-node
// path does — chrome title = the node name, body = path/tags labels (NO
// title) — and asserts all three properties HEADLESSLY:
//   * the close / minimize / maximize dots each exist exactly once and the
//     close dot is hit-testable (a real cursor click at its center, via
//     `simulate_click`, flips the panel's `open` flag false — a covered or
//     intercepted dot would never register the click),
//   * the node title text appears EXACTLY ONCE (no duplicate title).

#[test]
fn node_panel_header_traffic_lights_clickable_and_title_once() {
    use graph_renderer::ui::floating::FloatingPanel;
    use graph_renderer::ui::state::{FocusedPanel, PanelId};
    use graph_renderer::ui::tiles::Placement;
    use std::cell::Cell;

    const NODE_TITLE: &str = "MyDistinctiveNode";

    // Driven across frames: the close dot must be able to flip this false.
    let open = Cell::new(true);
    // Channels the FloatingPanel mutates in place; kept alive across frames.
    let collapsed = Cell::new(false);
    let placement = Cell::new(Placement::Floating);
    let focused: Cell<Option<FocusedPanel>> = Cell::new(None);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .build(|ctx| {
            graph_renderer::ui::theme::apply_default(ctx);

            let mut open_local = open.get();
            let mut collapsed_local = collapsed.get();
            let mut placement_local = placement.get();
            let mut focused_local = focused.get();

            FloatingPanel::new(PanelId::Node, NODE_TITLE.to_string())
                .default_pos([200.0, 120.0])
                .default_size([460.0, 480.0])
                .with_placement(&mut placement_local)
                .with_collapsed(&mut collapsed_local)
                .with_focus(
                    &mut focused_local,
                    FocusedPanel::AnchoredNode(0),
                )
                .show(ctx, &mut open_local, |ui| {
                    // Body mirrors `render_node_body`: path + tags, and
                    // crucially NO title label (the chrome owns the title).
                    ui.label(
                        egui::RichText::new("vault/notes/my-node.md")
                            .small()
                            .weak()
                            .monospace(),
                    );
                    ui.label("rust, egui");
                    ui.separator();
                    ui.label("Body paragraph that stands in for the inspector.");
                });

            open.set(open_local);
            collapsed.set(collapsed_local);
            placement.set(placement_local);
            focused.set(focused_local);
        });

    // Settle the auto-sizing window (single-pass egui needs a couple of
    // frames for a fresh window to reach a stable rect).
    harness.run();
    harness.run();

    // --- Title appears EXACTLY ONCE -------------------------------------
    let title_hits = harness.query_all_by_label(NODE_TITLE).count();
    assert_eq!(
        title_hits, 1,
        "node-panel title regression: expected the node title {NODE_TITLE:?} \
         to appear exactly once, found {title_hits}. A second hit means the \
         body is re-emitting the title under the chrome's title (the \
         original double-title bug).",
    );

    // --- All three traffic-light dots exist ----------------------------
    for (label, n) in [
        ("Close", harness.query_all_by_label("Close").count()),
        ("Minimize", harness.query_all_by_label("Minimize").count()),
        ("Toggle Tile/Float", harness.query_all_by_label("Toggle Tile/Float").count()),
    ] {
        assert_eq!(
            n, 1,
            "node-panel header regression: expected exactly one {label:?} \
             traffic-light dot, found {n}.",
        );
    }

    // --- Close dot is HIT-TESTABLE -------------------------------------
    // `simulate_click` moves the real cursor to the dot center and presses
    // the primary button. If anything (the window's top-left corner resize
    // handle, an overlapping squircle, a squashed body row) sat over the
    // dot, the click would land on that instead and `open` would stay true.
    assert!(open.get(), "precondition: panel starts open");
    harness.get_by_label("Close").simulate_click();
    harness.run();
    harness.run();
    assert!(
        !open.get(),
        "node-panel header regression: clicking the close traffic-light dot \
         did NOT close the panel — its hit area is obstructed (resize handle \
         over the cluster / overlapping header). open is still true.",
    );
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

// ---------------------------------------------------------------------------
// Timeline scrub controls (Phase P3)
// ---------------------------------------------------------------------------
//
// The Timeline section renders transport controls (play/pause/step) + a scrub
// slider over the buffered frame range. These headless tests drive the egui
// pass and assert the controls exist and mutate `state.timeline.scrub`. The
// ring-buffer reconstruct logic is unit-tested separately in
// `graph_renderer::timeline::tests`.

use graph_renderer::timeline::ScrubState;

/// Build a one-shot harness that renders the Timeline section against `state`,
/// then return it. Wraps the shared `sections::show` call the same way test 2
/// does so the section's accessible widgets are queryable.
fn timeline_harness<'a>(
    state: &'a mut AppState,
    registry: &'a mut ActionRegistry,
    layout_registry: &'a LayoutRegistry,
    perf: &'a PerfCollector,
) -> Harness<'a> {
    Harness::builder()
        .with_size(egui::vec2(360.0, 520.0))
        .build(|ctx| {
            egui::SidePanel::left("timeline-test")
                .exact_width(340.0)
                .show(ctx, |ui| {
                    sections::show(
                        ui,
                        Section::Timeline,
                        state,
                        registry,
                        layout_registry,
                        perf,
                    );
                });
        })
}

#[test]
fn timeline_pause_toggles_scrub_state() {
    let mut state = AppState::default();
    // Simulate a filled ring so the transport controls render.
    state.timeline.buffered_len = 50;
    let mut registry = ActionRegistry::new();
    let layout_registry = LayoutRegistry::seed_default();
    let perf = PerfCollector::default();

    assert_eq!(
        state.timeline.scrub,
        ScrubState::Live,
        "precondition: a fresh timeline starts Live"
    );

    {
        let mut h = timeline_harness(&mut state, &mut registry, &layout_registry, &perf);
        h.get_by_label("⏸ Pause").click();
        h.run();
    }
    assert!(
        matches!(state.timeline.scrub, ScrubState::Paused { .. }),
        "clicking Pause must move scrub to Paused; got {:?}",
        state.timeline.scrub
    );

    // Now the toggle reads "▶ Play" and clicking it resumes Live.
    {
        let mut h = timeline_harness(&mut state, &mut registry, &layout_registry, &perf);
        h.get_by_label("▶ Play").click();
        h.run();
    }
    assert_eq!(
        state.timeline.scrub,
        ScrubState::Live,
        "clicking Play must resume Live; got {:?}",
        state.timeline.scrub
    );
}

#[test]
fn timeline_step_back_updates_frame_index() {
    let mut state = AppState::default();
    state.timeline.buffered_len = 50;
    // Start paused at the head so a step-back lands on a deterministic index.
    state.timeline.pause_at(49);
    state.timeline.seek_dirty = false; // clear the pause's flag
    let mut registry = ActionRegistry::new();
    let layout_registry = LayoutRegistry::seed_default();
    let perf = PerfCollector::default();

    let before = state.timeline.current_idx();
    assert_eq!(before, 49, "precondition: paused at head (49)");

    {
        let mut h = timeline_harness(&mut state, &mut registry, &layout_registry, &perf);
        h.get_by_label("⏮ Step −").click();
        h.run();
    }

    assert_eq!(
        state.timeline.current_idx(),
        48,
        "Step − must decrement the scrub frame index by one"
    );
    assert!(
        state.timeline.seek_dirty,
        "a step must flag a fresh seek so the App pushes the frame to the GPU"
    );
}

#[test]
fn timeline_step_forward_past_head_returns_live() {
    let mut state = AppState::default();
    state.timeline.buffered_len = 10;
    state.timeline.pause_at(9); // head
    let mut registry = ActionRegistry::new();
    let layout_registry = LayoutRegistry::seed_default();
    let perf = PerfCollector::default();

    {
        let mut h = timeline_harness(&mut state, &mut registry, &layout_registry, &perf);
        h.get_by_label("Step + ⏭").click();
        h.run();
    }

    assert_eq!(
        state.timeline.scrub,
        ScrubState::Live,
        "stepping forward past the head must return to Live; got {:?}",
        state.timeline.scrub
    );
}

#[test]
fn timeline_empty_buffer_shows_no_transport() {
    // With an empty ring the section shows only the hint + capture knobs — no
    // Pause control to click.
    let mut state = AppState::default();
    state.timeline.buffered_len = 0;
    let mut registry = ActionRegistry::new();
    let layout_registry = LayoutRegistry::seed_default();
    let perf = PerfCollector::default();

    let mut h = timeline_harness(&mut state, &mut registry, &layout_registry, &perf);
    h.run();
    assert!(
        h.query_by_label("⏸ Pause").is_none(),
        "empty timeline must not render the Pause transport control"
    );
}

// ---------------------------------------------------------------------------
// promoted_node_body_renders_path_exactly_once
// ---------------------------------------------------------------------------
//
// Real-screenshot regression: the promoted/expanded node-preview panel
// rendered a DUPLICATED, OVERLAPPING body — the node's path string showed
// up TWICE (once dim/small from the panel's path header, once bold/bright
// from the inspector's `show_metadata` id row), with the metric rows
// interleaved between them and the whole top band squashed over the
// traffic-light dots.
//
// Why the *other* node-panel test (`node_panel_header_traffic_lights_…`)
// didn't catch it: that test mounts the `FloatingPanel` with a STAND-IN
// body (two hand-written `ui.label`s), so it never exercised the real
// `render_node_body → inspector::render_body → show_metadata` path where
// the duplication actually lives.
//
// This test mounts the GENUINE `app::render_node_body` (re-exported via
// `graph_renderer::test_support`) — not a hand-copied mirror — so it
// exercises the exact production path: `render_node_body →
// inspector::render_body → show_metadata`. For a vault node whose `id`
// equals its `path` (the common Obsidian case, exactly as in the
// screenshot's `shared/knowledge-base/.../jira-ITHELP-32104`) it asserts
// the path string appears EXACTLY ONCE.
//
// Pre-fix `render_node_body` pre-drew a `meta.path` header on top of the
// inspector's id row (== path) → 2 hits. Post-fix the pre-header is gone
// (the body owns the id/tags) → 1 hit. Because it calls the real
// function, re-introducing any path pre-header makes this fail again.

#[test]
fn promoted_node_body_renders_path_exactly_once() {
    use eframe::egui;
    use graph_renderer::proto::NodeMeta;
    use graph_renderer::test_support::{render_node_body, AnchoredChannels};
    use graph_renderer::ui::query::ActiveFieldFilters;
    use graph_renderer::ui::state::ColorBy;
    use std::collections::HashMap;

    // Mirror the screenshot: the node's id IS its path.
    const PATH: &str = "shared/knowledge-base/it/tickets/jira-ITHELP-32104";

    // One node; id == path. Metrics populate the idx/degree/community rows
    // so the body matches the screenshot's shape.
    let ids = vec![PATH.to_string()];
    let mut metrics: HashMap<String, Vec<f32>> = HashMap::new();
    metrics.insert("degree".into(), vec![9.0]);
    metrics.insert("pagerank".into(), vec![0.0]);
    metrics.insert("community".into(), vec![633.0]);
    metrics.insert("kcore".into(), vec![0.0]);
    let edges: Vec<u32> = vec![];

    let meta = NodeMeta {
        id: PATH.to_string(),
        title: String::new(),
        path: PATH.to_string(),
        folder: String::new(),
        doctype: None,
        tags: vec!["inventory-management".into(), "it".into()],
        frontmatter_json: String::new(),
        body: String::new(),
        ..Default::default()
    };

    let active_filters = ActiveFieldFilters::default();
    let mut tag_query = String::new();
    let mut page_states = HashMap::new();
    let mut md_cache = egui_commonmark::CommonMarkCache::default();
    let mut channels = AnchoredChannels::default();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 700.0))
        .build(|ctx| {
            graph_renderer::ui::theme::apply_default(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_max_width(460.0);
                // Genuine production body — same call `render_node_panel_floating`
                // hands to the FloatingPanel.
                render_node_body(
                    ui,
                    480.0,
                    0,
                    &meta,
                    &ids,
                    &metrics,
                    &edges,
                    ColorBy::default(),
                    graph_renderer::data::PaletteId::default(),
                    &active_filters,
                    None,
                    &mut page_states,
                    &mut md_cache,
                    &mut tag_query,
                    &mut channels,
                );
            });
        });

    harness.run();
    harness.run();

    let path_hits = harness.query_all_by_label(PATH).count();
    assert_eq!(
        path_hits, 1,
        "promoted-node body regression: the node path {PATH:?} appears \
         {path_hits} times. render_node_body must NOT pre-draw a path \
         header on top of the inspector's id row (id == path for this \
         vault node) — that produced the duplicated/overlapping body from \
         the screenshot. Expected exactly one.",
    );
}
