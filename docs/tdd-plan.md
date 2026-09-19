# TDD Plan: Water-Energetic Docking UI Integration

## Principle

Tests are written BEFORE implementation. Each test should:
1. FAIL on the current codebase (proves the feature is absent)
2. PASS after the implementation (proves the feature works)
3. Continue to PASS after refactoring (regression guard)

## Test Layers

```
┌─────────────────────────────────────────┐
│  L3: Browser smoke tests                │  just test browser-rust
│  (chromiumoxide — real rendering)       │
├─────────────────────────────────────────┤
│  L2: Component unit tests               │  cargo test -p app-ui (wasm)
│  (Dioxus rsx! output assertions)        │
├─────────────────────────────────────────┤
│  L1: Backend integration tests          │  cargo test -p graph-api
│  (HTTP endpoint responses)              │
├─────────────────────────────────────────┤
│  L0: Data model tests (EXISTING ✅)     │  cargo test -p graph-layouts -p graph-metrics
│  (energy, stability, anomaly, converge) │  30 tests passing
└─────────────────────────────────────────┘
```

---

## L1: Backend Integration Tests (graph-api)

### File: `crates/graph-api/tests/inspector_metrics.rs`

These tests verify that the graph-api HTTP server returns the new Phase 0 metrics
in its `/node/:id` JSON response. They should FAIL today because the fields don't exist yet.

```rust
// Test 1: NodeMeta includes energy fields when available
#[tokio::test]
async fn node_meta_includes_energy_fields() {
    // GIVEN a graph with energy decomposition computed
    // WHEN GET /node/{id} is called
    // THEN response includes node_energy, node_energy_attractive, node_energy_repulsive
    // ASSERT all three are f32, non-NaN, non-negative
}

// Test 2: NodeMeta includes stability fields when available
#[tokio::test]
async fn node_meta_includes_stability_fields() {
    // GIVEN a graph with stability analysis computed
    // WHEN GET /node/{id} is called
    // THEN response includes stability_variance, stability_drift
    // ASSERT variance >= 0.0, drift in [0.0, 1.0]
}

// Test 3: NodeMeta includes anomaly flag
#[tokio::test]
async fn node_meta_includes_anomaly_flag() {
    // GIVEN a graph with edge anomaly detection run
    // WHEN GET /node/{id} is called
    // THEN response includes anomaly_flag (bool)
    // ASSERT true when node participates in anomalous edge, false otherwise
}

// Test 4: Old fields are unchanged (regression)
#[tokio::test]
async fn node_meta_old_fields_unchanged() {
    // GIVEN same graph as before
    // WHEN GET /node/{id} is called
    // THEN degree, pagerank, betweenness, community, kcore, wcc are unchanged
    // ASSERT all existing field values match pre-change values
}

// Test 5: Energy fields are absent when not computed
#[tokio::test]
async fn node_meta_energy_absent_when_not_computed() {
    // GIVEN a graph WITHOUT energy decomposition
    // WHEN GET /node/{id} is called
    // THEN energy fields are None/null, not zero
    // ASSERT Option<f32> serializes as null
}

// Test 6: Protobuf round-trip preserves new fields
#[tokio::test]
async fn protobuf_roundtrip_preserves_energy_fields() {
    // GIVEN a NodeMeta with energy fields set
    // WHEN serialized to protobuf bytes and deserialized
    // THEN energy fields match original values
    // ASSERT all three f32 values are bit-identical
}

// Test 7: Protobuf backward compatibility
#[tokio::test]
async fn protobuf_backward_compatible() {
    // GIVEN a protobuf message WITHOUT new fields (old client)
    // WHEN deserialized by new server code
    // THEN new fields are None (optional)
    // ASSERT no parse error, no panic
}

// Test 8: Edge anomaly endpoint
#[tokio::test]
async fn edge_anomaly_endpoint_returns_sorted_list() {
    // GIVEN a graph with anomalous edges
    // WHEN GET /graph/metrics/edge-anomaly?threshold=3.0 is called
    // THEN returns JSON array of EdgeAnomaly sorted by score desc
    // ASSERT each entry has source, target, anomaly_score, jaccard_actual, jaccard_expected
}

// Test 9: Edge anomaly threshold filtering
#[tokio::test]
async fn edge_anomaly_endpoint_filters_by_threshold() {
    // GIVEN a graph with edges at anomaly scores [1.5, 2.8, 4.2, 6.1]
    // WHEN GET /graph/metrics/edge-anomaly?threshold=3.0 is called
    // THEN only edges with score >= 3.0 are returned
    // ASSERT result length == 2
}
```

---

## L2: Frontend Component Tests (app/ui)

### File: `app/ui/tests/inspector_tests.rs` (new)

These tests verify the Dioxus Inspector panel renders the correct RSX elements.
They should FAIL today because the new rows don't exist.

```rust
// Test 1: Inspector renders energy row after wcc
#[test]
fn inspector_renders_energy_row_after_wcc() {
    // GIVEN a NodeMeta with energy_total = 1.2345
    // WHEN the Inspector panel renders
    // THEN the metrics-grid contains a kv row with key "energy" and value "1.2345"
    // ASSERT the row appears after wcc in DOM order
}

// Test 2: Inspector renders stability row after energy
#[test]
fn inspector_renders_stability_row_after_energy() {
    // GIVEN a NodeMeta with stability_variance = 42.0
    // WHEN the Inspector panel renders
    // THEN the metrics-grid contains a kv row with key "stability" and value "42.00"
    // ASSERT the row appears after energy in DOM order
}

// Test 3: Inspector energy format is 4 decimal places
#[test]
fn inspector_energy_uses_4_decimal_format() {
    // GIVEN energy_total = 1.23456789
    // THEN rendered value is "1.2346" (rounded to 4 decimals)
}

// Test 4: Inspector shows anomaly banner when flagged
#[test]
fn inspector_shows_anomaly_banner_when_flagged() {
    // GIVEN anomaly_flag = true
    // THEN a warning div with class "ins-anomaly-warn" is rendered
    // ASSERT it contains "⚠ Anomalous edges connected"
}

// Test 5: Inspector hides anomaly banner when clean
#[test]
fn inspector_hides_anomaly_banner_when_clean() {
    // GIVEN anomaly_flag = false
    // THEN no div with class "ins-anomaly-warn" exists
}

// Test 6: Metrics panel renders convergence status
#[test]
fn metrics_panel_renders_convergence_status() {
    // GIVEN convergence state is "converged"
    // THEN metrics panel shows "Converged ✓" with green styling
}

// Test 7: Convergence states have distinct styles
#[test]
fn convergence_states_have_distinct_styles() {
    // converged → green checkmark
    // diverging → red warning
    // oscillating → yellow caution
    // ASSERT each state maps to correct CSS class
}
```

### File: `app/ui/tests/edge_inspector_tests.rs` (new)

```rust
// Test 1: Edge Inspector registered in panel catalog
#[test]
fn edge_inspector_is_registered_panel() {
    // ASSERT Panel::EdgeInspector exists in the enum
    // ASSERT PanelKind::title returns "Edge Inspector"
}

// Test 2: Edge Inspector renders empty state
#[test]
fn edge_inspector_empty_state() {
    // GIVEN no anomalous edges
    // THEN "No anomalous edges found" message is rendered
}

// Test 3: Edge Inspector threshold slider filters rows
#[test]
fn edge_inspector_threshold_filters() {
    // GIVEN sample anomalies at [1.9, 3.2, 5.1, 7.2]
    // WHEN threshold is set to 3.0
    // THEN only [3.2, 5.1, 7.2] are visible
}

// Test 4: Edge Inspector sort modes work
#[test]
fn edge_inspector_sort_by_anomaly() {
    // GIVEN unsorted anomalies
    // WHEN SortBy::Anomaly is selected
    // THEN rows are sorted by score descending (7.2 first, 1.9 last)
}

// Test 5: Select source/target works
#[test]
fn edge_inspector_select_source_changes_selection() {
    // GIVEN an edge row with source "Node 12"
    // WHEN "Select source" is clicked
    // THEN ctx.selected becomes "Node 12"
}

// Test 6: Panel catalog test includes EdgeInspector
#[test]
fn panel_catalog_includes_edge_inspector() {
    // Verify the test in main.rs that iterates all panels includes EdgeInspector
}
```

### File: `app/ui/tests/regime_tests.rs` (new — Phase 4)

```rust
// Test 1: Default regime is Layout
#[test]
fn default_regime_is_layout() {
    // ASSERT ctx.regime == Regime::Layout on fresh start
}

// Test 2: Regime switch changes energy label
#[test]
fn docking_regime_changes_energy_label() {
    // GIVEN regime = Docking
    // THEN Inspector energy key renders as "Binding Energy" not "energy"
}

// Test 3: Regime switch adds kJ/mol unit
#[test]
fn docking_regime_adds_kjmol_unit() {
    // GIVEN regime = Docking, energy_total = 42.0
    // THEN Inspector energy value renders as "42.0000 kJ/mol"
}

// Test 4: Regime persists in localStorage
#[test]
fn regime_persists_across_restart() {
    // GIVEN regime set to Docking, saved to localStorage
    // WHEN app restarts and reads localStorage
    // THEN regime is restored to Docking
}

// Test 5: Layout regime labels unchanged
#[test]
fn layout_regime_labels_unchanged() {
    // GIVEN regime = Layout
    // THEN all labels match pre-change values (backward compatible)
}
```

---

## L3: Browser Smoke Tests (test-browser)

### File: `crates/test-browser/tests/inspector_metrics.rs` (new)

```rust
// Test 1: Inspector panel shows energy after layout
#[tokio::test]
async fn inspector_shows_energy_after_layout() {
    // Boot app with test vault
    // Select a node
    // Wait for layout to complete (energy computed)
    // Assert Inspector contains "energy" key in metrics-grid
}

// Test 2: Style panel has Stability coloring option
#[tokio::test]
async fn style_panel_has_stability_option() {
    // Open Style panel
    // Click "Color by" dropdown
    // Assert "Stability" option exists in the list
}

// Test 3: Edge Inspector panel opens
#[tokio::test]
async fn edge_inspector_panel_opens() {
    // Open Edge Inspector from panel menu
    // Assert panel renders with title "Edge Inspector"
    // Assert at least one anomaly row or empty state message
}

// Test 4: Regime dropdown exists in Settings
#[tokio::test]
async fn settings_has_regime_dropdown() {
    // Open Settings panel
    // Assert "Regime" dropdown exists with "Layout" selected by default
}
```

---

## Test Execution Order

### Before ANY Phase 1 implementation:
```
cargo test -p graph-api -- inspector_metrics   # ALL FAIL (9 tests — fields don't exist)
cargo test -p app-ui -- inspector              # ALL FAIL (7 tests — rows don't render)
```

### After Chain A (Proto + API + Inspector rows):
```
cargo test -p graph-api -- inspector_metrics   # ALL PASS (9 tests)
cargo test -p app-ui -- inspector              # ALL PASS (7 tests)
cargo test -p app-ui -- edge_inspector         # ALL PASS (6 tests — already done ✅)
```

### After Chain B (Shader pipeline + Style panel):
```
cargo test -p app-ui -- style                  # NEW TESTS PASS
just test browser-rust                         # 2 new tests pass
```

### After Chain C (Progress streaming + Timeline):
```
cargo test -p app-ui -- convergence            # NEW TESTS PASS
just test browser-rust                         # 1 new test passes
```

### After Phase 4 (Regime):
```
cargo test -p app-ui -- regime                 # 5 tests PASS
just test browser-rust                         # 1 new test passes
```


---

## Test Reality: What Can Actually Run

### Layer 0: Backend data tests (NATIVE — cargo test works)
✅ 30 tests already passing (Phase 0 modules)
✅ Can add graph-api integration tests (L1 from above)

### Layer 1: Compilation verification (WASM — cargo check only)
`just app-check` verifies the frontend compiles for `wasm32-unknown-unknown`.
This is the gate for ALL frontend changes. It catches:
- Missing imports
- Type errors in RSX macros
- Missing panel registration points
- Wrong function signatures

**This is the primary TDD gate for frontend work.**

### Layer 2: Browser smoke tests (chromiumoxide — `just test browser-rust`)
Only 4 tests exist today (boot log, canvas dimensions, screenshot).
Adding new tests requires appending to `crates/test-browser/src/main.rs`.

### Layer 3: Dioxus component tests (NOT AVAILABLE)
Dioxus 0.6 targets `wasm32-unknown-unknown` with the `web` feature.
There is no `dioxus-test` crate for server-side rendering in this version.
Component-level assertions on RSX output are NOT possible without a browser.

---

## Revised TDD Execution Plan

### Step 1: Write backend integration tests → they FAIL
```bash
# Write crates/graph-api/tests/inspector_metrics.rs (9 tests)
# These compile and run natively
cargo test -p graph-api -- inspector_metrics
# EXPECTED: 0 passed, 9 failed — fields don't exist yet
```

### Step 2: Implement proto + API changes → tests PASS
```bash
# Extend graph.proto NodeMeta with energy/stability/anomaly fields
# Regen WASM proto (just app-proto)
# Wire through graph-api handlers
cargo test -p graph-api -- inspector_metrics
# EXPECTED: 9 passed, 0 failed
```

### Step 3: Implement frontend Inspector rows → cargo check PASSES
```bash
# Add energy/stability rows to inspector.rs
just app-check
# EXPECTED: compiles clean for wasm32-unknown-unknown
```

### Step 4: Browser smoke test → renders correctly
```bash
just test browser-rust
# EXPECTED: existing 4 tests pass + new Inspector test passes
```

### Step 5: Repeat for each chain (B: Shader, C: Progress streaming, Phase 4: Regime)

---

## Immediate TDD Actions (This Session)

### Action 1: Create `crates/graph-api/tests/inspector_metrics.rs`
This is the ONLY test file that can actually run today.
It will FAIL because the NodeMeta proto hasn't been extended yet.
This proves the feature gap and becomes the acceptance test.

### Action 2: Extend proto → make L1 tests pass
### Action 3: Frontend changes → verify with `just app-check`
### Action 4: Browser smoke → verify with `just test browser-rust`
