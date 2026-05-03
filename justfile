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

# All-in-one dev server. Builds WASM bundle if missing, then watches the API.
dev:
    @if [ ! -d crates/graph-renderer/assets/dist ]; then \
        echo "→ first-run trunk build…"; \
        trunk build --release; \
    fi
    cargo build --release -p vault-search
    cargo watch \
      -w crates/graph-api -w crates/vault-data -w crates/vault-links -w crates/graph-metrics \
      -x 'run -p graph-api -- --assets-dir crates/graph-renderer/assets/dist'

# Run the production binary (embedded assets, no watch).
run:
    cargo run --release -p graph-api

# Build everything in release mode.
build:
    cargo build --release --workspace

# Run all tests.
test:
    cargo test --workspace

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
test-browser: wasm
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

# Headless browser PROFILER. Same setup as test-browser but instead of a
# pass/fail screenshot check it captures rAF frame timings + a V8 CPU
# profile (.cpuprofile) per phase (idle / palette-open / fit-camera).
# Output: tests/browser/out/profile-*.{png,cpuprofile} + a JSON summary
# on stdout (avg FPS, p50/p95/p99 frame time, jank pct, top-12 hot fns).
# Drop the .cpuprofile into Chrome DevTools → Performance for a flame chart.
profile-browser: wasm
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

# Headless perf gate. Single phase, single synth vault, asserts FPS /
# p99 / jank thresholds. Fails the process (exit 1) on regression. Use
# in CI alongside test-browser. Tunables (env): PERF_VAULT_NODES (1000),
# PERF_MIN_FPS (50), PERF_MAX_P99_MS (25), PERF_MAX_JANK_PCT (5).
# Side effect: writes tests/browser/out/perf-idle.flame.txt — an
# AI-readable text flame graph of where time went.
perf-test: wasm
    cargo build --release -p graph-api
    @if [ ! -d tests/browser/node_modules ]; then \
        echo "→ installing playwright npm package (one-time)…"; \
        cd tests/browser && npm install --silent --no-audit --no-fund; \
    fi
    cd tests/browser && node perf.mjs
