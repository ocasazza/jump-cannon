//! GPU graph analytics — hardware-agnostic (wgpu → Metal/Vulkan/DX12) one-shot
//! algorithms over the symmetrized CSR, distinct from the layout `engines`.
//!
//! These produce per-node scalars (ranks, components, labels), not positions, so
//! they're free functions rather than `LayoutEngine`s. They replace the
//! NVIDIA-only cuGraph diagnostics in the lavender notebooks. The CPU
//! references live in `crates/graph-metrics` and `engines::geometric`.
//!
//! Today: PageRank (pull-style SpMV power iteration), connected components
//! (min-label propagation), and single-source BFS (distance relaxation). Next:
//! label propagation — all the same CSR gather with a different semiring.

mod bfs;
mod connected_components;
mod distributed;
mod pagerank;
mod spmv;

/// 2-D workgroup dispatch dims for `total_workgroups`, tiling into Y when the
/// count exceeds wgpu's hard 65535 per-dimension cap (Metal/Vulkan won't go
/// higher). A 1-D dispatch caps at 65535 × workgroup_size threads — only ~4.2M
/// nodes at wg=64 — so kernels that want the 10–20M+ regime tile here, and the
/// WGSL recovers the linear index as `gid.y * num_workgroups.x * wg_size + gid.x`.
pub(crate) fn workgroup_dims_2d(total_workgroups: u32) -> (u32, u32, u32) {
    const MAX_PER_DIM: u32 = 65535;
    let total = total_workgroups.max(1);
    if total <= MAX_PER_DIM {
        (total, 1, 1)
    } else {
        (MAX_PER_DIM, total.div_ceil(MAX_PER_DIM), 1)
    }
}

pub use bfs::{cpu_bfs, gpu_bfs, UNREACHABLE};
pub use connected_components::{cpu_connected_components, gpu_connected_components};
pub use distributed::{distributed_pagerank, distributed_pagerank_gpu};
pub use pagerank::{cpu_pagerank, gpu_pagerank};
pub use spmv::{cpu_spmv, gpu_spmv, gpu_spmv_f16, gpu_spmv_hybrid, WeightedCsr};
