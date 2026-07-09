# jump-cannon — task runner.
#
#   just                 list every recipe, grouped
#   just test            run the full test suite      (module: `just test --list`)
#   just test fuzz 5000  run one test target with args
#   just cluster up sky  manage the compute cluster   (module: `just cluster --list`)
#
# `test` and `cluster` are modules (just/*.just): their subcommands are real,
# completable recipes with their own `--list`, not bash-case dispatch.

set dotenv-load := true
set positional-arguments

# Subcommand modules — the "rich grammar". Each lives in just/<name>.just;
# run `just --list <mod>` for its subcommands.
# Test suite: `just test`, `just test cargo`, `just test fuzz 5000`, …
mod test 'just/test.just'
# graph-compute cluster: `just cluster up`, `just cluster down sky`, …
mod cluster 'just/cluster.just'

# Shared paths — single source of truth, no hardcoded duplicates across recipes.
app_dir := "app"
dist     := app_dir / "ui/dist"

# Default: list every recipe (grouped by [group(...)]).
default:
    @just --list

#
# ── app ── Dioxus + Tauri desktop app (the app/ workspace). The shell is a pure
# webview container — start the backend separately (`just dev-up`) and the app
# connects to graph-api over HTTP (default http://127.0.0.1:8765, configurable
# in its Settings panel).
#

# Run the desktop app with hot-reload (trunk serve behind tauri dev).
[group('app')]
app-dev:
    cd {{app_dir}} && cargo tauri dev

# Build the release bundle (.dmg / .AppImage / .msi per platform).
[group('app')]
app-build:
    cd {{app_dir}} && cargo tauri build

# Type-check the app workspace: WASM frontend + worker bundle + native Tauri
# shell. (The frontend also builds reproducibly via `nix build .#app-web`.)
[group('app')]
[doc('Type-check the app workspace (WASM frontend + worker + native Tauri shell)')]
app-check:
    cd {{app_dir}} && cargo check --target wasm32-unknown-unknown -p panel-kit -p jump-cannon-ui
    cd {{app_dir}} && cargo check --target wasm32-unknown-unknown -p tvix-worker --features wasm
    cd {{app_dir}} && cargo check -p jump-cannon-app

# Regenerate the checked-in prost output (app/ui/src/proto/) after editing
# crates/graph-api/proto/graph.proto. Checked in (instead of a build.rs) so
# the app workspace is self-contained — no protoc, pure crane/nix build.
[group('app')]
[doc('Regenerate the checked-in prost output after editing graph.proto')]
app-proto:
    cargo check -p graph-api
    cp -f "$(ls -t target/debug/build/graph-api-*/out/jumpcannon.graph.rs | head -1)" {{app_dir}}/ui/src/proto/jumpcannon.graph.rs
    @echo "regenerated {{app_dir}}/ui/src/proto/jumpcannon.graph.rs"

#
# ── dev ── the contributor entry point. `dev-up` backgrounds `nix run .#dev-up`
# (native binary on darwin, podman+compose on linux) for the distributed compute
# backend, then runs the API server with cargo-watch in the foreground. Ctrl-C
# tears down both via a trap. `nix run .#dev-up` standalone still works for
# orchestration (CI, deploy, integration tests).
#

# Notes: `gpu` selects the Barnes-Hut wgpu engine (fa2-bh), `cpu` selects
# SGD-stress (sgd-stress). The worker hosts every engine regardless — this only
# sets the broker's INITIAL pick; switch live from the UI's "Remote engine"
# picker. On a host with no usable GPU adapter the GPU engines fail init — use
# `cpu` there.
# All-in-one dev stack + hot-reload; backend = gpu (default) | cpu.
# The frontend is the Dioxus app (app/ui) — served by graph-api at :8765
# in the browser, and what `just app-dev`'s Tauri shell connects to.
[group('dev')]
[doc('All-in-one dev stack + hot-reload; backend = gpu (default) | cpu')]
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

    TRUNK_DIR="{{app_dir}}"
    ASSETS="{{dist}}"
    echo "→ frontend: {{app_dir}} (Dioxus, dist: $ASSETS)"

    # ---- Stage 1: WASM bundle, always built ----
    # The trunk dist is what graph-api serves at `/`. The previous
    # "skip if dist exists" check meant any change in the frontend
    # (the WASM half) was silently invisible after the first dev-up of
    # the day — the user saw stale UI for hours wondering why their
    # recent commits had no effect. Always run trunk; incremental
    # builds are seconds, full ones ~30s. Worth the predictability.
    echo "→ trunk build (WASM)…"
    (cd "$TRUNK_DIR" && trunk build --release)

    # Background trunk watch so subsequent frontend edits rebuild the
    # WASM bundle while dev-up is running. The user just refreshes the
    # browser; no need to Ctrl-C + restart dev-up.
    (cd "$TRUNK_DIR" && exec trunk watch) &
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
      -x "run -p graph-api -- --assets-dir $ASSETS"

# Symmetric teardown for `just dev-up`. Idempotent — safe to re-run.
[group('dev')]
dev-down:
    nix run .#dev-down

# ── Quick backend stack ── the fast path for compute / GPU work: native
# graph-compute (→ Metal on macOS, real hardware) + graph-api serving the built
# Dioxus UI. It builds the WASM bundle ONCE and serves it (no trunk-watch /
# cargo-watch hot-reload — that's the only thing it drops vs `dev-up`, and what
# keeps it quick). Detached, so it returns to your shell with the stack running
# and the app open at http://127.0.0.1:$GRAPH_API_PORT. All config is
# declarative in `.env` (VAULT_ROOT, GRAPH_API_PORT, JUMP_CANNON_COMPUTE_URL).
# Inspect with `just stack-status`; stop with `just stack-down`. For live
# frontend hot-reload while editing the UI, use `just dev-up`.
[group('dev')]
[doc('Quick dev stack: native graph-compute (Metal) + graph-api + served Dioxus UI')]
stack backend="gpu":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{backend}}" in
      gpu) engine=fa2-bh ;;      # GPU: Barnes-Hut ForceAtlas2 (wgpu → Metal)
      cpu) engine=sgd-stress ;;  # CPU: SGD stress-majorization
      *) echo "stack: unknown backend '{{backend}}' (expected gpu|cpu)" >&2; exit 2 ;;
    esac
    port="${GRAPH_API_PORT:-8765}"
    echo "→ backend={{backend}} (engine: $engine) · app :${port} · worker ${JUMP_CANNON_COMPUTE_URL:-unset}"
    echo "→ building graph-compute + graph-api + vault-search…"
    cargo build -p graph-compute -p graph-api -p vault-search
    echo "→ building frontend WASM bundle (trunk; incremental)…"
    (cd {{app_dir}} && trunk build)
    just stack-down >/dev/null 2>&1 || true   # idempotent: clear any prior stack
    echo "→ starting graph-compute (native worker — Metal on macOS)…"
    RUST_LOG=info nohup ./target/debug/graph-compute > /tmp/graph-compute.log 2>&1 &
    echo "→ starting graph-api (serving the UI from {{dist}})…"
    JUMP_CANNON_COMPUTE_LAYOUT_ID="$engine" RUST_LOG=info \
      nohup ./target/debug/graph-api --assets-dir {{dist}} > /tmp/graph-api.log 2>&1 &
    echo "→ waiting for graph-api on :${port}…"
    curl -s --retry 90 --retry-delay 1 --retry-connrefused "http://127.0.0.1:${port}/graph/init" -o /dev/null || true
    echo
    just stack-status

# Probe a running `just stack`: worker GPU adapter, broker health, engine list.
[group('dev')]
stack-status:
    #!/usr/bin/env bash
    port="${GRAPH_API_PORT:-8765}"
    echo "── graph-compute adapter ──"
    grep -iE 'adapter initialized|backend=' /tmp/graph-compute.log 2>/dev/null | tail -1 || echo "  (no worker log yet)"
    echo "── /compute/health (frontend-facing) ──"
    curl -s "http://127.0.0.1:${port}/compute/health" 2>/dev/null || echo "  (graph-api not responding)"; echo
    echo "── /compute/engines ──"
    curl -s "http://127.0.0.1:${port}/compute/engines" 2>/dev/null \
      | python3 -c 'import sys,json; d=json.load(sys.stdin); print("  active:",d.get("active")); [print("   -",e["id"]) for e in d.get("engines",[])]' \
      2>/dev/null || echo "  (could not parse engines — is the stack up?)"
    echo "── frontend ──"
    if curl -s "http://127.0.0.1:${port}/" 2>/dev/null | grep -qiE '<html|jump-cannon|<script'; then
      echo "  UI served ✓"
    else
      echo "  UI not served (no --assets-dir dist?)"
    fi
    echo "── open ──"
    echo "  app    : http://127.0.0.1:${port}   (Dioxus UI + API)"
    echo "  worker : ${JUMP_CANNON_COMPUTE_URL:-unset}"

# Stop the `just stack` backend (scoped to its binaries; safe to re-run).
[group('dev')]
stack-down:
    -@pkill -f 'target/debug/graph-compute' 2>/dev/null || true
    -@pkill -f 'target/debug/graph-api' 2>/dev/null || true
    @echo "→ stack stopped"

# Run the production binary (no watch). Assets are no longer embedded in
# the binary — build the app dist first and serve it via --assets-dir.
[group('dev')]
[doc('Run the production binary (no watch): build app dist, then serve it')]
run:
    cd {{app_dir}} && trunk build --release
    cargo run --release -p graph-api -- --assets-dir {{dist}}

#
# ── build / quality ──
#

# Build everything in release mode.
[group('build')]
build:
    cargo build --release --workspace

# Format the workspace.
[group('build')]
fmt:
    cargo fmt --all

# Lint with clippy, deny warnings.
[group('build')]
clippy:
    cargo clippy --all-targets -- -D warnings

#
# ── util ──
#

# Reindex vault-search manually.
[group('util')]
reindex VAULT_ROOT=`echo ${VAULT_ROOT:-.}`:
    cargo run --release -p vault-search -- --vault {{ VAULT_ROOT }} --rebuild --port 0

# Tail the detached graph-compute log (written by `just dev-up`'s native worker).
[group('util')]
logs:
    tail -f /tmp/graph-compute.log

# Kill every stray jump-cannon process from a crashed run — mirrors dev-up's
# teardown (graph-api, vault-search, graph-compute, trunk watch, cargo-watch).
[group('util')]
[doc('Kill every stray jump-cannon process from a crashed run')]
[confirm("kill all stray graph-api / vault-search / graph-compute / trunk processes?")]
kill:
    -pkill -f 'graph-api'
    -pkill -f 'vault-search'
    -pkill -f 'graph-compute'
    -pkill -f 'trunk watch'
    -pkill -f 'cargo-watch.*graph-api'

# Nuke the trunk dist to force a full rebuild.
[group('util')]
[confirm("rm -rf the WASM dist (forces a full rebuild)?")]
clean-wasm:
    rm -rf {{dist}}
