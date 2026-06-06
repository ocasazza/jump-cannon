//! GPU graph analytics — hardware-agnostic (wgpu → Metal/Vulkan/DX12) one-shot
//! algorithms over the symmetrized CSR, distinct from the layout `engines`.
//!
//! These produce per-node scalars (ranks, components, labels), not positions, so
//! they're free functions rather than `LayoutEngine`s. They replace the
//! NVIDIA-only cuGraph diagnostics in the lavender notebooks. The CPU
//! references live in `crates/graph-metrics` and `engines::geometric`.
//!
//! Today: PageRank (pull-style SpMV power iteration). Next: connected components
//! (min-label SpMV), label propagation, BFS — all the same CSR gather with a
//! different semiring.

mod pagerank;

pub use pagerank::{cpu_pagerank, gpu_pagerank};
