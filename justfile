# jump-cannon — task runner. `just <recipe>` to run.
# `just` lists all recipes.

set dotenv-load := true
set positional-arguments

# Default: show recipes
default:
    @just --list

# Build the trunk-managed Rust+egui+wgpu frontend.
wasm:
    trunk build --release

# Watch + auto-rebuild on Rust source changes.
watch-wasm:
    trunk watch

#
# Dioxus + Tauri desktop app (the app/ workspace). The shell is a pure
# webview container — start the backend separately (`just dev-up`) and the
# app connects to graph-api over HTTP (default http://127.0.0.1:8765,
# configurable in its Settings panel).

# Run the desktop app with hot-reload (trunk serve behind tauri dev).
app-dev:
    cd app && cargo tauri dev

# Build the release bundle (.dmg / .AppImage / .msi per platform).
app-build:
    cd app && cargo tauri build

# Type-check the app workspace: WASM frontend + native Tauri shell.
# (The frontend also builds reproducibly via `nix build .#app-web`.)
app-check:
    cd app && cargo check --target wasm32-unknown-unknown -p panel-kit -p jump-cannon-ui
    cd app && cargo check -p jump-cannon-app

# Regenerate the checked-in prost output (app/ui/src/proto/) after editing
# crates/graph-api/proto/graph.proto. Checked in (instead of a build.rs) so
# the app workspace is self-contained — no protoc, pure crane/nix build.
app-proto:
    cargo check -p graph-api
    cp -f "$(ls -t target/debug/build/graph-api-*/out/jumpcannon.graph.rs | head -1)" app/ui/src/proto/jumpcannon.graph.rs
    @echo "regenerated app/ui/src/proto/jumpcannon.graph.rs"

#
# Internals: backgrounds `nix run .#dev-up` (native binary on darwin,
# podman+compose on linux) for the distributed compute backend, then runs
# the API server with cargo-watch in the foreground. Ctrl-C tears down both
# via a trap. `nix run .#dev-up` standalone still works for orchestration
# (CI, deploy, integration tests) — this recipe is the contributor entry.

# Notes: `gpu` selects the Barnes-Hut wgpu engine (fa2-bh), `cpu` selects
# SGD-stress (sgd-stress). The worker hosts every engine regardless — this only
# sets the broker's INITIAL pick; switch live from the UI's "Remote engine"
# picker. On a host with no usable GPU adapter the GPU engines fail init — use
# `cpu` there.
# All-in-one dev stack + hot-reload; backend = gpu (default) | cpu.
dev-up backend="gpu":
    #!/usr/bin/env bash
    set -euo pipefail

    # ---- Stage 0: resolve the requested compute backend ----
    case "{{backend}}" in
      gpu) COMPUTE_ENGINE="fa2-bh"     ;;  # GPU: Barnes-Hut ForceAtlas2 (wgpu)
      cpu) COMPUTE_ENGINE="sgd-stress" ;;  # CPU: SGD stress-majorization
      *)
        echo "dev-up: unknown backend '{{backend}}' (expected 'gpu' or 'cpu')" >&2
        exit 2
        ;;
    esac
    echo "→ compute backend: {{backend}} (engine: $COMPUTE_ENGINE)"

    # ---- Stage 1: WASM bundle, always built ----
    # The trunk dist is what graph-api serves at `/`. The previous
    # "skip if dist exists" check meant any change in graph-renderer
    # (the WASM half) was silently invisible after the first dev-up of
    # the day — the user saw stale UI for hours wondering why their
    # recent commits had no effect. Always run trunk; incremental
    # builds are seconds, full ones ~30s. Worth the predictability.
    echo "→ trunk build (WASM)…"
    trunk build --release

    # Background trunk watch so subsequent edits to graph-renderer
    # rebuild the WASM bundle while dev-up is running. The user just
    # refreshes the browser; no need to Ctrl-C + restart dev-up.
    trunk watch &
    TRUNK_PID=$!

    # ---- Stage 2 (parallel): everything else ----
    # Three independent builds/processes run concurrently so the user
    # doesn't wait for vault-search → backend → api serially:
    #   1. vault-search compile         (~30s cold, fast incremental)
    #   2. graph-compute backend boot   (~5s after binary build)
    #   3. graph-api pre-build          (~10-30s cold; warms the cargo
    #      cache so `cargo watch`'s startup build is a no-op and the
    #      server starts serving the frontend within ms)

    echo "→ kicking off parallel builds + backend…"
    cargo build --release -p vault-search &
    VAULT_PID=$!

    # Build graph-compute (release for GPU backends to work properly)
    cargo build --release -p graph-compute &
    COMPUTE_BUILD_PID=$!

    cargo build -p graph-api &
    API_BUILD_PID=$!

    # Wait for compute build before starting it
    wait "$COMPUTE_BUILD_PID" || { echo "graph-compute build failed"; exit 1; }

    # Start graph-compute natively (avoids nix remote-build SSH issues)
    RUST_LOG=info GRAPH_COMPUTE_TICK_HZ=30 ./target/release/graph-compute > /tmp/graph-compute.log 2>&1 &
    BACKEND_PID=$!

    cleanup() {
        echo
        echo "→ tearing down (backend pid $BACKEND_PID, vault pid $VAULT_PID, trunk pid $TRUNK_PID)…"
        # Kill tracked PIDs first with SIGKILL (-9) for immediate termination
        kill -9 "$BACKEND_PID" 2>/dev/null || true
        kill -9 "$VAULT_PID" 2>/dev/null || true
        kill -9 "$API_BUILD_PID" 2>/dev/null || true
        kill -9 "$TRUNK_PID" 2>/dev/null || true
        # Kill cargo-watch (starts after trap, so not in a tracked PID var)
        pkill -9 -f "cargo-watch.*graph-api" 2>/dev/null || true
        # Comprehensive fallback: kill any stragglers by process name
        pkill -9 -f "trunk watch" 2>/dev/null || true
        pkill -9 -f "graph-compute" 2>/dev/null || true
        pkill -9 -f "graph-api" 2>/dev/null || true
        # Idempotent: kills the native binary (darwin) or stops compose (linux).
        nix run .#dev-down 2>/dev/null || true
        echo "→ teardown complete"
    }
    trap cleanup EXIT INT TERM

    # Wait for the api pre-build before handing off to cargo-watch. The
    # other three (vault-search, backend, trunk watch) continue in the
    # background — the frontend doesn't block on any of them.
    wait "$API_BUILD_PID" || { echo "graph-api build failed"; exit 1; }

    echo "→ graph-api built; starting hot-reload server (frontend live now, WASM rebuilds on edit)…"
    # graph-api's compute broker is now opt-in (no default URL), so dev-up
    # must explicitly point it at the local graph-compute worker that
    # `nix run .#dev-up` boots on [::1]:50051. Standalone `cargo run -p
    # graph-api` without this env var simply runs broker-disabled and
    # logs no warnings — exactly what we want outside dev-up.
    # TODO: if `nix run .#dev-up` is ever made optional (e.g. a flag to
    # skip the backend for frontend-only iteration), gate this env var
    # accordingly.
    # JUMP_CANNON_COMPUTE_LAYOUT_ID picks the broker's initial remote engine
    # (read by RemoteLayout::from_env); the UI's "Remote engine" picker can
    # change it at runtime via PUT /compute/layout.
    JUMP_CANNON_COMPUTE_URL=http://[::1]:50051 \
    JUMP_CANNON_COMPUTE_LAYOUT_ID="$COMPUTE_ENGINE" \
    exec cargo watch \
      -w crates/graph-api -w crates/vault-data -w crates/vault-links -w crates/graph-metrics \
      -x 'run -p graph-api -- --assets-dir crates/graph-renderer/assets/dist'

# Symmetric teardown for `just dev-up`. Idempotent — safe to re-run.
dev-down:
    nix run .#dev-down

# Run the production binary (embedded assets, no watch).
run:
    cargo run --release -p graph-api

# Build everything in release mode.
build:
    cargo build --release --workspace

# Run tests. `just test` runs everything; pass a target for one layer.
# An optional ARG parameterizes targets that take a knob:
#   just test                # all (cargo + browser smoke + regression e2e)
#   just test cargo          # native unit + integration tests (incl. regression.rs + fuzz.rs at default volume)
#   just test fuzz [N]       # property-based fuzz, N cases per layout (default 10000)
#   just test bench          # criterion benches across layouts; HTML in target/criterion/
#   just test canary [URL]   # live-cluster gRPC smoke (default URL: http://[::1]:50051)
#   just test geometric      # geometric engine: solved-case canary + regression golden + perf
#   just test geometric-golden # regenerate the geometric regression golden (intentional baseline bump)
#   just test browser        # Playwright canvas-not-black smoke
#   just test regression     # Playwright UI regression suite
#   just test perf           # headless perf gate (synth vault)
#   just test profile        # diagnostic profiler (3-phase flame trace)
test target='all' arg='':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
      all)        just test cargo && just test browser && just test regression ;;
      cargo)      cargo test --workspace ;;
      fuzz)       PROPTEST_CASES="${ARG:-{{arg}}}"; PROPTEST_CASES="${PROPTEST_CASES:-10000}" \
                  cargo test -p graph-layouts --test fuzz --release ;;
      bench)      cargo run --release -p graph-layouts --example bench_static_layouts -- --bench ;;
      canary)     URL="${ARG:-{{arg}}}"; URL="${URL:-http://[::1]:50051}" \
                  GRAPH_COMPUTE_CANARY_URL="$URL" \
                  cargo test -p graph-compute --test canary -- --nocapture ;;
      geometric)  cargo test -p graph-compute --test geometric_solver -- --nocapture ;;
      geometric-golden) UPDATE_GEOMETRIC_GOLDEN=1 \
                  cargo test -p graph-compute --test geometric_solver regression_golden_master -- --nocapture ;;
      browser)    just _test-browser ;;
      browser-rust) just _test-browser-rust ;;
      regression) just _test-regression ;;
      perf)       just _test-perf ;;
      profile)    just _test-profile ;;
      *) echo "unknown test target: {{target}} (try: cargo, fuzz, bench, canary, geometric, geometric-golden, browser, browser-rust, regression, perf, profile)" >&2; exit 1 ;;
    esac

# Format the workspace.
fmt:
    cargo fmt --all

# Lint with clippy, deny warnings.
clippy:
    cargo clippy --all-targets -- -D warnings

# Reindex vault-search manually.
reindex VAULT_ROOT=`echo ${VAULT_ROOT:-.}`:
    cargo run --release -p vault-search -- --vault {{ VAULT_ROOT }} --rebuild --port 0

# Tail graph-api logs (only useful if launched detached).
logs:
    tail -f /tmp/graph-api.log

# Kill any stray graph-api / vault-search processes from previous runs.
kill:
    -pkill -f 'graph-api'
    -pkill -f 'vault-search'

# Nuke the trunk dist to force a full rebuild.
clean-wasm:
    rm -rf crates/graph-renderer/assets/dist

# Headless browser test. Spins up graph-api against a tiny test vault, opens
# it in Chromium with WebGPU enabled, screenshots the canvas, asserts it
# isn't all-black + no console errors. Output:
# tests/browser/out/screenshot.png and a JSON result on stdout.
# Exit 0 = ok, 1 = canvas dark / page error / startup timeout.
_test-browser: wasm
    @# Build graph-api binary if missing
    cargo build --release -p graph-api
    @# Tiny synthetic vault with three cross-linked notes
    @mkdir -p /tmp/test-vault
    @if [ ! -f /tmp/test-vault/Alpha.md ]; then \
        printf 'See [[Beta]] and [[Gamma]].\n' > /tmp/test-vault/Alpha.md; \
        printf '[[Alpha]]\n' > /tmp/test-vault/Beta.md; \
        printf '[[Alpha]] [[Beta]]\n' > /tmp/test-vault/Gamma.md; \
    fi
    @# One-time playwright install. PLAYWRIGHT_BROWSERS_PATH (set in the
    @# nix devshell) points at the nix-provided Chromium bundle, so the
    @# install is just the npm package — no browser download.
    @if [ ! -d tests/browser/node_modules ]; then \
        echo "→ installing playwright npm package (one-time)…"; \
        cd tests/browser && npm install --silent --no-audit --no-fund; \
    fi
    cd tests/browser && node run.mjs

# Rust-driven browser smoke test (foundation). Sibling to _test-browser
# but the harness is a Rust binary (crates/test-browser) driving chromium
# via CDP. The legacy Playwright suite is kept as a fallback while the
# Rust assertion set grows. Output: target/test-browser-rust/{boot.png,
# report.json}.
_test-browser-rust: wasm
    nix run .#test-browser-rust

# Headless browser REGRESSION suite. Sibling to `test-browser`; shares
# the same boot scaffolding (factored into tests/browser/harness.mjs)
# but runs a handful of named UI regression checks instead of a single
# canvas-bright smoke. Output: tests/browser/out/regression-*.png + a
# JSON line on stdout. Exit 0 = ok, 1 = regression fired.
_test-regression: wasm
    cargo build --release -p graph-api
    @mkdir -p /tmp/test-vault
    @if [ ! -f /tmp/test-vault/Alpha.md ]; then \
        printf 'See [[Beta]] and [[Gamma]].\n' > /tmp/test-vault/Alpha.md; \
        printf '[[Alpha]]\n' > /tmp/test-vault/Beta.md; \
        printf '[[Alpha]] [[Beta]]\n' > /tmp/test-vault/Gamma.md; \
    fi
    @if [ ! -d tests/browser/node_modules ]; then \
        echo "→ installing playwright npm package (one-time)…"; \
        cd tests/browser && npm install --silent --no-audit --no-fund; \
    fi
    cd tests/browser && node regression.mjs

# Headless browser PROFILER. Same setup as test-browser but instead of a
# pass/fail screenshot check it captures rAF frame timings + a V8 CPU
# profile (.cpuprofile) per phase (idle / palette-open / fit-camera).
# Output: tests/browser/out/profile-*.{png,cpuprofile} + a JSON summary
# on stdout (avg FPS, p50/p95/p99 frame time, jank pct, top-12 hot fns).
# Drop the .cpuprofile into Chrome DevTools → Performance for a flame chart.
_test-profile: wasm
    cargo build --release -p graph-api
    @mkdir -p /tmp/test-vault
    @if [ ! -f /tmp/test-vault/Alpha.md ]; then \
        printf 'See [[Beta]] and [[Gamma]].\n' > /tmp/test-vault/Alpha.md; \
        printf '[[Alpha]]\n' > /tmp/test-vault/Beta.md; \
        printf '[[Alpha]] [[Beta]]\n' > /tmp/test-vault/Gamma.md; \
    fi
    @if [ ! -d tests/browser/node_modules ]; then \
        echo "→ installing playwright npm package (one-time)…"; \
        cd tests/browser && npm install --silent --no-audit --no-fund; \
    fi
    cd tests/browser && node profile.mjs

# Headless perf gate. Defaults to a synth 1000-node vault — that's our
# defended floor (gates against regression below 60 fps / 25ms p99).
# For stress measurements at higher node counts, set PERF_VAULT_NODES
# manually; expect the gate to fail above ~3000 nodes until further
# render-side optimization lands. Tunables (env): PERF_VAULT_NODES (1000),
# PERF_MIN_FPS (50), PERF_MAX_P99_MS (25), PERF_MAX_JANK_PCT (5).
# Side effect: writes tests/browser/out/perf-idle.flame.txt — an
# AI-readable text flame graph of where time went.
_test-perf: wasm
    cargo build --release -p graph-api
    @if [ ! -d tests/browser/node_modules ]; then \
        echo "→ installing playwright npm package (one-time)…"; \
        cd tests/browser && npm install --silent --no-audit --no-fund; \
    fi
    cd tests/browser && node perf.mjs

# Manage the graph-compute cluster. WHERE picks the backend.
#   just cluster up [local|sky]      # default: local (podman) — also auto-renders configs
#   just cluster down [local|sky]
#   just cluster endpoint sky        # prints JUMP_CANNON_COMPUTE_URL=http://<host>:50051
#   just cluster render              # regenerate docker-compose.yml + infra/sky/*.yaml from flake.nix
cluster action='up' where='local':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{action}}/{{where}}" in
      up/local)       nix run .#render-stack-configs && nix run .#dev-up ;;
      down/local)     nix run .#dev-down ;;
      up/sky)         sky launch -c graph-compute infra/sky/graph-compute.yaml --yes ;;
      down/sky)       sky down graph-compute --yes ;;
      endpoint/sky)   sky status --endpoint 50051 graph-compute | awk '{print "JUMP_CANNON_COMPUTE_URL=http://" $0}' ;;
      render/*)       nix run .#render-stack-configs ;;
      *) echo "usage: just cluster {up|down|endpoint|render} [local|sky]" >&2; exit 1 ;;
    esac
