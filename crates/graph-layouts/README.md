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

// Configure and apply fCoSE layout
const options = {
  base: {
    padding: 30
  },
  quality: "default",
  node_repulsion: 4500,
  ideal_edge_length: 50,
  node_overlap: 10
};

// Apply layout and get the result
const result = manager.apply_fcose_layout(JSON.stringify(options));
const graph = JSON.parse(result);

// Access node positions
console.log(graph.nodes["1"].position);  // [x, y] coordinates
```

### In Rust

```rust
use rust_graph_layouts::{Graph, Node, Edge, FcoseLayoutEngine, FcoseOptions};

// Create a new graph
let mut graph = Graph::new();

// Add nodes and edges
graph.add_node(Node::new("1"));
graph.add_node(Node::new("2"));
graph.add_edge(Edge::new("e1", "1", "2"));

// Configure and apply layout
let options = FcoseOptions::default();
let engine = FcoseLayoutEngine::new(options);
engine.apply_layout(&mut graph).unwrap();

// Access node positions
if let Some(pos) = graph.nodes.get("1").unwrap().position {
    println!("Node 1 position: ({}, {})", pos.0, pos.1);
}
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
