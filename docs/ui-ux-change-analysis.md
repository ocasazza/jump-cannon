# UI/UX Change Analysis: Water-Energetic Docking Integration

## Current UI Surface

### Inspector Panel (`inspector.rs`, 804 lines)
Current metric rows (lines 690-699), in this order:
1. idx (optional)
2. degree (indegree / outdegree)
3. pagerank (4 decimal places)
4. betweenness (4 decimal places)
5. community
6. kcore
7. wcc

Format: `div.kv > span.k + span.v` — a key-value pair in a css-grid.

### Metrics Panel (`metrics.rs`, 323 lines)
Renders aggregated graph metrics — no per-node data, no charts.

### Style Panel (`style.rs`, 1532 lines)
Coloring modes. `ColorBy` enum drives node/edge color:
- Degree, PageRank, Community, KCore, Betweenness, WCC, Doctype
- (No stability, no edge anomaly, no energy coloring modes)

### Timeline Panel (`timeline.rs`, 768 lines)
Per-frame iteration display. No convergence visualization.

### Settings / Layout Panel (`layout.rs`, 4223 lines)
Controls for layout engine parameters. No regime concept.

---

## What Changes (by phase)

### Phase 1: Inspector & Metrics Surface

#### ADDED (new UI surface)
| Panel | New Element | Location | Data Source |
|-------|-------------|----------|-------------|
| Inspector | Energy row: `div.kv span.k "energy" span.v "{total:.4}"` | After wcc row (L699) | NodeMeta.energy_total |
| Inspector | Energy breakdown (optional): attractive/repulsive sub-values | Expandable under energy row | NodeMeta.energy_attractive / energy_repulsive |
| Inspector | Stability row: `div.kv span.k "stability" span.v "{var:.2}"` | After energy row | NodeMeta.stability_variance |
| Inspector | Edge anomaly alert: inline warning banner | Below metrics-grid, above badge_row | NodeMeta.anomaly_flag |
| Metrics | Convergence status indicator (converged/diverging/oscillating) | New section in metrics panel | Progress stream convergence events |

#### DEPRECATED (nothing removed — pure additions)
No existing UI elements are removed. All current metric rows remain.

#### TESTS to create (TDD)
```
test_inspector_energy_row_present           — energy kv appears after wcc
test_inspector_energy_value_format           — "{:.4}" format, non-negative
test_inspector_stability_row_present         — stability kv appears after energy
test_inspector_stability_value_range         — variance in [0, 1e6]; drift in [0, 1]
test_inspector_anomaly_banner_when_flagged   — warning banner when anomaly_flag=true
test_inspector_no_banner_when_clean          — no banner when anomaly_flag=false
test_metrics_convergence_indicator_states    — renders converged/diverging/oscillating states
```

---

### Phase 2: Visual Overlays

#### ADDED
| Surface | New Element | Location | Data Source |
|---------|-------------|----------|-------------|
| Style panel | `Stability` variant in `ColorBy` enum | After existing variants | node_stability buffer |
| Style panel | `Anomaly` variant in `ColorBy` enum | After Stability | edge_anomaly buffer |
| Canvas | Node stability heatmap (per-vertex color from stability variance) | Shader: `node.wgsl` | New vertex buffer |
| Canvas | Edge anomaly highlighting (per-edge color/thickness from anomaly_score) | Shader: `edge.wgsl` | New edge buffer |

#### DEPRECATED
**Nothing deprecated** — coloring modes are additive. Existing Degree/PageRank/etc. remain.

#### UI/UX behavior change
- When `ColorBy::Stability` is selected, the color legend changes to "Stable → Unstable" gradient
- When `ColorBy::Anomaly` is selected, edge thickness scales with anomaly_score
- Both new modes respect the existing color scale controls

#### TESTS to create (TDD)
```
test_color_by_enum_has_stability_variant     — Stability variant in ColorBy
test_color_by_enum_has_anomaly_variant       — Anomaly variant in ColorBy
test_stability_mode_changes_legend           — legend text switches to "Stable → Unstable"
test_anomaly_mode_changes_legend             — legend text switches to anomaly scale
test_node_stability_buffer_non_empty         — buffer populated after layout step
test_edge_anomaly_buffer_zero_when_none      — zero-filled when no anomalous edges
test_stability_color_mapping_continuous      — color varies continuously with stability value
test_anomaly_edge_thickness_monotonic        — thickness increases with anomaly_score
```

---

### Phase 3: New Panels

#### ADDED
| Surface | Type | File |
|---------|------|------|
| Edge Inspector | New panel | `edge_inspector.rs` ✅ (already created) |
| Timeline convergence | New rendering in existing panel | `timeline.rs` modifications |

#### DEPRECATED
**Nothing deprecated** — the Timeline panel currently has a PARITY GAP (not fully implemented from egui migration). The convergence work fills that gap.

#### TESTS to create (TDD)
```
test_edge_inspector_registered_as_panel       — Panel::EdgeInspector exists in enum
test_edge_inspector_appears_in_workspace      — can be dragged into workspace
test_edge_inspector_empty_state               — "No anomalous edges found" when empty
test_edge_inspector_filters_by_threshold      — slider changes visible rows
test_edge_inspector_sorts_correctly           — each SortBy mode works
test_timeline_convergence_chart_renders       — chart element exists when data present
test_timeline_convergence_axis_labels          — stress on Y, iteration on X
```

---

### Phase 4: Regime Abstraction

#### ADDED
| Panel | New Element | Location |
|-------|-------------|----------|
| Settings | Regime dropdown (select: Layout / Docking) | New control in settings |
| Layout panel | Regime selector (reuse or link to Settings) | Top of layout controls |
| Inspector | Regime-aware labels ("Binding Energy" vs "Layout Energy") | kv rows change labels |
| Metrics | Regime-aware unit display (kJ/mol vs arbitrary units) | value formatting |

#### DEPRECATED (or renamed)
| Old | New (Docking regime) | New (Layout regime — unchanged) |
|-----|----------------------|--------------------------------|
| "energy" label | "Binding Energy" | "energy" (unchanged) |
| "stability" label | "RMSF" | "stability" (unchanged) |
| (no unit) | kJ/mol | (unitless) |

**DEPRECATION STRATEGY**: The label change is regime-controlled. Default regime is Layout (backward compatible). Existing users see no change. Switching to Docking regime re-renders labels. No code is deleted — labels are conditional on `ctx.regime`.

#### TESTS to create (TDD)
```
test_regime_default_is_layout                 — Layout regime on startup
test_regime_switch_changes_inspector_labels   — energy → Binding Energy in Docking regime
test_regime_switch_changes_metric_units       — units change to kJ/mol in Docking regime
test_regime_persists_across_restart           — localStorage retains regime selection
test_regime_dropdown_renders_in_settings      — dropdown exists
test_regime_dropdown_has_both_options         — Layout and Docking options
```

---

## TDD Summary by Test Type

### Rust backend tests (cargo test)
```
graph-layouts/src/convergence.rs         14 tests ✅ (existing)
graph-layouts/src/stability.rs            4 tests ✅ (existing)
graph-layouts/src/energy_decomposition.rs 5 tests ✅ (existing)
graph-metrics/src/edge_anomaly.rs         7 tests ✅ (existing)

# NEW tests needed (backend):
graph-api: NodeMeta serialization round-trip with energy/stability fields
graph-api: /node/:id returns new fields
graph-layouts: regime-aware energy label mapping
graph-metrics: anomaly_flag threshold calibration
```

### Rust browser tests (just test browser-rust)
```
# NEW tests needed:
Inspector renders energy row after layout
Inspector renders stability row after layout
Style panel has Stability and Anomaly color modes
Edge Inspector panel opens and renders sample data
Regime dropdown switches Inspector labels
```

### Frontend Dioxus component tests
```
# NEW tests needed (if Dioxus test infrastructure exists):
Inspector metrics-grid contains 9 rows (7 existing + 2 new)
EdgeInspector panel renders 12 sample anomalies
EdgeInspector threshold slider filters correctly
Layout panel tooltips contain refinement analogies
```

---

## What Is NOT Deprecated

- All 7 existing Inspector metric rows (idx, degree, pagerank, betweenness, community, kcore, wcc)
- All existing Style panel coloring modes (Degree, PageRank, Community, KCore, Betweenness, WCC, Doctype)
- All Layout panel controls (repulsion mode, θ, K, speed, edge strength, mass source, engine cascade)
- The existing badge/chip system in Inspector
- The existing community/neighbors section in Inspector
- The existing node/document/vault panels

## What IS Deprecated (regime-switch only, not removed)

| Component | Deprecation | Condition |
|-----------|-------------|-----------|
| "energy" label | Renamed to "Binding Energy" | Only in Docking regime |
| "stability" label | Renamed to "RMSF" | Only in Docking regime |
| Unitless values | Gaining kJ/mol suffix | Only in Docking regime |
