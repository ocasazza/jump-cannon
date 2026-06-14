# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Copilot, Cursor, etc.) when working with code in this repository.

## Hard repo rule: Rust everywhere

Every surface is Rust. The backend is **Rust + axum**. The frontend is `app/` — **Rust + Dioxus** compiled to WASM, shipped in a **Tauri v2** shell (see "Dioxus + Tauri app" below). The original Rust + wgpu + egui frontend (`crates/graph-renderer`) was retired once its features were ported; it lives in git history.

No hand-written JavaScript, no TypeScript, no React/Vue, no JS bundlers. If you're tempted to "just add a quick JS function" — stop. Add it in Rust (the Dioxus panels in `app/ui/src/` are where UI goes).

The test harness in `crates/test-browser/` is the only exception, and only because it drives a real browser from outside the WASM bundle (and even that is Rust via chromiumoxide).

## Crate map

| Crate | Role |
|---|---|
| `crates/graph-api` | axum HTTP server. Loads the vault, serves `/graph/*`, `/node/*id`, `/search`, `/vault/page` (editor PUT), `/progress`, etc. Watches `$VAULT_ROOT` via `notify` and atomically swaps the in-memory `GraphSnapshot` through `arc-swap`. Serves the frontend dist from `--assets-dir` / `JUMP_CANNON_ASSETS_DIR`. |
| `crates/graph-layouts` | wgpu compute force-sim. Native + WASM. Consumed in-process by `app/ui` (path dependency). |
| `crates/graph-compute` | Optional standalone layout solver, gRPC on `[::1]:50051`. Opt-in via `--compute-url` / `JUMP_CANNON_COMPUTE_URL` — unset means the broker is never dialed. Deployable as the docker-compose service or via Sky-Pilot (`infra/sky/`). |
| `crates/graph-metrics` | PageRank, Louvain, k-core, betweenness, weakly-connected-components. Stateless functions over `vault-data::Graph`. |
| `crates/vault-data` | Shared domain types: `Node`, `Edge`, `Graph`, `FieldSchema`, the categorical color palette. Every other vault crate depends on it; no I/O. |
| `crates/vault-links` | Wikilink extractor. Walks an Obsidian vault on disk, parses markdown + frontmatter, and produces a `VaultGraph`. The startup loader inside graph-api lives here. |
| `crates/vault-search` | Standalone HTTP service. Tantivy-backed full-text + field search over the vault. Spawned by graph-api as a subprocess; respawned with `--rebuild` on every watcher-driven reload. |
| `crates/tvix-wasm` | `tvix-eval` bridge — native + WASM Nix expression evaluator. Enables Nix expressions in the UI/data pipeline without shelling out. |
| `crates/test-browser` | Rust-only Chromium driver (chromiumoxide) for the foundational browser regression suite. Spawned by `just test browser-rust` / `nix run .#test-browser-rust`. |

## Dioxus + Tauri app (`app/`)

**THE frontend**, modeled 1:1 on `snake-pit`. A **separate Cargo workspace** (`app/Cargo.toml`) so the main nix + crane workspace never absorbs the Tauri/Dioxus dependency tree:

| Crate | Role |
|---|---|
| [`panel-kit`](https://github.com/ocasazza/panel-kit) (external) | **Generic, app-agnostic** panel-workspace library: floating/tiling panels, macOS traffic lights, drag/resize, tiling drag-reorder, minimize-to-dock, localStorage layout persistence, base CSS theme (`panel_kit::CSS`). Lives in its own repo and is consumed as a git dependency by both this app and snake-pit, so the two share one component/styling library. Apps implement `panel_kit::PanelKind` on an enum and call `use_workspace` + `ws.render(body_fn)`. Local development: `[patch."https://github.com/ocasazza/panel-kit"]` in `app/Cargo.toml`. |
| `app/ui` | jump-cannon's Dioxus 0.6 frontend (trunk-built WASM, port 8081; `app/Trunk.toml` at the workspace root drives both dev and nix builds). Panels: Graph (wgpu canvas), Nodes, Inspector, Document (editor → `PUT /vault/page`), Progress (polls `/progress`), Settings, Help, plus the tray-parity panels (Layout, Style, Camera, Filter, Metrics, Instances, Generate, Timeline, Debug). Talks to graph-api with three wire formats: JSON, protobuf, and raw LE f32/u32 buffers. The prost types are **checked in** (`app/ui/src/proto/`, regen via `just app-proto`). |
| `app/src-tauri` | Tauri v2 shell. Pure webview container — **no IPC commands**; the frontend reaches graph-api over HTTP (`tauri-plugin-http` allows LAN/Tailscale hosts). Lib+main split for iOS/Android entrypoints. |

Workflow: `just dev-up` (backend) + `just app-dev` (desktop app, hot-reload). `just app-check` type-checks both targets; `just app-build` makes release bundles. Default server URL is `http://127.0.0.1:8765` (the compose port), changeable in the Settings panel and persisted to localStorage.

Nix integration: `nix build .#app-web` builds the frontend dist through crane + trunk (it's also a flake check, so `nix flake check` gates it). `wasm-bindgen` is pinned to `=0.2.118` in `app/Cargo.toml` to match the nixpkgs CLI exactly. The Tauri shell itself stays a devshell build — bundling needs platform signing toolchains nix can't usefully sandbox on macOS.

Scope rule: the compute layer (`graph-compute`, gRPC broker, Sky-Pilot orchestration) is **not** part of this app — it stays behind graph-api's interfaces.

**Parity contract:** the migration target was *identical features* to the retired egui renderer — same wgpu renderer, same layout engines, same settings. The phase plan, feature inventory, and remaining PARITY-GAP items live in [`docs/dioxus-migration.md`](docs/dioxus-migration.md); the egui reference implementation is in git history.

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
       │                                                  │     app/ui (Dioxus WASM in browser / Tauri webview)
       │                                                  │           │
       │                                                  │           └──► graph-layouts (wgpu compute, in-process)
       │                                                  │
       │                                                  └──► graph-compute (optional gRPC, out-of-process layout)
       │
       └──► vault-search subprocess ──► tantivy index ──► HTTP /search

graph-api notify watcher ──► debounce 400ms ──► rebuild Graph + respawn vault-search ──► emit /progress events
```

`tvix-wasm` is the cross-target Nix evaluator (used server-side for `POST /generate`; available for future config surfaces).

## Build & development

The whole repo builds through **nix + crane + trunk**. No `npm install`, no `wasm-pack`, no yarn lockfile.

- `nix build` — the graph-api server binary; `nix build .#app-web` — the frontend dist.
- `just dev-up` — full dev stack: graph-api with file watcher, graph-compute optional, hot-reload via `trunk watch` in `app/`. Serves the Dioxus app at `:8765`.
- `just dev-down` — symmetric teardown.
- `just app-dev` / `just app-build` — Tauri desktop shell (dev / release bundles).
- `just run` — production binary, no watch: builds the app dist (`cd app && trunk build --release`) then serves it via `--assets-dir app/ui/dist`.
- `just kill` — purge stray graph-api / vault-search processes from prior runs.

`just dev-up` and the test recipes are convenience wrappers around the nix outputs — never standalone command stacks.

`just` is structured with **modules** for subcommand grammar: `test` and `cluster` live in `just/*.just`, so `just test cargo`, `just test fuzz 5000`, `just cluster up sky` are real, completable recipes (not bash-case dispatch). `just --list` shows the top level grouped by `[group(...)]`; `just --list test` / `just --list cluster` enumerate a module's subcommands. Each module pins `set working-directory := '..'` so recipes run from the repo root.

## Testing

- `just test` / `just test all` — workspace `cargo test` + the Rust browser smoke test.
- `just test browser-rust` — Rust-driven browser suite via `crates/test-browser` (chromiumoxide + Nix-provided Chromium), driving the Dioxus app: boot log (`[jump-cannon-ui] boot`), canvas dimensions, screenshot. (The egui-era Playwright/JS suite was removed with the egui frontend.)
- `cargo test -p <crate>` for single-crate runs; standard `cargo test -- <filter>` for one test.

Run `just test browser-rust` before claiming any visual change works. Don't commit visual changes without `ok: true`.

## Wire format

- **Bulk numeric** (positions, edges, metrics): raw little-endian `f32` / `u32` buffers.
- **Structured** (init, `NodeMeta`, search results): protobuf via `prost` on both the server and the WASM client. Don't bring `protobufjs` into the JS shim.

## Configuration (durable)

| Var / flag | Read by | Default |
|---|---|---|
| `VAULT_ROOT` env / `--vault-root` flag | graph-api | `$PWD` (CWD fallback) |
| `GRAPH_API_PORT` env / `--port` flag | graph-api | `0` (OS-assigned ephemeral) |
| `GRAPH_API_HOST` env / `--host` flag | graph-api | `127.0.0.1` (container override: `0.0.0.0`) |
| `JUMP_CANNON_COMPUTE_URL` env / `--compute-url` flag | graph-api | unset → broker disabled, `/graph/layout/stream` returns 503 |
| `JUMP_CANNON_ASSETS_DIR` env / `--assets-dir` flag | graph-api | unset → assets 404 (no embedded bundle; point it at the app dist) |
| `GRAPH_API_NO_WATCH=1` | graph-api | unset → file watcher armed |

`.env` at the repo root is auto-loaded by the justfile (`set dotenv-load := true`).

## Frontend state architecture

The frontend is the Dioxus app in `app/` — panel workspace from `panel-kit` (external crate), app signals bundled in `Ctx` (`app/ui/src/main.rs`), panels in `app/ui/src/panels/`, wgpu renderer in `app/ui/src/render.rs` + `graph_canvas.rs`. Layout persistence is localStorage via `panel_kit::use_workspace`. The migration phase plan, feature inventory, and remaining PARITY-GAP notes live in [`docs/dioxus-migration.md`](docs/dioxus-migration.md). (The egui-era `AppState` architecture this replaced is in git history.)

## Backend state architecture

`graph-api` holds the in-memory `GraphSnapshot` (graph + id maps + binary cache) inside `arc-swap::ArcSwap`. Handlers grab one snapshot per request and the watcher's reload can't invalidate an in-flight read. The watcher (`crates/graph-api/src/watcher.rs`) debounces `.md` changes at 400 ms; a save burst coalesces into one reload. `vault-search` is currently respawned with `--rebuild` on every reload — incremental refresh is a known follow-up.

Progress for each reload stage emits to `crates/graph-api/src/progress.rs` and surfaces to the frontend via `GET /progress?since=<seq>`. The app's Progress panel polls and renders the stream automatically.

## Coding constraints

- No hand-written JS, no JS bundlers (vite, esbuild, webpack), no `protobufjs`, no three.js, no Cosmograph, no OrbitControls. wgpu + Dioxus only.
- Styling lives in `app/ui/assets/app.css` + `panel_kit::CSS` — keep it there, not inline in components.
- Match the existing palette/font: the panel-kit theme, Courier Prime monospace.

## Session completion (mandatory)

Work is **not complete until `git push` succeeds**. Before ending a session:

1. Record remaining work for the next session (in the relevant `docs/*.md` plan, or the handoff note below).
2. Run quality gates (`cargo check --workspace --tests`, `just test browser-rust` if visual).
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
