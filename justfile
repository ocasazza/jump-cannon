# jump-cannon — task runner. `just <recipe>` to run.
# `just` lists all recipes.

set dotenv-load := true
set positional-arguments

# Default: show recipes
default:
    @just --list

# All-in-one dev server.
# - Builds vault-search if missing (idempotent — cargo skips if up to date).
# - Watches Rust sources; rebuilds + relaunches graph-api on changes.
# - Serves /assets from disk, so JS/CSS/HTML edits show up on browser refresh.
# - Reads VAULT_ROOT from .env (or current cwd if unset).
dev:
    cargo build --release -p vault-search
    cargo watch \
      -w crates/graph-api \
      -w crates/vault-data \
      -w crates/vault-links \
      -w crates/graph-metrics \
      -x 'run -p graph-api -- --assets-dir crates/graph-renderer/assets'

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

# Reindex vault-search manually (it auto-spawns from graph-api but you can
# also exercise it standalone).
reindex VAULT_ROOT=`echo ${VAULT_ROOT:-.}`:
    cargo run --release -p vault-search -- --vault {{ VAULT_ROOT }} --rebuild --port 0

# Tail graph-api logs (only useful if launched detached).
logs:
    tail -f /tmp/graph-api.log

# Kill any stray graph-api / vault-search processes from previous runs.
kill:
    -pkill -f 'graph-api'
    -pkill -f 'vault-search'
