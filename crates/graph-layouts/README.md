# Rust Graph Layouts

A WebAssembly library for efficient graph layout algorithms. This library provides a collection of layout algorithms for graph visualization, optimized for performance and memory efficiency.

## Features

- Core graph data structures for nodes and edges
- Multiple layout algorithms:
  - fCoSE (Force-directed Compound Spring Embedder)
  - More algorithms coming soon (Dagre, KLay, etc.)
- WASM bindings for seamless JavaScript integration
- JSON serialization support
- Metadata support for nodes and edges

## Building

This library uses `wasm-pack` to build WebAssembly modules. To build the project:

1. Install wasm-pack if you haven't already:
```bash
cargo install wasm-pack
```

2. Build the library:
```bash
wasm-pack build --target web
```

This will generate the `pkg` directory containing the WebAssembly module and JavaScript bindings.

## Usage

### In JavaScript/TypeScript

```javascript
import init, { LayoutManager } from 'rust-graph-layouts';

// Initialize the WASM module
await init();

// Create a new layout manager
const manager = new LayoutManager();

// Add nodes and edges
manager.add_node("1", null, null);  // Position will be set by layout
manager.add_node("2", null, null);
manager.add_edge("e1", "1", "2");

// Layouts run through the `StaticLayout` / `PhysicsLayout` registry rather
// than per-algorithm WASM entry points. See the renderer's layout registry
// and the `LayoutManager` GPU-force bindings for the live integration path.
```

### In Rust

Each algorithm implements the `StaticLayout` trait: a one-shot `solve(&settings,
&graph)` that returns positions packed as `[x0,y0,z0, x1,y1,z1, …]` in the
graph's id-sorted node order.

```rust
use rust_graph_layouts::{Graph, Node, Edge, FcoseLayout, FcoseSettings};
use rust_graph_layouts::StaticLayout;

// Create a new graph
let mut graph = Graph::new();

// Add nodes and edges
graph.add_node(Node::new("1"));
graph.add_node(Node::new("2"));
graph.add_edge(Edge::new("e1", "1", "2"));

// Configure and solve — `packed` is [x,y,z] per node, id-sorted.
let settings = FcoseSettings::default();
let packed = FcoseLayout::solve(&settings, &graph).unwrap();
println!("Node 1 position: ({}, {})", packed[0], packed[1]);
```

## Layout Algorithms

### fCoSE (Force-directed Compound Spring Embedder)

The fCoSE algorithm is a force-directed layout algorithm optimized for compound graphs. It uses:
- Node-to-node repulsion
- Edge-based attraction
- Overlap removal
- Simulated annealing for optimization

Configuration options:
- `quality`: Layout quality level ("draft", "default", "proof")
- `node_repulsion`: Repulsion force between nodes
- `ideal_edge_length`: Preferred length of edges
- `node_overlap`: Percentage of allowed node overlap (0-100)

## License

MIT License
