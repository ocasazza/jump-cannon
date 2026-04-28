#!/bin/bash
set -e

# Check if wasm-bindgen-cli is installed
if ! command -v wasm-bindgen &> /dev/null; then
    echo "Installing wasm-bindgen-cli..."
    cargo install wasm-bindgen-cli
fi

# Check if wasm32-unknown-unknown target is installed
if ! rustup target list | grep "wasm32-unknown-unknown (installed)" &> /dev/null; then
    echo "Adding wasm32-unknown-unknown target..."
    rustup target add wasm32-unknown-unknown
fi

# Build the project for wasm
echo "Building for wasm32-unknown-unknown..."
cargo build --release --target wasm32-unknown-unknown

# Use wasm-bindgen to generate the JavaScript bindings
echo "Generating JavaScript bindings..."
wasm-bindgen --out-dir pkg --target web target/wasm32-unknown-unknown/release/rust-graph-renderer.wasm

echo "WASM build complete! The output is in the 'pkg' directory."
echo "You can serve it with a local pkg server, for example:"
echo "python3 -m http.server"
