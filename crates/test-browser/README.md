# test-browser

Rust-driven browser smoke test for the Dioxus frontend (`app/ui`) WASM
bundle. It exercises the built app through Chromium's DevTools Protocol; no
hand-written JavaScript is shipped with the frontend.

## What it asserts today

1. The page at `--base-url` responds with HTTP 200.
2. Headless Chromium launches with WebGPU flags and navigates.
3. The boot log line `[jump-cannon-ui] boot` appears on the JS console
   within `--timeout-secs` (logged from `app/ui/src/main.rs`).
4. The Dioxus mount and pre-WASM shell obey the startup invariant: `#main`
   exists, the marked static shell is its adjacent sibling rather than a
   child, and the shell is hidden after Dioxus mounts. Keeping foreign DOM out
   of `#main` prevents Dioxus 0.6's `invalid key` startup panic.
5. The Graph panel has initialized its WebGPU render host, loaded at least one
   node, and exposes a non-zero-size canvas.
6. In tiling mode, Pause/Resume and Fit are visible inside the Graph header,
   clicking them does not start a panel drag, and the canvas remains mounted.
7. No console error, unhandled runtime exception, or failed resource load was
   observed.
8. A screenshot is written to `<out-dir>/boot.png` for visual review. Pixel
   content is not automatically asserted.

The retired egui-era Playwright suite lives in git history; this Rust harness
is the browser regression gate for the Dioxus app.

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

- `boot.png` — screenshot at the moment all assertions passed
- `report.json` — machine-readable result including canvas dimensions, boot
  and mount-handoff state, render/node readiness, header-action checks,
  browser errors, and recent console logs

## CLI

```
test-browser \
  --base-url http://127.0.0.1:8765 \
  --chromium /path/to/chromium \
  --out-dir target/test-browser-rust \
  --timeout-secs 60
```
