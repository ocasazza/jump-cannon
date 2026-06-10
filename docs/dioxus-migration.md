# Dioxus migration — feature-parity contract

The requirement (2026-06-09): **keep the features exactly the same; only the
UI/UX framework changes** (egui → Dioxus, shipped in a Tauri shell). This doc
is the parity inventory and the phase plan. A feature is "migrated" when its
behavior in `app/` matches `crates/graph-renderer` — same renderer, same
engines, same settings, same wire traffic — not when a lookalike exists.

Status legend: ✅ done · 🚧 in progress · ⬜ not started

## Phase 0 — shell + plumbing ✅

Workspace shell (panel-kit: floating/tiling, traffic lights, dock, layout
persistence), typed graph-api client (JSON + protobuf + binary buffers),
Nodes/Search/Inspector/Document/Progress/Settings/Help panel skeletons,
Tauri shell, nix + crane build (`nix build .#app-web`), compose/dev-up
frontend selection (`just dev-up gpu app`, `FRONTEND=app nix run .#dev-up`).

## Phase 1 — the real renderer 🚧

Port the wgpu pipeline from `crates/graph-renderer` into `app/ui/src/render/`,
replacing the interim Canvas2D view (which was a placeholder, not the target):

- `graph_pipelines.rs` — node/edge pipelines, storage buffers, camera/effects
  uniforms, GpuForceLayout compute binding, positions-readback state machine.
  The egui_wgpu CallbackTrait boundary (prepare/paint) becomes a self-owned
  Surface on the app's canvas + requestAnimationFrame loop.
- `camera.rs` — the 6DoF perspective camera, verbatim. WASD pans, mouse-drag
  rotates pitch+yaw, scroll zooms along forward, QE ascends/descends.
- `shaders/node.wgsl`, `shaders/edge.wgsl` — verbatim.
- `graph-layouts` becomes a path dep of `app/ui` (in-process GPU force layout
  bound to the same positions buffer). The flake's `appSrc` fileset widens to
  include the path-dep crates.
- Same node color/size derivation as `data.rs` (community → palette, metric
  sizing); same pick math (proj*view projection, nearest-on-screen).

## Phase 2 — layout system ⬜

Source: `ui/layout/registry.rs` + `ui/layout/algorithms/*`, panel:
`ui/sections/layout.rs` (grouped Engine picker — see memory/layout-engine-picker).

- Local CPU engines: random, circle, grid, hilbert, concentric, sphere,
  spectral, klay, dagre, fcose, cose_bilkent.
- Local GPU engine: gpu_force (graph-layouts, in-process).
- Geometric engine bridge (`algorithms/geometric.rs`).
- Remote engines: remote_fa2 via the compute broker — `/compute/engines`,
  `PUT /compute/layout`, `/graph/layout/stream`; initial engine from
  `JUMP_CANNON_COMPUTE_LAYOUT_ID`, switchable live.
- Per-engine parameter UIs, run/pause/reset, engine grouping identical to the
  egui picker.

## Phase 3 — settings panels ⬜

Exact ports of `ui/sections/`:

- `camera.rs` (105 l) — camera controls/presets, reset, fit.
- `style.rs` (145 l) — node size/edge opacity/color-by-metric, palette,
  background, label thresholds.
- `filter.rs` (237 l) + `filter_strip.rs` — field/value filters over the
  `/graph/meta_summary` inverted index (proto already in the app), filter
  chips, intersection semantics.
- `metrics.rs` (94 l) — metric selection + pinning (memory/layout-metrics-home).
- `seed.rs`, `debug.rs`, `generate.rs` (tvix-expr Generate panel + worker),
  `timeline.rs` (AppState snapshots, 250 ms debounce, restore).

## Phase 4 — interaction surfaces ⬜

- Command palette (`ui/command_palette/`, actions/builtins).
- Anchored hover/click cards (`ui/anchored.rs`) — world-space anchor projected
  through the same proj*view, EMA smoothing, edge clamping.
- Inspector parity (`ui/inspector.rs` — sections, neighbours, community).
- Page viewer editing semantics (`ui/page_viewer.rs`), frontmatter chip grid
  (`ui/frontmatter_grid.rs`), document viewer, focus sets, query/field index,
  modal/badges, status footer task list.
- `AppState` (de)serialization parity: YAML/JSON round-trip, sessionStorage
  persistence, instances import/export, `?config=<name>` presets.

## Phase 5 — retirement ⬜

When phases 1–4 hold: retire `crates/graph-renderer`, the root Trunk.toml,
the Playwright harness's egui assumptions; point `just dev-up`'s default
frontend at `app`; move browser regression coverage to the Dioxus app.

## Non-goals

The compute layer (graph-compute, broker orchestration, Sky-Pilot) stays
behind graph-api — the app consumes its HTTP/streaming interfaces only.
