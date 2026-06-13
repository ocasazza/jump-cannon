# Test subcommands — `just test <target> [arg]`.
#
#   just test                  full suite (native cargo + Rust browser smoke)
#   just test cargo            native unit + integration (incl. regression.rs + fuzz.rs at default volume)
#   just test fuzz [CASES]     property-based fuzz: graph-layouts + graph-compute engines (default 10000)
#   just test bench            criterion benches across layouts; HTML in target/criterion/
#   just test canary [URL]     live-cluster gRPC smoke (default http://[::1]:50051)
#   just test geometric        geometric engine: solved-case canary + regression golden + perf
#   just test geometric-golden regenerate the geometric regression golden (intentional baseline bump)
#   just test browser-rust     Rust-driven browser smoke (crates/test-browser via CDP)
#
# `just test --list` shows these as real recipes (completion, per-recipe docs).

# Submodule recipes default to the module-file dir; pin to the repo root so
# cargo / nix run from where they expect.
set working-directory := '..'

# Full suite: native cargo tests + the Rust browser smoke test. (Default recipe,
# so bare `just test` runs everything — same as the old `just test all`.)
[group('test')]
[doc('Full suite: native cargo tests + Rust browser smoke (the default)')]
all: cargo browser-rust

# Native unit + integration tests across the workspace.
[group('test')]
cargo:
    cargo test --workspace

# Property-based fuzz over graph-layouts layouts + graph-compute engines.
# CASES sets PROPTEST_CASES (default 10000).
[group('test')]
[doc('Property-based fuzz: graph-layouts + graph-compute engines (CASES, default 10000)')]
fuzz cases='10000':
    PROPTEST_CASES={{cases}} cargo test -p graph-layouts --test fuzz --release
    PROPTEST_CASES={{cases}} cargo test -p graph-compute --test fuzz --release

# Criterion benches across static layouts; HTML report in target/criterion/.
[group('test')]
bench:
    cargo run --release -p graph-layouts --example bench_static_layouts -- --bench

# Live-cluster gRPC smoke test. URL defaults to the local worker.
[group('test')]
canary url='http://[::1]:50051':
    GRAPH_COMPUTE_CANARY_URL={{url}} cargo test -p graph-compute --test canary -- --nocapture

# Geometric engine: solved-case canary + regression golden + perf.
[group('test')]
geometric:
    cargo test -p graph-compute --test geometric_solver -- --nocapture

# Regenerate the geometric regression golden (intentional baseline bump).
[group('test')]
geometric-golden:
    UPDATE_GEOMETRIC_GOLDEN=1 cargo test -p graph-compute --test geometric_solver regression_golden_master -- --nocapture

# Rust-driven browser smoke test (crates/test-browser via CDP). No local trunk
# build needed — the flake wrapper serves the nix-built app-web dist by default
# (override with ASSETS_DIR=...). Output: target/test-browser-rust/{boot.png,report.json}.
[group('test')]
[doc('Rust-driven browser smoke test (crates/test-browser via CDP)')]
browser-rust:
    nix run .#test-browser-rust
