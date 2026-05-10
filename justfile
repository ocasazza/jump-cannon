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

# Run tests. `just test` runs everything; pass a target for one layer.
# An optional ARG parameterizes targets that take a knob:
#   just test                # all (cargo + browser smoke + regression e2e)
#   just test cargo          # native unit + integration tests (incl. regression.rs + fuzz.rs at default volume)
#   just test fuzz [N]       # property-based fuzz, N cases per layout (default 10000)
#   just test bench          # criterion benches across layouts; HTML in target/criterion/
#   just test canary [URL]   # live-cluster gRPC smoke (default URL: http://[::1]:50051)
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
      browser)    just _test-browser ;;
      regression) just _test-regression ;;
      perf)       just _test-perf ;;
      profile)    just _test-profile ;;
      *) echo "unknown test target: {{target}} (try: cargo, fuzz, bench, canary, browser, regression, perf, profile)" >&2; exit 1 ;;
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
