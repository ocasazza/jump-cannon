# Bevy 2D Shapes Example

This project demonstrates rendering simple 2D shapes using the Bevy game engine. It's based on the official Bevy 2D shapes example and has been adapted to support both native and WebAssembly (WASM) builds.

## Features

- Displays various 2D shapes with different colors
- Supports both native and WASM builds

## Running Natively

To run the application natively:

```bash
cargo run
```

## Building for WebAssembly

To build for WebAssembly and run in a browser:

1. Run the build script:

```bash
./build_wasm.sh
```

This script will:
- Install the wasm-bindgen-cli if not already installed
- Add the wasm32-unknown-unknown target if not already added
- Build the project for WASM
- Generate the JavaScript bindings

2. Serve the web directory:

```bash
cd web
python3 -m http.server
```

3. Open your browser and navigate to http://localhost:8000

## Project Structure

- `src/main.rs` - The main application code with WASM support
- `web/index.html` - HTML file for the WASM build
- `build_wasm.sh` - Build script for WASM

## Dependencies

- Bevy 0.16.0
- wasm-bindgen (for WASM builds)
