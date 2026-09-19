# Implementation Plan: Water-Energy Docking ↔ Jump-Cannon Bidirectional Transfer

**Generated:** 2026-09-19
**Source:** Tree of Thoughts research session (`docs/research/water-energetic-docking/`), 36 documents, 754 KB
**Status:** Planning phase — no code written yet

## Overview

This plan maps the 16 concrete proposals from the research session into a phased implementation roadmap. Each phase produces a working, testable increment. Phases are ordered by dependency: backend primitives before UI features, data plumbing before visualization, small validated steps before large speculative ones.

## Phase Map

```
Phase 0: Foundation Primitives (backend-only, no UI)
    │
Phase 1: Inspector & Metrics Surface (existing panels, new data)
    │
Phase 2: Visual Overlays (canvas heatmaps, color semantics)
    │
Phase 3: New Panels (Timeline, Edge Inspector)
    │
Phase 4: Regime Abstraction (Settings dropdown, dual rendering)
    │
Phase 5: Cross-Regime Bridge (verified round-trip, Lean proofs)
```

---

## Phase 0: Foundation Primitives

**Goal:** Backend data structures and computations that Phase 1–3 UI features consume. No user-visible changes.

**Duration estimate:** 2–3 sessions
**Risk:** Low (greenfield additions to existing crates)

### 0.1 Per-Node Energy Decomposition

**Crate:** `graph-layouts`
**File:** New `crates/graph-layouts/src/layout/algorithms/energy_decomposition.rs`

Add a function that, given a layout state and the force model parameters, computes per-node energy contributions:

```rust
/// Per-node energy decomposition (analog of per-residue MM-GBSA decomposition).
pub struct NodeEnergy {
    /// Total energy contribution from this node (sum of its incident edges + repulsion)
    pub total: f32,
    /// Attractive energy (springs on incident edges)
    pub attractive: f32,
    /// Repulsive energy (Barnes-Hut or exact, depending on mode)
    pub repulsive: f32,
    /// Gravity/centering term
    pub gravity: f32,
    /// Number of incident edges
    pub edge_count: u32,
    /// Energy per edge (normalized)
    pub energy_per_edge: f32,
}

pub fn compute_per_node_energy(
    positions: &[f32],
    edges: &[(u32, u32)],
    edge_strengths: &[f32],
    params: &GpuForceOptions,
    octree: Option<&Octree>,
) -> Vec<NodeEnergy>;
```

**Dependencies:** None (pure computation on existing data structures)
**Test:** Verify `sum(total) ≈ global_stress` within floating-point tolerance on toy graphs (triangle, square, star).

### 0.2 Layout WaterMap (Per-Node Stability)

**Crate:** `graph-layouts`
**File:** New `crates/graph-layouts/src/layout/algorithms/stability.rs`

Run N short layout perturbations and compute per-node positional variance:

```rust
/// LayoutWaterMap: per-node stability analysis via perturbed re-layout.
/// Analogous to WaterMap's hydration site thermodynamics.
pub struct StabilityProfile {
    /// Per-node positional variance (analog of dG_hyd)
    pub variance: Vec<f32>,
    /// Per-node drift (mean displacement from initial position)
    pub drift: Vec<f32>,
    /// Global stability score (mean variance across all nodes)
    pub global_stability: f32,
    /// Number of perturbation steps used
    pub perturbation_steps: u32,
}

pub fn compute_stability(
    positions: &[f32],
    edges: &[(u32, u32)],
    params: &GpuForceOptions,
    perturbations: u32,  // N perturbation runs (default: 100)
    steps_per_perturbation: u32,  // steps per run (default: 50)
) -> StabilityProfile;
```

**Algorithm:** 
1. Add small random jitter to all positions
2. Run `steps_per_perturbation` force iterations
3. Record final position for each node
4. Repeat `perturbations` times
5. Compute per-node variance: σ² = Var(final_positions across runs)
6. Compute per-node drift: |mean(final_pos) - initial_pos|

**Dependencies:** None (calls existing force step functions)
**Test:** On a stable layout (e.g., grid), variance ≈ 0. On a barely-stable layout (e.g., random initialization after 5 steps), variance is high. Verify that variance decreases monotonically with more layout iterations.

### 0.3 Edge Anomaly Detection

**Crate:** `graph-metrics`
**File:** New `crates/graph-metrics/src/edge_anomaly.rs` or add to existing `edge_strength.rs`

```rust
/// Edge anomaly score: how much lower is the actual Jaccard than expected
/// given the endpoint degrees? High anomaly = "magic methyl" edge.
pub struct EdgeAnomaly {
    pub edge_idx: u32,
    pub source: u32,
    pub target: u32,
    pub jaccard: f32,
    pub expected_jaccard: f32,
    pub anomaly_score: f32,  // expected / actual — higher = more surprising
    /// What would happen if this edge were removed?
    pub removal_impact: Option<f32>,  // stress change if removed (expensive, optional)
}

pub fn detect_anomalous_edges(
    graph: &Graph,
    threshold: f32,  // edges with anomaly_score > threshold are flagged
    compute_impact: bool,  // if true, compute stress change on removal
) -> Vec<EdgeAnomaly>;
```

**Dependencies:** `vault-data` (Graph), existing `edge_strength` module
**Test:** On a graph with two dense clusters connected by a single bridge edge, the bridge edge should have the highest anomaly score.

### 0.4 Stress Convergence Metrics

**Crate:** `graph-layouts`
**File:** Add to existing `fa2_speed.rs` or new `convergence.rs`

```rust
/// Convergence diagnostic: has the layout stabilized?
pub struct ConvergenceStatus {
    /// Current stress
    pub stress: f32,
    /// Cumulative running average of stress
    pub cumulative_avg: f32,
    /// Drift: |second_half_avg - first_half_avg|
    pub drift: f32,
    /// Is the layout converged? (drift < threshold)
    pub converged: bool,
    /// Steps since last significant stress change
    pub stable_steps: u32,
    /// Stress history (circular buffer, last N steps)
    pub stress_history: Vec<f32>,
}

pub fn check_convergence(
    stress_history: &[f32],
    threshold: f32,  // drift below this → converged
) -> ConvergenceStatus;
```

**Dependencies:** None (statistical computation on stress values)
**Test:** On a layout that converges, `drift → 0` and `converged → true`. On a layout that oscillates, `drift` stays above threshold.

---

## Phase 1: Inspector & Metrics Surface

**Goal:** Expose Phase 0 data in existing panels. No new panels, no visual changes to canvas. The user sees new rows in Inspector and new charts in Metrics.

**Duration estimate:** 1–2 sessions
**Risk:** Low (additive changes to existing panels)

### 1.1 Inspector: Per-Node Energy Row

**File:** `app/ui/src/panels/inspector.rs`

Add a section below the existing metric rows (degree, pagerank, community, kcore):

```
── Energy ──────────────────
  Total stress:     0.0034
  Attractive:       0.0021 (springs)
  Repulsive:        0.0013 (Barnes-Hut)
  Energy per edge:  0.00028
```

**Data flow:** Graph snapshot → `compute_per_node_energy()` → serialized in `NodeMeta` or separate binary buffer → fetched by Inspector.

**Depends on:** 0.1 (Per-Node Energy Decomposition)
**Effort:** Small

### 1.2 Inspector: Stability Row

```
── Stability ───────────────
  Variance:         0.12 (stable)
  Drift:            0.04
  Stability class:  ████░░ Blue (converged)
```

**Data flow:** Trigger `compute_stability()` on-demand (button in Inspector or Settings) → results cached in graph snapshot → Inspector reads per-node values.

**Depends on:** 0.2 (Layout WaterMap)
**Effort:** Small

### 1.3 Inspector: Edge Anomaly Alert

When an anomalous edge is incident to the selected node, show:

```
── Edge Anomaly ────────────
  ⚠ Edge to #891: anomaly 4.2× (Jaccard 0.02 vs expected 0.09)
  This edge may be a "magic methyl" — its removal would significantly restructure the layout.
```

**Data flow:** `detect_anomalous_edges()` runs on graph load → top-N anomaly list stored in snapshot → Inspector checks if selected node has any flagged edges.

**Depends on:** 0.3 (Edge Anomaly Detection)
**Effort:** Small

### 1.4 Metrics Panel: Convergence Plot

**File:** `app/ui/src/panels/metrics.rs`

Add a stress-over-time plot using the existing metrics panel infrastructure:

```
  Stress convergence
  0.05 │●
  0.04 │ ●●
  0.03 │   ●●●
  0.02 │      ●●●●●
  0.01 │           ●●●●●●●●
  0.00 │__________________●
       0    100   200   300   400   500
                   Step

  Status: Converged (drift 0.0003 < 0.001)
```

**Data flow:** `check_convergence()` consumes the stress history already tracked by the layout engine → serialized as a float buffer → Metrics panel renders with Dioxus SVG or canvas.

**Depends on:** 0.4 (Stress Convergence Metrics)
**Effort:** Medium

### 1.5 Metrics Panel: AdaptiveSpeed Display

Surface the FA2 controller state:

```
  Adaptive Speed
  Speed:            0.015
  Efficiency:       0.87
  Swing/Traction:   0.34
  Status:           Optimal (0.75 < efficiency < 0.95)
```

**Data flow:** `AdaptiveSpeed` struct already exists in `fa2_speed.rs` → expose via existing progress/compute streaming → Metrics panel reads.

**Depends on:** None (data already exists, just not surfaced)
**Effort:** Small

---

## Phase 2: Visual Overlays

**Goal:** Color the canvas and nodes with stability/energy semantics. This is where the topos-theoretic color commuting property becomes user-visible.

**Duration estimate:** 1–2 sessions
**Risk:** Medium (touches the wgpu render pipeline)

### 2.1 Layout Quality Heatmap (Node Stability Coloring)

**Files:** `app/ui/src/render/graph_pipelines.rs`, `app/ui/src/panels/style.rs`

Add a "Stability" coloring mode to the Style panel's existing node color options:

```
Style → Node Color → [Degree ▼]
                     ├─ Degree
                     ├─ Community
                     ├─ PageRank
                     ├─ Stability (new)
                     └─ ...
```

When "Stability" is selected:
- Nodes colored by `StabilityProfile.variance` on a red→blue scale
- Red (variance > 0.5): unstable, "high-energy water" — candidate for re-layout
- Yellow (0.2 < variance < 0.5): marginal
- Blue (variance < 0.2): stable, "low-energy water" — converged

**Shader impact:** Add a `node_stability` buffer to the node pipeline, populated when stability coloring is active. The shader already has per-node color support; this adds a new color source.

**Depends on:** 0.2 (Layout WaterMap)
**Effort:** Medium

### 2.2 Color Semantics Verification

**File:** `app/ui/src/panels/style.rs` or new test module

Add a test that verifies the color scale commutes with the translation:

```rust
#[test]
fn stability_color_commutes_with_translation() {
    // In docking regime: dG_hyd = +2.1 → red (unstable, displace)
    // In layout regime via f*: variance > 0.5 → red (unstable, re-layout)
    // They must produce the SAME color.
    let docking_color = watermap_color(2.1);  // hypothetical
    let layout_color = stability_color(0.6);  // real
    assert!(
        color_distance(docking_color, layout_color) < 0.05,
        "Translation functor color commuting violated"
    );
}
```

**Depends on:** 2.1 (Layout Quality Heatmap)
**Effort:** Small

### 2.3 Edge Anomaly Highlighting

Add an edge coloring mode "Anomaly" that colors edges by their anomaly score:
- Red: anomaly > 5× (magic methyl candidates)
- Yellow: 2× < anomaly < 5×
- Default edge color: anomaly < 2×

**Shader impact:** Add `edge_anomaly` buffer to edge pipeline, analogous to node stability.

**Depends on:** 0.3 (Edge Anomaly Detection)
**Effort:** Small

---

## Phase 3: New Panels

**Goal:** Add panels that don't exist yet but have a clear topos-theoretic justification.

**Duration estimate:** 2–3 sessions
**Risk:** Medium (new panel structure, but follows existing patterns)

### 3.1 Timeline Panel Enhancement

**File:** `app/ui/src/panels/timeline.rs` (already exists at 28 KB!)

The Timeline panel already exists in the parity plan. Enhance it with the convergence visualization from 0.4:

```
┌─ Timeline ─────────────────────────────────────────────┐
│  [◄◄] [◄] [▶] [►] [►►]     Step 142 / 500             │
│                                                        │
│  Stress                   Convergence                  │
│  0.05 │●                  Drift: 0.0003               │
│  0.04 │ ●●                Stable: 87 steps            │
│  0.03 │   ●●●             Status: Converged ✓         │
│  0.02 │      ●●●●●                                    │
│  0.01 │           ●●●●●●●●                            │
│  0.00 │__________________●                            │
│       0   100   200   300                              │
│                                                        │
│  ── Layout states ──────────────────────────────────── │
│  State 1 (step 0):   Random seed positions             │
│  State 2 (step 50):  Coarse structure emerging         │
│  State 3 (step 142): Converged (current)               │
└────────────────────────────────────────────────────────┘
```

**Depends on:** 0.4 (Stress Convergence Metrics), 0.2 (Layout WaterMap — for stability overlay)
**Effort:** Medium

### 3.2 Edge Inspector Panel (New)

**File:** New `app/ui/src/panels/edge_inspector.rs`

A new panel that lists edges sorted by anomaly score:

```
┌─ Edge Inspector ───────────────────────────────────────┐
│  Sort by: [Anomaly ▼]  Threshold: [3.0×  ──○──]       │
│                                                        │
│  ⚠ Edge #234: Node 12 → Node 891                      │
│    Anomaly: 4.2×  │  Jaccard: 0.02  │  Expected: 0.09 │
│    This edge connects two otherwise-separate clusters. │
│    Removing it would increase stress by 0.0034 (12%).  │
│    [Select source] [Select target] [Highlight path]    │
│                                                        │
│  ⚠ Edge #567: Node 3 → Node 445                       │
│    Anomaly: 3.8×  │  Jaccard: 0.01  │  Expected: 0.04 │
│    ...                                                 │
│                                                        │
│  12 anomalous edges found (threshold 3.0×)             │
└────────────────────────────────────────────────────────┘
```

**Panel registration:** Add `EdgeInspector` variant to the `Panel` enum in `app/ui/src/main.rs`.

**Depends on:** 0.3 (Edge Anomaly Detection)
**Effort:** Medium

---

## Phase 4: Regime Abstraction

**Goal:** Implement the "Regime" dropdown that lets the user switch between Graph Layout and Docking Energy rendering modes. This is the concrete realization of the topos translation functor `f*` at the UI layer.

**Duration estimate:** 2–3 sessions
**Risk:** High (cross-cutting: touches Settings, Inspector, Metrics, Style, and shader pipelines)

### 4.1 Regime Selector in Settings

**File:** `app/ui/src/panels/settings.rs`

Add a new Settings tab "Regime" or add a regime selector to the existing Layout tab:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderingRegime {
    #[default]
    GraphLayout,
    DockingEnergy,
    // Future: Custom(String),
}
```

Persisted to localStorage alongside other Settings state.

**When regime changes:**
1. Inspector panel re-renders: shows "per-residue" labels instead of "per-node"
2. Metrics panel re-renders: stress → "ΔG_bind", variance → "ΔG_hyd"
3. Style panel: color scales use thermodynamic labels
4. No backend changes — same engine, different rendering lens

**Effort:** Small (pure UI plumbing)

### 4.2 Inspector Regime-Aware Rendering

When regime is `DockingEnergy`, the Inspector shows:

```
── Binding Energy ─────────
  ΔG_bind:         -8.2 kcal/mol  (was: Stress: 0.0034)
  ΔH (MM):         -12.4 kcal/mol (was: Attractive: 0.0021)
  ΔG_solv:         +4.2 kcal/mol  (was: Repulsive: 0.0013)

── Water Thermodynamics ───
  ΔG_hyd:          +2.1 kcal/mol  (was: Variance: 0.12)
  −TΔS:            +5.5 kcal/mol  (was: Drift: 0.04)
  Hydration class:  Red (displace) (was: Stability: unstable)
```

**Data flow:** Same underlying floats. The rendering layer applies the translation labels and (optionally) unit conversions. The topos translation table (§16) defines the exact mapping.

**Depends on:** 1.1, 1.2, 4.1
**Effort:** Medium

### 4.3 Settings Refinement Validation

Add tooltips to every Settings control explaining its role in the refinement funnel:

```
Layout → Engine: [multilevel ▼]
  ℹ This engine implements a multi-resolution cascade: coarsen →
  solve coarsest → prolong → refine. Analogous to the Glide
  SP→XP→WS→MM-GBSA→FEP+ deployment funnel. Each level trades
  speed for accuracy.

Layout → θ: [0.5 ───○──]
  ℹ Barnes-Hut opening angle. Controls the approximation fidelity
  of spatial partitioning. θ=0: exact O(n²) (analogous to explicit
  solvent MD). θ=1.0: coarsest O(n log n) (analogous to GB implicit
  solvent). Default 0.5 is the "sweet spot" (analogous to Nwat=30).
```

**Depends on:** 4.1
**Effort:** Small

---

## Phase 5: Cross-Regime Bridge

**Goal:** Implement the verified round-trip translation between docking and layout regimes. This is the research-phase item that proves the topos formalization on real data.

**Duration estimate:** 3–5 sessions (research, not production)
**Risk:** High (external Lean project, AMBER topology parsing, FFI between Lean and Rust)

### 5.1 AMBER Topology Parser (Rust)

**Crate:** New `crates/docking-bridge` (or add to `graph-compute`)

Parse AMBER `.prmtop` and `.inpcrd` files into Rust structs:

```rust
pub struct AmberTopology {
    pub atoms: Vec<AmberAtom>,
    pub bonds: Vec<(u32, u32, f32)>,  // (i, j, force_constant)
    pub angles: Vec<(u32, u32, u32, f32)>,
    pub dihedrals: Vec<(u32, u32, u32, u32, f32, f32, u32)>,
    pub charges: Vec<f32>,
    pub masses: Vec<f32>,
    pub residue_labels: Vec<String>,
}
```

**Effort:** Large (`.prmtop` is a complex binary format with extensive metadata)

### 5.2 Translation Functor Implementation (Rust)

Implement `poseToLayout` and `layoutToPose` from the translation table (§16):

```rust
impl BindingPose {
    pub fn to_layout_state(&self) -> LayoutState {
        LayoutState {
            graph: self.protein_to_graph(),
            node_positions: self.ligand_to_node_positions(),
            dynamic_edges: self.waters_to_edges(),
            stress: self.free_energy,
        }
    }
}
```

**Effort:** Medium (straightforward mapping, no new algorithms)

### 5.3 Round-Trip Test

```rust
#[test]
fn round_trip_preserves_energy() {
    let original = parse_amber_system("hiv_pr_complex.prmtop", "hiv_pr_complex.inpcrd");
    let layout = original.to_layout_state();
    let relaxed = run_gpu_layout(layout, 500);
    let result = relaxed.to_binding_pose();
    
    let error = (original.free_energy - result.free_energy).abs();
    assert!(error < 1.0, "Round-trip energy error {} exceeds 1.0 kcal/mol bound", error);
}
```

**Depends on:** 5.1, 5.2
**Effort:** Medium

### 5.4 Lean 4 Verified Transport (External)

**Repository:** New `lean-docking-topos/` (outside jump-cannon, but cross-referenced)

Implement the Lean 4 formalization from the topos document (Parts I–III) as an actual Lean project:

1. `EnergyMinimizer.lean` — The common abstraction structure
2. `Refinement.lean` — Refinement relations and funnel chains
3. `TranslationFunctor.lean` — `f* : Sh(D) ⇄ Sh(G)` geometric morphism
4. `TransportTheorem.lean` — Proof that geometric formulas transport
5. `GaloisConnection.lean` — Bidirectional refinement Galois connection
6. `VerifiedRoundTrip.lean` — Lean proof that the Rust round-trip preserves energy within ε

**Effort:** X-Large (multi-week research project)
**Dependencies:** None (standalone Lean project)

---


## Phase 0 Completion Notes (2026-09-19)

Phase 0 is complete. All four backend modules are implemented and tested:

| Module | Crate | File | Tests |
|---|---|---|---|
| 0.1 Per-node energy decomposition | `graph-layouts` | `energy_decomposition.rs` | 5 ✅ |
| 0.2 Layout WaterMap | `graph-layouts` | `stability.rs` | 4 ✅ |
| 0.3 Edge anomaly detection | `graph-metrics` | `edge_anomaly.rs` | 7 ✅ |
| 0.4 Stress convergence | `graph-layouts` | `convergence.rs` | 14 ✅ |

**Total: 137 tests pass workspace-wide** (`cargo test -p graph-layouts -p graph-metrics`).

## Revised Phase 1–2 Dependency Analysis

The UI panel audit (`docs/research/water-energetic-docking/04-benchmarks/lean-topos/ui-panel-audit.md`, 19.6 KB) identified blockers for each Phase 1–3 item:

### Phase 1 Blockers

| Item | Blocker | What's Needed |
|---|---|---|
| 1.1 Inspector energy row | `NodeMeta` proto needs new fields (tag 23+) | Add `node_energy`, `node_energy_attractive`, `node_energy_repulsive` to `graph.proto`, regen WASM proto |
| 1.2 Inspector stability row | Same proto change | Add `stability_variance`, `stability_drift` fields |
| 1.3 Inspector edge alert | Edge anomaly data must flow through graph-api → frontend | New endpoint or add to NodeMeta |
| 1.4 Metrics convergence plot | New rendering infrastructure needed in metrics panel | Canvas/SVG rendering; data channel from layout host |
| 1.5 AdaptiveSpeed display | **No blocker** — data already exists in `fa2_speed.rs` | Pure frontend: read from existing progress stream, render in metrics panel |

### Phase 2 Blockers

| Item | Blocker | What's Needed |
|---|---|---|
| 2.1 Stability heatmap | New backend endpoint + new shader coloring path | `/graph/metrics/stability` endpoint; `node_stability` buffer in node.wgsl |
| 2.2 Color verification | Test infrastructure | Rust test in `app/ui` or `graph-layouts` |
| 2.3 Edge anomaly highlighting | Same as 2.1 but for edges | `edge_anomaly` buffer in edge.wgsl |

### Phase 3–4 Blockers

| Item | Blocker | What's Needed |
|---|---|---|
| 3.1 Timeline convergence | Per-frame metric streaming over `/progress` | Extend progress event protocol |
| 3.2 Edge Inspector panel | No backend blockers | Pure frontend: new panel, `PanelKind` variant, data from existing anomaly module |
| 4.1 Regime dropdown | Regime concept needs backend support | `RenderingRegime` enum, regime-aware serialization |
| 4.2 Regime-aware Inspector | Same as 4.1 | Regime-aware label/unit rendering |
| 4.3 Settings refinement tooltips | No blockers | Pure frontend: tooltip text additions |

## Revised Parallel Dispatch Strategy

Given these blockers, the optimal next dispatch is:

### Can dispatch NOW (no backend blockers):
- **Item 1.5**: AdaptiveSpeed display in Metrics panel (pure frontend)
- **Item 3.2**: Edge Inspector panel (pure frontend, new panel)
- **Item 4.3**: Settings refinement validation tooltips (pure frontend)

### Needs coordinated backend+frontend (single agent per chain):
- **Chain A (Proto → API → Inspector)**: 0.1/0.2 data → NodeMeta proto extension → graph-api endpoint → Inspector rows (items 1.1–1.3)
- **Chain B (Shader pipeline)**: 0.2/0.3 data → shader buffers → Style panel coloring modes (items 2.1–2.3)
- **Chain C (Progress streaming)**: 0.4 data → progress event extension → Metrics/Timeline rendering (items 1.4, 3.1)

### Deferred (needs design consensus):
- **Item 4.1–4.2**: Regime dropdown + regime-aware rendering (architectural decision)


### Item 1.5 Re-assessment (2026-09-19)

**Finding by phase1-adaptive-speed agent**: AdaptiveSpeed data (speed, speed_efficiency, swing, traction) does NOT exist in the frontend path.

- `graph-compute` (gRPC broker, server-side) has `fa2_speed.rs` with full AdaptiveSpeed state machine for FA2 engines
- `graph-layouts` (in-process GPU sim, frontend path) uses t-FDP force model and has no AdaptiveSpeed mechanism
- The frontend WebSocket frames carry only positions — no speed metadata
- Progress events (`/progress` SSE) have no speed fields

**Revised scope**: Item 1.5 is NOT frontend-only. It requires either:
1. Adding an AdaptiveSpeed controller to `graph-layouts`'s `GpuForceLayout` (new `DynPhysicsLayout` trait method)
2. Extending the WebSocket frame format in the compute broker path
3. Or computing an equivalent metric from t-FDP's existing `last_max_ke()` and recent position deltas

**Recommendation**: Merge into Chain C (Progress streaming) — item 3.1. The timeline panel already needs per-frame metric streaming; AdaptiveSpeed is one of the metrics that would stream.

## Dependency Graph

```
0.1 Energy Decomp ──┬──► 1.1 Inspector Energy Row ──► 4.2 Regime-Aware Inspector
                    │
0.2 Layout WaterMap ──┬──► 1.2 Inspector Stability Row ──► 4.2
                      ├──► 2.1 Layout Quality Heatmap ──► 2.2 Color Verification
                      └──► 3.1 Timeline Enhancement
                      
0.3 Edge Anomaly ──┬──► 1.3 Inspector Edge Alert
                   ├──► 2.3 Edge Anomaly Highlighting
                   └──► 3.2 Edge Inspector Panel

0.4 Convergence ──┬──► 1.4 Metrics Convergence Plot
                  ├──► 1.5 AdaptiveSpeed Display
                  └──► 3.1 Timeline Enhancement

4.1 Regime Dropdown ──► 4.2 Regime-Aware Inspector ──► 4.3 Settings Refinement Validation

5.1 AMBER Parser ──► 5.2 Translation Functor ──► 5.3 Round-Trip Test ──► 5.4 Lean Verification
```

---

## What Can Be Parallelized

The following can run in parallel (different crates, no shared state):
- **Session A:** 0.1 + 0.4 (Energy Decomposition + Convergence) → Phase 1
- **Session B:** 0.2 (Layout WaterMap) → Phase 2
- **Session C:** 0.3 (Edge Anomaly) → Phase 3

UI work in Phases 1–3 can also parallelize across panels:
- **Session D:** Inspector changes (1.1, 1.2, 1.3)
- **Session E:** Metrics panel changes (1.4, 1.5)
- **Session F:** Timeline panel (3.1)

Phase 4 requires Phase 1–3 completion (needs the data to render through the regime lens).
Phase 5 is independent of Phases 1–4 (greenfield crate + external Lean project).

---

## GitHub Project Milestones

### Milestone 1: "Water-Energy Primitives" (Phase 0)
- [x] 0.1 Per-node energy decomposition (`graph-layouts`)
- [x] 0.2 Layout WaterMap stability analysis (`graph-layouts`)
- [x] 0.3 Edge anomaly detection (`graph-metrics`)
- [x] 0.4 Stress convergence metrics (`graph-layouts`)
- **Gate:** All four modules tested on toy graphs, PR merged

### Milestone 2: "Inspector & Metrics Surface" (Phase 1)
- [ ] 1.1 Inspector: per-node energy row
- [ ] 1.2 Inspector: stability row
- [ ] 1.3 Inspector: edge anomaly alert
- [ ] 1.4 Metrics: convergence plot
- [ ] 1.5 Metrics: AdaptiveSpeed display
- **Gate:** Screenshot showing new Inspector and Metrics content

### Milestone 3: "Visual Overlays" (Phase 2)
- [ ] 2.1 Layout quality heatmap (node stability coloring)
- [ ] 2.2 Color semantics verification test
- [ ] 2.3 Edge anomaly highlighting
- **Gate:** `just test browser-rust` passes with new coloring modes

### Milestone 4: "Timeline & Edge Inspector" (Phase 3)
- [ ] 3.1 Timeline panel with convergence visualization
- [ ] 3.2 Edge Inspector panel (new)
- **Gate:** Both panels render in the workspace, drag/dock works

### Milestone 5: "Regime Abstraction" (Phase 4)
- [ ] 4.1 Regime selector in Settings
- [ ] 4.2 Inspector regime-aware rendering
- [ ] 4.3 Settings refinement validation tooltips
- **Gate:** Switching regime re-renders all panels with correct labels

### Milestone 6: "Cross-Regime Bridge" (Phase 5)
- [ ] 5.1 AMBER topology parser (`docking-bridge` crate)
- [ ] 5.2 Translation functor implementation
- [ ] 5.3 Round-trip test (energy error < 1.0 kcal/mol)
- [ ] 5.4 Lean 4 verified transport (external repo)
- **Gate:** Round-trip test passes; Lean project compiles with transport theorem stated

---

## Effort Summary

| Phase | Items | Effort | Risk | User-Visible? |
|---|---|---|---|---|
| 0: Primitives | 4 | Medium | Low | No |
| 1: Inspector/Metrics | 5 | Small–Medium | Low | Yes (new panel rows/charts) |
| 2: Visual Overlays | 3 | Medium | Medium | Yes (canvas coloring) |
| 3: New Panels | 2 | Medium | Medium | Yes (new panels) |
| 4: Regime Abstraction | 3 | Medium–Large | High | Yes (cross-cutting UI change) |
| 5: Cross-Regime Bridge | 4 | Large–X-Large | High | No (research/validation) |
| **Total** | **21 items** | | | |

---

## First Actions (This Session)

1. Create `docs/implementation-plan.md` (this document) — ✅ Done
2. Create GitHub Project with the 6 milestones above
3. Create issue for 0.1 (Energy Decomposition) — simplest, highest-value first item
4. Label issues by phase and crate
5. Decide: start Phase 0 inline or dispatch to sub-agents?

## Next Session Handoff

- Research complete: 36 files, 754 KB in `docs/research/water-energetic-docking/`
- Implementation plan: `docs/implementation-plan.md`
- Key decision needed: **Phase 5 scope** — does the Lean verification happen in-tree (as a workspace member gated behind a feature flag) or out-of-tree (separate repo)?
- Sub-agents from the research session all completed; their results are integrated
- No code written yet — this is a pure planning artifact
