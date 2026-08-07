# test-browser

Rust-driven browser regression test for the Dioxus frontend (app/ui) WASM
bundle.

## What it asserts today

1. The page at `--base-url` responds with HTTP 200.
2. Headless Chromium launches with WebGPU flags and navigates.
3. The boot log line `[jump-cannon-ui] boot` appears on the JS console
   within `--timeout-secs` (logged from `app/ui/src/main.rs`).
4. The graph `<canvas>` exists, reaches render-ready, and has non-zero size.
5. Graph header actions are visible, clickable, and do not begin a panel drag.
6. Nodes has a left navigator and wider focused-content pane; selecting a
   seeded note loads its body.
7. Flat/Tags round-trips preserve selection, exact multi-tag groups contain
   one row per node, synthetic `(untagged)` is present, core schema keys remain
   visible, and the controls stay below the panel header in floating and tiling.
8. Unified Settings exposes four accessible, content-backed tabs; restoring it
   remounts a render-ready Graph canvas.
9. CDP resource errors, console errors, Rust tracing errors, and unhandled
   exceptions fail the run.
10. Screenshots are written to `<out-dir>/nodes-editor.png` and `boot.png`.
   Pixel content is reviewed rather than asserted.

On Linux, the harness uses Vulkan for WebGPU compute while disabling the
display surface; unified headless Chrome presents through its offscreen path
instead of waiting on a nonexistent swapchain.
Each invocation also uses incognito mode and a run-scoped Chromium profile, so
persisted Panel Kit state and concurrent Chrome profile locks cannot leak
between regressions.

## Running locally

```
just test browser-rust
# or directly:
nix run .#test-browser-rust
```

The wrapper script (`flake.nix#test-browser-rust`) serves the nix-built
`app-web` dist by default; override with `ASSETS_DIR=app/ui/dist` after
a local `cd app && trunk build --release` if you want to test local
edits.

Output lands in `target/test-browser-rust/`:
- `nodes-editor.png` — Nodes workbench with tag hierarchy and focused content
- `boot.png` — screenshot at the moment all assertions passed
- `report.json` — JSON including structured `nodes_editor`, `settings_tabs`,
  `graph_header_actions`, canvas, boot-log, and browser-error results

## CLI

```
test-browser \
  --base-url http://127.0.0.1:8765 \
  --chromium /path/to/chromium \
  --out-dir target/test-browser-rust \
  --timeout-secs 60
```
