# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Copilot, Cursor, etc.) when working with code in this repository.

## Hard repo rule: Rust everywhere

Every surface is Rust. The backend is **Rust + axum**. There are two frontend stacks during the Dioxus migration (see "Dioxus + Tauri app" below):

- `app/` — **Rust + Dioxus** compiled to WASM, shipped in a **Tauri v2** shell. This is the successor full-stack frontend.
- `crates/graph-renderer` — the original **Rust + wgpu + egui** WASM frontend; stays until the wgpu graph canvas is ported into the Dioxus app.

No hand-written JavaScript, no TypeScript, no React/Vue, no JS bundlers. The single allowed JS shim is `crates/graph-renderer/assets/main.js`, which **must stay under 50 lines** and do exactly one thing: load the WASM module and hand control to Rust. No DOM manipulation, no event handlers, no fetch helpers, no protobuf decoding, no UI state.

If you're tempted to "just add a quick JS function" — stop. Add it in Rust. The egui surface in `graph-renderer/src/ui/` is where UI goes; `graph-renderer/src/web.rs` is where browser-side event hooks live.

The test harness under `tests/browser/*.mjs` and `crates/test-browser/` is the only exception, and only because it drives a real browser from outside the WASM bundle.

## Crate map

| Crate | Role |
|---|---|
| `crates/graph-api` | axum HTTP server. Loads the vault, serves `/graph/*`, `/node/*id`, `/search`, `/vault/page` (editor PUT), `/progress`, etc. Watches `$VAULT_ROOT` via `notify` and atomically swaps the in-memory `GraphSnapshot` through `arc-swap`. |
| `crates/graph-renderer` | Rust + wgpu + egui via eframe. Native binary + WASM (trunk-built). **All UI lives here**: the floating panel system (`ui/floating.rs`), inspector, editable page viewer (`ui/page_viewer.rs`), anchored hover/click cards (`ui/anchored.rs`), Instances panel with YAML export/import + state-snapshot timeline. |
| `crates/graph-layouts` | wgpu compute force-sim. Native + WASM. |
| `crates/graph-compute` | Optional standalone layout solver, gRPC on `[::1]:50051`. Opt-in via `--compute-url` / `GRAPH_API_COMPUTE_URL` — unset means the broker is never dialed. Deployable as the docker-compose service or via Sky-Pilot (`infra/sky/`). |
| `crates/graph-metrics` | PageRank, Louvain, k-core, betweenness, weakly-connected-components. Stateless functions over `vault-data::Graph`. |
| `crates/vault-data` | Shared domain types: `Node`, `Edge`, `Graph`, `FieldSchema`, the categorical color palette. Every other vault crate depends on it; no I/O. |
| `crates/vault-links` | Wikilink extractor. Walks an Obsidian vault on disk, parses markdown + frontmatter, and produces a `VaultGraph`. The startup loader inside graph-api lives here. |
| `crates/vault-search` | Standalone HTTP service. Tantivy-backed full-text + field search over the vault. Spawned by graph-api as a subprocess; respawned with `--rebuild` on every watcher-driven reload. |
| `crates/jump-io` | Platform-agnostic input layer: semantic actions, rebindable triggers, per-device sensitivity. Decouples "what should happen" (e.g. `Pan`, `Zoom`, `Select`) from raw mouse/keyboard/touch events. Consumed by graph-renderer. |
| `crates/tvix-wasm` | `tvix-eval` bridge — native + WASM Nix expression evaluator. Enables Nix expressions in the UI/data pipeline without shelling out. |
| `crates/test-browser` | Rust-only Chromium driver (chromiumoxide) for the foundational browser regression suite. Spawned by `just test browser-rust` / `nix run .#test-browser-rust`. |

## Dioxus + Tauri app (`app/`)

The full-stack frontend migration target, modeled 1:1 on `apple-notes-ocr-flow`. A **separate Cargo workspace** (`app/Cargo.toml`) so the main nix + crane workspace never absorbs the Tauri/Dioxus dependency tree:

| Crate | Role |
|---|---|
| [`panel-kit`](https://github.com/ocasazza/panel-kit) (external) | **Generic, app-agnostic** panel-workspace library: floating/tiling panels, macOS traffic lights, drag/resize, tiling drag-reorder, minimize-to-dock, localStorage layout persistence, base CSS theme (`panel_kit::CSS`). Lives in its own repo and is consumed as a git dependency by both this app and apple-notes-ocr-flow, so the two share one component/styling library. Apps implement `panel_kit::PanelKind` on an enum and call `use_workspace` + `ws.render(body_fn)`. Local development: `[patch."https://github.com/ocasazza/panel-kit"]` in `app/Cargo.toml`. |
| `app/ui` | jump-cannon's Dioxus 0.6 frontend (trunk-built WASM, port 8081; `app/Trunk.toml` at the workspace root drives both dev and nix builds). Panels: Graph (Canvas2D pan/zoom/click-select view of the vault graph), Nodes, Search, Inspector, Document (editor → `PUT /vault/page`), Progress (polls `/progress`), Settings, Help. Talks to graph-api with the same three wire formats the egui renderer uses: JSON, protobuf, and raw LE f32/u32 buffers. The prost types are **checked in** (`app/ui/src/proto/`, regen via `just app-proto`) so the workspace is self-contained — no protoc, no build.rs, no reach outside `app/`. |
| `app/src-tauri` | Tauri v2 shell. Pure webview container — **no IPC commands**; the frontend reaches graph-api over HTTP (`tauri-plugin-http` allows LAN/Tailscale hosts). Lib+main split for iOS/Android entrypoints. |

Workflow: `just dev-up` (backend) + `just app-dev` (desktop app, hot-reload). `just app-check` type-checks both targets; `just app-build` makes release bundles. Default server URL is `http://127.0.0.1:8765` (the compose port), changeable in the Settings panel and persisted to localStorage.

Nix integration: `nix build .#app-web` builds the frontend dist through crane + trunk (same machinery as `graph-renderer-web`; it's also a flake check, so `nix flake check` gates it). `wasm-bindgen` is pinned to `=0.2.118` in `app/Cargo.toml` to match the nixpkgs CLI exactly. The Tauri shell itself stays a devshell build — bundling needs platform signing toolchains nix can't usefully sandbox on macOS.

Scope rule: the compute layer (`graph-compute`, gRPC broker, Sky-Pilot orchestration) is **not** part of this app — it stays behind graph-api's interfaces.

**Parity contract:** the migration target is *identical features* — same wgpu renderer, same layout engines, same settings — with only the UI framework changing. The phase plan and full feature inventory live in [`docs/dioxus-migration.md`](docs/dioxus-migration.md); don't mark a feature migrated unless its behavior matches `crates/graph-renderer`.

## Data flow

```
$VAULT_ROOT (.md files)
       │
       ├──► vault-links ──► vault-data::Graph ──► graph-api (in-memory ArcSwap<GraphSnapshot>)
       │                                                  │
       │                                                  ├──► graph-metrics (computed once + on reload)
       │                                                  │
       │                                                  ├──► HTTP /graph/* /node/*id /vault/page /progress
       │                                                  │           │
       │                                                  │           ▼
       │                                                  │     graph-renderer (WASM in browser, or native)
       │                                                  │           │
       │                                                  │           └──► graph-layouts (wgpu compute, in-process)
       │                                                  │
       │                                                  └──► graph-compute (optional gRPC, out-of-process layout)
       │
       └──► vault-search subprocess ──► tantivy index ──► HTTP /search

graph-api notify watcher ──► debounce 400ms ──► rebuild Graph + respawn vault-search ──► emit /progress events
```

`jump-io` sits inside graph-renderer between raw egui input events and the semantic actions the UI consumes. `tvix-wasm` is the cross-target Nix evaluator (not yet on the hot path; available for future config surfaces).

## Build & development

The whole repo builds through **nix + crane + trunk**. No `npm install`, no `wasm-pack`, no yarn lockfile.

- `nix build` — deployable bundle (server binary + WASM).
- `just dev-up` — full dev stack: graph-api with file watcher, graph-compute optional, hot-reload via trunk watch.
- `just dev-down` — symmetric teardown.
- `just wasm` — `trunk build --release`. Convenience for fast frontend iteration (pair with `ASSETS_DIR=$PWD/crates/graph-renderer/assets/dist just dev-up`). `dev-up` itself no longer requires it — the compose stack defaults to the nix-built `graph-renderer-web` derivation.
- `just watch-wasm` — `trunk watch` (rebuilds WASM on change; pair with `just dev-up`).
- `just run` — production binary, embedded assets, no watch.
- `just kill` — purge stray graph-api / vault-search processes from prior runs.

`just dev-up`, `just wasm`, and the test recipes are convenience wrappers around the nix outputs — never standalone command stacks.

## Testing

- `just test all` (or `just test`) — workspace `cargo test`.
- `just test browser` — legacy Playwright/JS suite. Launches headless Chromium with WebGPU, captures screenshot + console log, asserts the canvas isn't black + boot logs fire. Tolerated under the Rust-only rule because the harness runs *outside* the WASM bundle.
- `just test browser-rust` — new Rust-driven suite via `crates/test-browser` (chromiumoxide + Nix-provided Chromium). Minimal asserts for now; foundation to grow on once the frontend stabilizes.
- `cargo test -p <crate>` for single-crate runs; standard `cargo test -- <filter>` for one test.

Run `just test browser` (or `browser-rust`) before claiming any visual change works. Don't commit visual changes without `ok: true`.

## Wire format

- **Bulk numeric** (positions, edges, metrics): raw little-endian `f32` / `u32` buffers.
- **Structured** (init, `NodeMeta`, search results): protobuf via `prost` on both the server and the WASM client. Don't bring `protobufjs` into the JS shim.

## Configuration (durable)

| Var / flag | Read by | Default |
|---|---|---|
| `VAULT_ROOT` env / `--vault-root` flag | graph-api | `$PWD` (CWD fallback) |
| `GRAPH_API_PORT` env / `--port` flag | graph-api | `0` (OS-assigned ephemeral) |
| `GRAPH_API_HOST` env / `--host` flag | graph-api | `127.0.0.1` (container override: `0.0.0.0`) |
| `GRAPH_API_COMPUTE_URL` env / `--compute-url` flag | graph-api | unset → broker disabled, `/graph/layout/stream` returns 503 |
| `GRAPH_API_NO_WATCH=1` | graph-api | unset → file watcher armed |

`.env` at the repo root is auto-loaded by the justfile (`set dotenv-load := true`).

## Frontend state architecture

`AppState` (`crates/graph-renderer/src/ui/state.rs`) is the single source of truth for UI/UX state and is fully `Serialize + Deserialize` (serde JSON + YAML round-trips). Persistence is **per-session via `sessionStorage` on WASM** and `eframe::Storage` on native — see `ui/persist.rs`. A new tab starts fresh; a tab reload preserves layout, panel positions, filters, selections.

Three load-bearing UI patterns:

- **Floating panels.** Every panel (sections, inspector, filter strip, canvas pop-out) goes through `ui/floating.rs::FloatingPanel`. Visibility is `&mut bool`; X closes; egui memory persists position per `PanelId` variant. The tray strip (`ui/status_footer.rs::show_tray`) is the Windows-style launcher row. Panel toggles go left, view controls go right.
- **Anchored cards.** `ui/anchored.rs::AnchoredPanel` projects a world-space anchor through the same `proj * view` the inspector leader-line uses, with screen-pos EMA smoothing, off-screen-edge clamping, and soft-tether drag. Powers the hover preview and the click-promoted node card.
- **State timeline.** Every UI mutation auto-snaps into `AppState.snapshots` (250 ms debounce, hash-based diff). The Instances panel renders the timeline with per-entry Restore. `AppState::default()` is the first entry; command palette executions are labeled `palette: <action.title>`; section/slider mutations fall to `misc` for now.

## Backend state architecture

`graph-api` holds the in-memory `GraphSnapshot` (graph + id maps + binary cache) inside `arc-swap::ArcSwap`. Handlers grab one snapshot per request and the watcher's reload can't invalidate an in-flight read. The watcher (`crates/graph-api/src/watcher.rs`) debounces `.md` changes at 400 ms; a save burst coalesces into one reload. `vault-search` is currently respawned with `--rebuild` on every reload — incremental refresh is a known follow-up.

Progress for each reload stage emits to `crates/graph-api/src/progress.rs` and surfaces to the frontend via `GET /progress?since=<seq>`. The renderer polls every 250 ms while events flow, backs off to 2 s when idle. The footer's task list renders the stream automatically.

## Coding constraints

- No JS in `assets/`, no JS bundlers (vite, esbuild, webpack), no `protobufjs`, no three.js, no Cosmograph, no OrbitControls. wgpu + egui only.
- No CSS files. egui owns all styling in Rust (`ui/theme.rs`).
- No new logic in `tests/browser/*.js` beyond the test harness itself.
- Match the existing palette/font: `palette::*` constants from `theme.rs`, Courier Prime monospace, squircle-backed floating panels.

## Session completion (mandatory)

Work is **not complete until `git push` succeeds**. Before ending a session:

1. Record remaining work for the next session (in the relevant `docs/*.md` plan, or the handoff note below).
2. Run quality gates (`cargo check --workspace --tests`, `just test browser` if visual).
3. Push:
   ```bash
   git pull --rebase
   git push
   git status   # MUST show "up to date with origin"
   ```
4. Clean up stashes; prune remote branches.
5. Hand off context for the next session.

If `git push` fails, resolve and retry. Never stop before pushing.

## Non-interactive shell

Shell builtins may be aliased to `-i` mode and hang on confirmation prompts. Always use the non-interactive forms:

```bash
cp -f source dest          # NOT: cp source dest
mv -f source dest
rm -f file
rm -rf directory
cp -rf source dest
```

Other tools: `scp` / `ssh` need `-o BatchMode=yes`; `apt-get` needs `-y`; `brew` needs `HOMEBREW_NO_AUTO_UPDATE=1`.

## Recent UI fixes (2026-05-31)

### CollapsingHeader ID conflicts
Fixed egui v0.27.0+ bug where duplicate `CollapsingHeader` names caused ID collisions (only bottom-most section clickable). All inspector sections (`Frontmatter`, `Community`, `Neighbours`) now have unique `.id_salt()` IDs.

**Files**: `crates/graph-renderer/src/ui/inspector.rs` lines 622, 788, 825, 854

### Filter panel UX consistency
Removed auto-hide logic from floating filter strip so it behaves like other panels (Style, Layout, Camera). Filter panel now:
- Shows "no active filters" when empty (doesn't disappear)
- Starts closed by default (`filter_strip_open: false`)
- Consistent UX across floating and tiled modes

**Files**: `crates/graph-renderer/src/ui/filter_strip.rs` line 56, `crates/graph-renderer/src/ui/state.rs` line 1122

### High-contrast theme improvements
Reworked GUI control layout and styling for minimal high-contrast theme:

**Tab bar styling** (`app.rs` line 1300):
- Black background instead of grey (`palette::BLACK`)
- Border color matches theme (`palette::BORDER`)

**Control spacing & readability** (`widgets.rs`):
- `row()`: Labels now use `BODY` font size (was `SMALL`) at full `TEXT` brightness (was dim 0.6 alpha)
- Added `ITEM_GAP` (4px) spacing between controls to prevent cramped layout
- Increased slider width reserve from 48px → 56px to prevent value text clipping
- `subgroup_label()`: Uses `GREY` color (more contrast) instead of 0.6 alpha, adds spacing

**Result**: Controls are no longer cramped, labels are readable, titles don't get cut off, clear visual separation between GUI elements.

### Known egui limitations
- **SidePanel auto-shrink glitch**: When `CollapsingHeader` toggles inside `SidePanel`, panel may visually jump (egui issue #1262, open since 2022, no fix)
- **Single-pass frame delay**: Centered windows may be invisible on first frame or shift when content changes (architectural limitation)
- **Panel ordering rule**: `CentralPanel` must ALWAYS be added last after all other panels to avoid layout hierarchy bugs
- **Expand to tile**: Anchored panel expand button currently just enlarges the floating panel (can go off-screen). Should transition to tiled workspace instead (Niri-style) - tracked in `anchored.rs` TODO
