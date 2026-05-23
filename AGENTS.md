# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Copilot, Cursor, etc.) when working with code in this repository.

## Hard repo rule: Rust everywhere

The frontend is **Rust + wgpu + egui** compiled to WebAssembly. The backend is **Rust + axum**. No JavaScript, no TypeScript, no React, no Vue, no DOM frameworks, no virtual DOM. The single allowed JS shim is `crates/graph-renderer/assets/main.js`, which **must stay under 50 lines** and do exactly one thing: load the WASM module and hand control to Rust. No DOM manipulation, no event handlers, no fetch helpers, no protobuf decoding, no UI state.

If you're tempted to "just add a quick JS function" — stop. Add it in Rust. The egui surface in `graph-renderer/src/ui/` is where UI goes; `graph-renderer/src/web.rs` is where browser-side event hooks live.

The test harness under `tests/browser/*.mjs` and `crates/test-browser/` is the only exception, and only because it drives a real browser from outside the WASM bundle.

## Crate map

| Crate | Role |
|---|---|
| `crates/graph-api` | axum HTTP server. Loads the vault, serves `/graph/*`, `/node/*id`, `/search`, `/vault/page` (editor PUT), `/progress`, etc. Watches `$VAULT_ROOT` via `notify` and atomically swaps the in-memory `GraphSnapshot` through `arc-swap`. |
| `crates/graph-renderer` | Rust + wgpu + egui via eframe. Native binary + WASM (trunk-built). **All UI lives here**: the floating panel system (`ui/floating.rs`), inspector, editable page viewer (`ui/page_viewer.rs`), anchored hover/click cards (`ui/anchored.rs`), Instances panel with YAML export/import + state-snapshot timeline. |
| `crates/graph-layouts` | wgpu compute force-sim. Native + WASM. |
| `crates/graph-compute` | Optional standalone layout solver, gRPC on `[::1]:50051`. Opt-in via `--compute-url` / `GRAPH_API_COMPUTE_URL` — unset means the broker is never dialed. |
| `crates/graph-metrics` | PageRank, Louvain, k-core, etc. |
| `crates/vault-data`, `crates/vault-links`, `crates/vault-search` | Vault data pipeline: markdown parsing, link extraction, Tantivy index. |
| `crates/test-browser` | Rust-only Chromium driver (chromiumoxide) for the foundational browser regression suite. |

## Build & development

The whole repo builds through **nix + crane + trunk**. No `npm install`, no `wasm-pack`, no yarn lockfile.

- `nix build` — deployable bundle (server binary + WASM).
- `just dev-up` — full dev stack: graph-api with file watcher, graph-compute optional, hot-reload via trunk watch.
- `just dev-down` — symmetric teardown.
- `just wasm` — `trunk build --release`. Required once before `just dev-up` until `graph-renderer-web` becomes a flake output.
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

## Workflow: `bd` (beads) for issue tracking

This project uses **bd** for issue tracking. Run `bd prime` for the full workflow.

```bash
bd ready              # find available work
bd show <id>          # view issue details
bd update <id> --claim  # claim work atomically
bd close <id>         # complete work
bd dolt push          # push beads data to remote
```

- Use `bd` for ALL task tracking — do **not** use TodoWrite, TaskCreate, or markdown TODO lists.
- Use `bd remember` for persistent knowledge — do **not** create `MEMORY.md` files.

## Session completion (mandatory)

Work is **not complete until `git push` succeeds**. Before ending a session:

1. File issues for remaining work via `bd`.
2. Run quality gates (`cargo check --workspace --tests`, `just test browser` if visual).
3. Update issue status (close finished work, update in-progress items).
4. Push:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status   # MUST show "up to date with origin"
   ```
5. Clean up stashes; prune remote branches.
6. Hand off context for the next session.

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
