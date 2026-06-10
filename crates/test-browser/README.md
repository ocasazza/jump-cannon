# test-browser

Rust-driven browser smoke test for the Dioxus frontend (app/ui) WASM
bundle. Foundation crate — the assertion set is deliberately minimal
while the frontend stabilizes.

## What it asserts today

1. The page at `--base-url` responds with HTTP 200.
2. Headless Chromium launches with WebGPU flags and navigates.
3. The boot log line `[jump-cannon-ui] boot` appears on the JS console
   within `--timeout-secs` (logged from `app/ui/src/main.rs`).
4. The graph `<canvas>` element exists with non-zero width and height.
5. A screenshot is written to `<out-dir>/boot.png`. **Pixel content is
   not asserted** — that's the flaky part.

Future additions: motion deltas, click-doesn't-blank, tag round-trips,
`/compute/health` shape, etc. (The egui-era Playwright suite that held
those checks was removed with the egui frontend — see git history.)

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
- `report.json` — JSON with `{ok, canvas_width, canvas_height,
  boot_log_found, duration_ms, console_logs[]}`

## CLI

```
test-browser \
  --base-url http://127.0.0.1:8765 \
  --chromium /path/to/chromium \
  --out-dir target/test-browser-rust \
  --timeout-secs 60
```
