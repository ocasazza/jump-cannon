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

## Phase 1 — the real renderer ✅

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

## Phase 2 — layout system ✅

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

## Phase 3 — settings panels ✅

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

## Phase 4 — interaction surfaces ✅

Executed 2026-06-10 (five parallel ports against the egui reference at
723af10):

- Command palette (`app/ui/src/palette.rs`): ActionRegistry + all builtins,
  Ctrl/Cmd+P, fuzzy matching with title/path highlighting, param forms,
  two-pane node preview, category grouping. Jump-to-section actions open
  panels through `main.rs::OPEN_PANEL` → `panel_kit::Workspace::restore`.
- Anchored hover/click cards (`app/ui/src/anchored.rs`): 50 ms raycast
  throttle, 700 ms preview arm, sticky-beats-hover, EMA(0.4) placement,
  reserved-size edge clamping, tether line + off-screen arrow, promoted
  click card with fly-to (`render::look_at_node`). Focus sets
  (`FocusMode` × 5) push GPU dim masks; the Camera panel's picker drives
  them.
- Inspector parity (`panels/inspector.rs`): active-filter strip, empty-state
  tag browser, badge rows via `badges.rs`, frontmatter leftover grid,
  neighbour pills (+ in/out direction), community fold rules.
- Document/page viewer (`panels/document.rs`): per-node edit buffers,
  dirty/save/retry semantics, Rendered/Source tabs, Cmd+F find strip,
  Tab soft-indent, line gutter, frontmatter chip strip.
- `AppState` round-trip (`app/ui/src/appstate.rs`): egui-shaped struct over
  the `jc_*` keys, YAML/JSON export-import, share codec (JSON → DEFLATE →
  base64url, `#s=`), sessionStorage snapshot ring (cap 50, 250 ms ticker),
  `?config=<name>` boot presets, `note_mutation`/`note_source` attribution
  feeding the Debug event log.
- Client-side tvix eval (`panels/{layout,generate}.rs`): built-in + custom
  Nix seed expressions via `tvix_wasm::eval_seed`; Generate Inline executor
  via `eval_graph`; LocalWorker executor restored 2026-06-12 — the
  `tvix-worker` bin is back as an app workspace member (`app/tvix-worker`),
  trunk builds it as a worker bundle, and `ui/src/worker.rs` spawns it (Blob
  bootstrap + READY handshake). Auto's offline fallback is the worker again
  (server → worker, matching the egui wasm `resolve_generate_backend`).

Remaining `PARITY GAP` annotations (grep `app/ui/src` for the full list):
new-graph-tab (single Graph panel by design), client-side degree/wcc buffers
for generated graphs, per-stage perf overlay, syntect source highlighting,
edge-hover width from Style state, soft-tether card drag offsets.

## Phase 5 — retirement ✅

Executed 2026-06-09. Removed: `crates/graph-renderer` (the egui frontend),
its orphan dependencies `crates/jump-io` (input layer) and
`crates/tvix-worker` (LocalWorker bundle), the root `Trunk.toml`, the legacy
Playwright suite (`tests/browser/`), the `graph-renderer-web` nix derivation
and bevy system libs, and the `just wasm` / `watch-wasm` recipes. `just
dev-up` and the compose stack now serve the Dioxus app (`app-web`)
unconditionally; graph-api's embedded-asset fallback is gone (assets always
come from `--assets-dir` / `JUMP_CANNON_ASSETS_DIR`, renamed from
`GRAPH_RENDERER_ASSETS_DIR`); browser regression coverage is
`crates/test-browser`, which waits for the app's `[jump-cannon-ui] boot`
console marker. The egui implementation lives in git history.

## Non-goals

The compute layer (graph-compute, broker orchestration, Sky-Pilot) stays
behind graph-api — the app consumes its HTTP/streaming interfaces only.
