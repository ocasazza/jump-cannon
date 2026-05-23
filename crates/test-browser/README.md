# test-browser

Rust-driven browser smoke test for the graph-renderer WASM bundle.
Foundation crate — the assertion set is deliberately minimal while the
frontend stabilizes.

## What it asserts today

1. The page at `--base-url` responds with HTTP 200.
2. Headless Chromium launches with WebGPU flags and navigates.
3. The boot log line `[graph-renderer] status footer mounted` appears
   on the JS console within `--timeout-secs`.
4. The `#graph-canvas` element exists with non-zero width and height.
5. A screenshot is written to `<out-dir>/boot.png`. **Pixel content is
   not asserted** — that's the flaky part the legacy
   `tests/browser/run.mjs` suite handles.

Future additions (motion deltas, click-doesn't-blank, tag round-trips,
`/compute/health` shape, etc.) will be ported from `run.mjs` as the
frontend stops thrashing.

## Running locally

```
just test browser-rust
# or directly:
nix run .#test-browser-rust
```

The wrapper script (`flake.nix#test-browser-rust`) expects a trunk-built
dist at `crates/graph-renderer/assets/dist`. Run `just wasm` first if
you haven't. There is no `graph-renderer-web` flake derivation yet —
wiring trunk into crane is a follow-up.

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

## Relationship to the legacy suite

`tests/browser/*.mjs` (Playwright) remains in place as a side-by-side
fallback so coverage doesn't drop while the Rust suite matures.
`just test browser` runs the JS version; `just test browser-rust` runs
this one. Both should pass — once the Rust suite covers the same
assertions, the JS suite can be retired.
