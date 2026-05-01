# jump-cannon — task runner. `just <recipe>` to run.
# `just` lists all recipes.

set dotenv-load := true
set positional-arguments

# Default: show recipes
default:
    @just --list

# All-in-one dev server.
# - Auto-builds WASM bundle if missing (first-pull case).
# - Builds vault-search if missing (idempotent).
# - Watches Rust API sources; rebuilds + relaunches graph-api on changes.
# - Serves /assets from disk: JS/CSS/HTML edits show up on browser refresh.
# - Reads VAULT_ROOT from .env (or current cwd if unset).
#
# For active development of the graph-renderer crate, run `just watch-wasm`
# in a second terminal — that rebuilds the WASM bundle on .rs/.wgsl changes.
dev:
    @# Rebuild WASM if missing OR if any graph-renderer / graph-layouts Rust
    @# / WGSL source is newer than the bundle. Avoids "I pulled but the WASM
    @# is still the old broken one" footgun.
    @PKG=crates/graph-renderer/assets/pkg/graph_renderer.js; \
     if [ ! -f $PKG ] || [ -n "$(find crates/graph-renderer/src crates/graph-layouts/src -newer $PKG -name '*.rs' -o -newer $PKG -name '*.wgsl' -o -newer $PKG -name '*.toml' 2>/dev/null | head -1)" ]; then \
        echo "→ rebuilding WASM renderer bundle (Rust/WGSL changed or missing)…"; \
        wasm-pack build crates/graph-renderer --target web --out-dir assets/pkg --release -- --features wasm; \
     else \
        echo "→ WASM bundle up to date (run 'just wasm' to force rebuild)"; \
     fi
    cargo build --release -p vault-search
    cargo watch \
      -w crates/graph-api \
      -w crates/vault-data \
      -w crates/vault-links \
      -w crates/graph-metrics \
      -x 'run -p graph-api -- --assets-dir crates/graph-renderer/assets'

# Force-rebuild the WASM renderer bundle. Run when graph-renderer src changes.
wasm:
    wasm-pack build crates/graph-renderer --target web --out-dir assets/pkg --release -- --features wasm

# Watch graph-renderer src and rebuild WASM on every change. Run alongside
# `just dev` in a second terminal for live iteration on the renderer.
watch-wasm:
    cargo watch -w crates/graph-renderer/src -s 'wasm-pack build crates/graph-renderer --target web --out-dir assets/pkg --dev -- --features wasm'

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

# Nuke the WASM bundle to force a full rebuild on next `just dev`.
clean-wasm:
    rm -rf crates/graph-renderer/assets/pkg/graph_renderer*

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
