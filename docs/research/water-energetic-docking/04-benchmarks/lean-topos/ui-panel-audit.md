# UI Panel Audit: Jump Cannon Frontend Structure

**Date**: 2026-09-19
**Purpose**: Identify precise insertion points for the LeanTopos water-energetic-docking UI features: energy/stability metrics in the Inspector, convergence plot and AdaptiveSpeed display in Metrics, Regime dropdown in Settings (Layout), Stability coloring mode in Style, convergence visualization in Timeline, and a new EdgeInspector panel.

---

## 1. Inspector Panel (`app/ui/src/panels/inspector.rs`)

**Size**: 804 lines, 31,976 bytes

### Current structure

The inspector has two main code sections:

#### Data model
- **`Persisted`** (L51): localStorage shape for the "browse tags" query.
- **`Dir` enum** (L159): Link direction (`Incoming`, `Outgoing`) relative to the focused node.
- **`Pill` struct** (L222): Precomputed data for one clickable node pill row.

#### Functions
| Function | Line | Purpose |
|---|---|---|
| `persist()` | L62 | Write to localStorage |
| `ensure_community_metric()` | L82 | Fetch `/graph/metrics/community` |
| `community_tint()` | L107 | Color swatch for community |
| `open_url()` | L147 | URL chip click → new tab |
| `neighbor_set()` | L185 | Walk packed edge list for neighbors |
| `short_id_for_pill()` | L209 | Truncate id for pill label |
| `build_pills()` | L229 | Build pill row data |
| `pill_list()` | L262 | Render clickable node pills |
| `active_filter_strip()` | L304 | Active-filter chip strip |
| `fuzzy_score()` | L351 | Subsequence fuzzy match |
| `browse_tags()` | L384 | Empty-state tag browser |
| `grid_skipped()` | L512 | Keys promoted to NodeMeta |
| `chip_walker_handles()` | L519 | Check if chip walker emits |
| `frontmatter_grid()` | L538 | Collapsed frontmatter section |
| `fm_value_cell()` | L573 | One leftover frontmatter value |
| **`node_view()`** | **L629** | **Main node detail view** |
| **`community_and_neighbors()`** | **L729** | **Community + neighbors sections** |

### Data fields displayed (L686–L700)

The metric grid inside `node_view()` renders these rows:

```rust
div { class: "metrics-grid",
    // idx (if Some)
    div { class: "kv", "degree" → "{m.degree} ({m.indegree} in / {m.outdegree} out)" }
    div { class: "kv", "pagerank" → format!("{:.4}", m.pagerank) }
    div { class: "kv", "betweenness" → format!("{:.4}", m.betweenness) }
    div { class: "kv", "community" → "{m.community}" }
    div { class: "kv", "kcore" → "{m.kcore}" }
    div { class: "kv", "wcc" → "{m.wcc}" }
}
```

The identity rows above this grid are:
```rust
div { class: "ins-id", "{id}" }           // L687
div { class: "kv", "title" → "{m.title}" } // L688
div { class: "kv", "path" → "{m.path}" }   // L689
```

Data comes from a `NodeMeta` protobuf struct fetched via `/node/:id`.

### Insertion point for energy/stability rows

**Primary location**: After L699 (`wcc` row), before the closing `</div>` of `.metrics-grid` at L700.

New rows would look like:
```rust
div { class: "kv", span { class: "k", "energy" } span { class: "v", { format!("{:.4}", m.energy) } } }
div { class: "kv", span { class: "k", "stability" } span { class: "v", { format!("{:.4}", m.stability) } } }
```

**Prerequisites**:
1. Add `energy: f32` and `stability: f32` fields to the `NodeMeta` protobuf message in `app/ui/src/proto/jumpcannon.graph.rs`.
2. The backend (`crates/graph-api`) must include these fields in `/node/:id` responses.
3. If the fields are `Option<f32>`, gate with `if let Some(v) = m.energy { ... }`.

**Blockers**: The `NodeMeta` struct is protobuf-generated. The proto file is checked in at `app/ui/src/proto/jumpcannon.graph.rs`. Regenerate via `just app-proto` after editing the `.proto` source.

---

## 2. Metrics Panel (`app/ui/src/panels/metrics.rs`)

**Size**: 323 lines, 11,785 bytes

### Current structure

#### Data model
- **`MetricKind` enum** (L27–L32): Four layout-quality metrics:
  - `EdgeLengthCv` — coefficient of variation of edge lengths, O(E)
  - `EdgeStress` — scale-normalized stress over edges, O(E)
  - `FullStress` — all-pairs stress, O(n²), on-demand
  - `Crossings` — edge crossings, O(E²), on-demand

- **`MetricsSnapshot` struct** (L95–L105): Computed values for the active layout:
  ```rust
  n_nodes: u32,
  n_edges: u32,
  edge_length_cv: f32,
  edge_stress: f32,
  full_stress: Option<f32>,
  crossings: Option<u32>,
  ```

- **`Persisted`** (L114–L120): localStorage shape (`pinned`, `last`, `auto`).

#### Rendering (L211–L323)
- **`panel()`** (L211): Entry point, calls `MetricsPanel` component.
- **`MetricsPanel`** (L217): Dioxus component with:
  - Auto-compute timer (150ms loop for O(E) metrics when live)
  - "Compute" and "+ full stress" buttons
  - "Live" checkbox
  - Pinned metrics section (highlighted)
  - All metrics list
- **`MetricRow`** (L292): One metric line: pin toggle + label + monospace value.

All rendering is pure Dioxus `rsx!` elements (divs, spans, buttons). No SVG, no canvas, no charting library.

### Insertion points

#### Convergence plot

The metrics panel currently has no chart/graph rendering. A convergence plot would be the panel's first visual chart.

**Option A — Below the metrics list (after L284)**:
Add a new section after the "All metrics" block with a canvas or SVG element. This is where the convergence plot naturally fits — it's a quality-over-time visualization complementing the point-in-time metric rows.

**Option B — New section between "Live" checkbox and the metrics (after L261)**:
Insert a `<canvas>` element that renders the convergence plot (energy decrease over iterations). This keeps the chart visible even when metrics aren't computed.

**Canvas approach**: Use a `<canvas>` with WebGL or 2D context, similar to how `graph_canvas` works. The data would come from a new buffer pushed by the layout worker or polled from the renderer's host.

#### AdaptiveSpeed display

**After L247** (after the "+ full stress" button), insert a readout row:
```rust
div { class: "metrics-adaptive",
    span { class: "metric-label", "Adaptive speed" }
    span { class: "metric-value", "{speed_multiplier:.2}×" }
}
```

Or add a new `MetricKind::AdaptiveSpeed` variant to the existing enum and let it render as a standard `MetricRow`.

**Blockers**:
1. The convergence data stream needs a source: either the local layout host (`crates/graph-layouts`) or the remote worker. This is a new data channel not yet wired.
2. The chart rendering infrastructure (canvas/SVG helpers) doesn't exist yet. The existing `graph_canvas.rs` uses wgpu — a simpler 2D canvas may be more appropriate here.

---

## 3. Settings Panel / Layout (`app/ui/src/panels/settings.rs` + `app/ui/src/panels/layout.rs`)

### Settings panel (`settings.rs`)

**Size**: 320 lines, 10,894 bytes

#### Tabs
- **`SettingsTab` enum** (L22–L28): `Connection`, `Layout`, `Appearance`, `Camera`
- Persisted to localStorage at `jc_settings_tab_v1` (L18, L74–L75).
- `ACTIVE_TAB` is a `GlobalSignal<SettingsTab>` (L74).
- Tab bar renders as a horizontal `role="tablist"` with keyboard navigation (L92–L122).
- Panel body dispatches via `match active` (L311–L316).

#### Delegation pattern
Layout/Appearance/Camera are **not** embedded in settings.rs — they delegate to their own panel modules:
```rust
SettingsTab::Layout => rsx! { LayoutSettings { ctx } },    // → super::layout::panel(ctx)
SettingsTab::Appearance => rsx! { AppearanceSettings { ctx } }, // → super::style::panel(ctx)
SettingsTab::Camera => rsx! { CameraSettings { ctx } },        // → super::camera::panel(ctx)
```

### Layout panel (`app/ui/src/panels/layout.rs`)

**Size**: 4,219 lines — the largest panel by far.

#### Key structures
- **`SeedStrategy` enum** (L66)
- **`PanelState`** (L76): Active engine, settings, compute URL
- **`ComputeHealth`** (L176): Worker health
- **`ComputeSession`** (L210): GPU session state
- **`LayoutPreset` enum** (L1057)
- **`Backend` enum** (L2170): `Local`, `Cluster`

#### Main panel function (L2461)
The `panel()` function renders:
1. Cross-backend banner (L2605–L2619): when viewing a different backend than running
2. Gallery view (L2621–L2632): local or cluster gallery cards
3. Worker health/status (L2637–L2649)
4. Worker connection UI (L2663–L2668): URL + Reconnect
5. Seed section (L2673)
6. Engine parameters (L2694)
7. Solve/Pause actions (L2696–L2700+)

### Insertion point for "Regime" dropdown

The Regime dropdown selects between energy-minimization regimes. It belongs in the engine parameters section.

**Location**: Inside `engine_params()` (L3367), or as a new standalone section before/after the Parameters block (around L2680–L2693).

**More precisely**: The `engine_params()` function dispatches to per-engine UI functions like `remote_fa2_ui()`, `geometric_ui()`, `fcose_ui()`, etc. The Regime dropdown should appear for every physics engine that supports multiple regimes. Add it:

1. **To `engine_params()` (L3367)**: Before the engine-specific UI, as a shared control for all physics engines.
2. **In the Parameters header area (L2680–L2693)**: As a dropdown alongside the engine name and reset button.

```rust
// Insert after L2682, before the reset button:
select_row("Regime",
    vec!["Standard", "Energetic", "Water"],
    current_regime_index,
    move |i| { set_regime(i); })
```

**Prerequisites**:
- Add `Regime` variant to the layout state (likely `PanelState` or a new field).
- Wire the regime selection through to `apply_engine()`.
- Persist regime in `PanelState` → localStorage.

---

## 4. Style Panel (`app/ui/src/panels/style.rs`)

**Size**: 1,532 lines, 55,888 bytes

### Current structure

#### Coloring modes — `ColorBy` enum (L79–L89)
```rust
enum ColorBy {
    Community,  // default
    Folder,
    Recency,
    Doctype,
    Tag,        // primary-tag hash from importer facet index
}
```

Each variant has:
- `label()` → display string
- `metric_key()` → server metric name (`"community"`, `"folder"`, etc.)

#### Other enums
- **`SizeBy`** (L42): Community, Folder, Recency, Doctype, Uniform, Tag
- **`ShapeBy`** (L124): Doctype (default), Community, Folder, Uniform
- **`EdgeColorBy`** (L158): None (uniform), Source, Target, Community, Tag
- **`CommunitySource`** (L204): Label, Louvain, Leiden
- **`PaletteId`** (L356): 10+ color palettes (Okabe-Ito, ColorBrewer Set1/Dark2, Viridis, Plasma, Schrödinger Corporate/Scientific, Monochrome, Blue, Paper)

#### State — `StyleState` (L225–L279)
Persisted struct with all visual parameters: `size_by`, `color_by`, `shape_by`, `edge_color_by`, `size_mul`, `edge_size_mul`, `log_scale_size`, `shader_intensity`, `edge_color[rgba]`, `edge_alpha_mul`, `edge_dist_min/max`, `edge_min_transparency`, `edge_fade_floor`, `edge_width`, `palette`, `community_source`, `region_mode/radius/alpha/outline/level`.

#### How colors reach the shader
1. `apply_now()` (L1081) computes per-node colors via `colors_from_metric()` (L611).
2. `colors_from_metric()` maps categorical metrics to palette colors: `palette_color(bucket_hash % palette_len, palette_id)`.
3. The resulting `Vec<[f32; 3]>` is pushed to the GPU via `render::push_colors()`.
4. Every `update()` call triggers `apply_now()` → recompute + GPU push.

#### Panel UI (L1310–L1532)
Rendered as Dioxus elements:
- Reset button
- `select_row` for: Size by, Color by, Community source, Shape by, Palette, Regions, Edge color by
- `slider_row` for: Region radius, Region fill, Node/Edge size multipliers, Shader intensity, Edge width, Edge density, Edge distance min/max, Edge min visibility, Long-distance fade floor
- Checkboxes for: Log scale, Region outline

### Insertion point for "Stability" coloring mode

**Step 1 — Add variant to `ColorBy`** (after L88, before closing `}`):
```rust
Stability,
```

**Step 2 — Add to `ColorBy::ALL`** (L92–L98):
```rust
ColorBy::Stability,
```

**Step 3 — Add `label()`** (in the match, ~L105):
```rust
ColorBy::Stability => "Stability",
```

**Step 4 — Add `metric_key()`** (in the match, ~L114):
```rust
ColorBy::Stability => "stability",
```

**Step 5 — Wire the metric fetch**. The `ensure_metrics()` function (L827) already fetches server metrics by key. Adding `"stability"` to the keys requested when `color_by == ColorBy::Stability` will fetch it automatically.

**Step 6 — Color mapping**. `colors_from_metric()` (L611) uses the existing `palette_color(bucket, palette)` path for categorical metrics. If stability is continuous (float energy values), a new continuous color mapping would be needed — likely interpolating through a gradient (e.g., blue=stable → red=unstable) rather than bucketing. This would be a new code path in `colors_from_metric()`.

**Blockers**:
1. The stability metric must exist on the backend as a per-node float vector served at `/graph/metrics/stability`.
2. Continuous color mapping doesn't exist yet — the current code assumes categorical bucketing. A gradient interpolation (like Viridis) would need to be added to `colors_from_metric()`.

---

## 5. Timeline Panel (`app/ui/src/panels/timeline.rs`)

**Size**: 768 lines, 27,936 bytes

### What exists

The timeline is a **position-history scrubber** for the force-sim animation. It captures live position frames into a compressed ring buffer and lets the user pause/scrub/step through history.

#### Core data structures
- **`Frame` enum** (L48): `Key(Vec<f32>)` | `Delta(Vec<f32>)` — keyframe+delta compression
- **`FrameRing`** (L67): Bounded ring buffer with keyframe cadence (every 8 frames), component-count change detection, depth resizing
- **`ScrubState` enum** (L228): `Live` | `Paused { idx: usize }`
- **`PersistedKnobs`** (L242): `depth` (30–1000), `stride` (1–30)

#### Global state (L298–L317)
- `DEPTH`, `STRIDE`, `SCRUB`, `BUFFERED_LEN`, `BUFFERED_BYTES` — all `GlobalSignal`
- `RING: RefCell<FrameRing>` — not signal-based (too large)
- `TICKER_ONCE: Cell<bool>` — ensures single ticker
- `PUSH_SEEK: Cell<bool>` — one-shot seek flag

#### How it works
1. `ensure_ticker()` (L362) starts a single `spawn_local` loop on ~16ms cadence.
2. `tick()` (L397) reads live positions from `render::POSITIONS`, pushes into the ring, and writes the current frame to GPU when live.
3. Pause/resume/step controls manipulate `ScrubState`.
4. `push_positions_to_gpu()` (L474) writes the scrubbed frame into the GPU position buffer.

#### UI (L493–L672)
- Transport controls: Play/Pause, Step −, Step +
- Scrub slider: range input over frame indices
- Readout: frame N/M, buffer stats
- Knobs: Depth slider (30–1000), Stride slider (1–30)

### What's NOT implemented (PARITY GAP noted at L369)
> "PARITY GAP: capture cadence is a ~16ms timer, not the renderer's frame cadence"

This is noted but functional — the timer approximates the old egui per-frame tick.

### What would change for convergence visualization

The timeline currently stores **only positions** `[x,y,z, ...]`. To visualize convergence, it needs to also store **per-frame metrics** (energy, stress, etc.).

#### Changes needed:

1. **Extend `FrameRing` or add a parallel ring** (L67):
   Add a `Vec<f32>` holding per-frame scalar metrics, indexed the same way as positions:
   ```rust
   struct FrameRing {
       // existing fields...
       metrics: Vec<Vec<f32>>,  // per-frame metric vectors (energy, stress, ...)
   }
   ```

2. **Capture metrics in `tick()`** (L397):
   Read current energy/stress from the layout host and push alongside positions.

3. **Add a convergence plot section** to `panel()` (after L617):
   A `<canvas>` rendering a line chart of energy vs. frame index, with the current scrub position highlighted.

4. **Insert at L620**:
   ```rust
   hr {}
   {convergence_plot(idx, max_idx)}  // new section
   {knobs()}
   ```

5. **New function `convergence_plot(current: usize, max: usize) -> Element`**:
   Renders a 2D canvas showing energy decrease over the buffered frames, with a vertical marker at the current scrub position.

**Blockers**:
1. The layout host must expose per-frame energy values. This is a new data channel — neither the local `graph-layouts` nor the remote worker currently streams scalar metrics alongside positions.
2. Canvas rendering code for the plot is new infrastructure.

---

## 6. Main Panel Registration (`app/ui/src/main.rs`)

**Size**: 2,163 lines, 87,603 bytes

### Panel enum (L172–L196)
```rust
pub(crate) enum Panel {
    Graph, Nodes, Inspector, Document, Progress, Settings, Help,
    // egui-tray parity panels:
    Filter, Metrics, Instances, Generate, Timeline, Debug,
    // Sessions view:
    Worlds, History, Branches, Merge, GitHub, GpuSessions,
    // Runtime importer package workbench:
    Importers,
}
```

### PanelKind impl (L198–L223)
Each variant maps to a `title()` string. This is the human-readable name shown in panel tabs and the command palette.

### Registration points for a new EdgeInspector panel

**Step 1 — Add variant to `Panel` enum** (after L195, before `}`):
```rust
EdgeInspector,
```

**Step 2 — Add title** (L198–L222, after `Importers` entry):
```rust
Panel::EdgeInspector => "Edge Inspector",
```

**Step 3 — Create panel module** at `app/ui/src/panels/edge_inspector.rs` with:
```rust
pub fn panel(ctx: Ctx) -> Element { ... }
```

**Step 4 — Declare module** in `app/ui/src/panels/mod.rs` (L25 — add after the `pub mod inspector;` line):
```rust
pub mod edge_inspector;
```

**Step 5 — Add dispatch in `panel_body()`** (after L1844):
```rust
Panel::EdgeInspector => panels::edge_inspector::panel(ctx),
```

**That's it.** The panel-kit workspace automatically picks up new `Panel` variants from the `PanelKind` impl. No additional registration beyond the enum variant + title + dispatch.

**Note on localStorage**: The workspace layout key is `jc_layout_v9` (L225). Adding a new panel variant is backward-compatible — unknown variants in stored layouts will be dropped on deserialization.

---

## Summary of Blockers

| Feature | Blocker | Severity |
|---|---|---|
| Inspector energy/stability rows | `NodeMeta` proto needs new fields; backend must serve them | Medium — requires proto regen + backend work |
| Metrics convergence plot | New canvas rendering infrastructure; data channel from layout host | High — new rendering code + data plumbing |
| Metrics AdaptiveSpeed | Needs a data source (layout host scalar) | Low — can be a computed ratio from existing metrics |
| Settings Regime dropdown | Regime concept needs backend support in layout engines | High — new layout engine feature |
| Style Stability coloring | Backend `/graph/metrics/stability` endpoint; continuous color mapping | High — new backend endpoint + new shader coloring path |
| Timeline convergence viz | Per-frame metric capture in ring buffer; canvas rendering | High — ring buffer extension + new rendering |
| EdgeInspector panel | Low — just panel boilerplate; content is the real work | Low — registration is trivial |

## File Summary

| File | Lines | Bytes | Key insertion points |
|---|---|---|---|
| `app/ui/src/panels/inspector.rs` | 804 | 31,976 | L699–L700 (metric grid), L686–L689 (identity rows) |
| `app/ui/src/panels/metrics.rs` | 323 | 11,785 | L247 (AdaptiveSpeed), L284 (convergence plot section) |
| `app/ui/src/panels/settings.rs` | 320 | 10,894 | L27 (tab enum), L314 (dispatch) |
| `app/ui/src/panels/layout.rs` | 4,219 | ~160KB | L2680–L2693 (Parameters header), L3367 (engine_params) |
| `app/ui/src/panels/style.rs` | 1,532 | 55,888 | L79–L89 (ColorBy enum), L92 (ALL), L100 (label), L108 (metric_key) |
| `app/ui/src/panels/timeline.rs` | 768 | 27,936 | L397 (tick capture), L620 (convergence section), L67 (FrameRing) |
| `app/ui/src/main.rs` | 2,163 | 87,603 | L172 (Panel enum), L198 (PanelKind impl), L1734 (panel_body dispatch) |
