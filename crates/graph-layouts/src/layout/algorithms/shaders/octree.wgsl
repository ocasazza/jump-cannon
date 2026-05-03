// Barnes-Hut octree build kernels.
//
// Followed Burtscher & Pingali's 6-kernel CUDA pipeline (2011), adapted to
// WGSL. WGSL has no f32 atomics, so the COM aggregation is *top-down*: each
// thread is the sole writer for its own subtree, which avoids atomics
// entirely. The build itself is bottom-up from morton-sorted leaves.
//
// v1 status: these kernels are *not currently dispatched* — the host (Rust)
// builds the octree CPU-side and uploads it to `oct_nodes`. This file
// exists so the GPU build is one Rust change away (just call mk_pipeline +
// dispatch) and the WGSL stays in lockstep with the host data layout.
//
// References:
//   - Burtscher & Pingali, "An Efficient CUDA Implementation of the
//     Tree-based Barnes Hut n-Body Algorithm" (2011)
//   - Petrescu et al., "Stochastic Barnes-Hut on GPU" (arXiv:2506.02219, 2025)
//     (relevant for the stackless-rope traversal in force.wgsl)

struct OctParams {
    n_nodes: u32,         // body count (leaves)
    n_octree: u32,        // total octree slots in use (≤ 2N)
    theta: f32,           // Barnes-Hut acceptance criterion (s/d < theta)
    _pad0: f32,
    world_min: vec3<f32>,
    _pad1: f32,
    world_max: vec3<f32>,
    _pad2: f32,
};

// Per-octree-node layout (must match `OctNodeRaw` on the Rust side):
//   pos_size:  vec4<f32>  xyz = node center, w = half-extent
//   com_mass:  vec4<f32>  xyz = center of mass, w = total mass
//   meta:      vec4<u32>  x = body_idx (0xFFFFFFFF for internal), y = next_idx,
//                         z = skip_idx, w = child_count
//
// Rope-traversal contract (consumed by force.wgsl):
//   - To start a walk: idx = 0 (root).
//   - At each step: if `should_descend(node)` is FALSE (leaf, empty, or
//     s/d < theta), apply the force from this node's COM and jump to
//     `meta.z` (skip_idx).
//   - Otherwise descend: jump to `meta.y` (next_idx, the first child in
//     DFS order).
//   - Sentinel `0xFFFFFFFFu` ends the walk.
struct OctNode {
    pos_size: vec4<f32>,
    com_mass: vec4<f32>,
    links:    vec4<u32>,
};

@group(2) @binding(0) var<uniform> oct_params: OctParams;
@group(2) @binding(1) var<storage, read_write> oct_nodes: array<OctNode>;
@group(2) @binding(2) var<storage, read_write> oct_bbox: array<vec4<f32>>;
// oct_bbox layout: [0] = min.xyz/0, [1] = max.xyz/0, used by bbox_reduce.

// ---- 1. Bounding-box reduce ------------------------------------------------
//
// Per-workgroup partial reduction; final pass writes into oct_bbox[0..2].
// Stub for v1 — host computes bbox CPU-side. Kept here so the kernel is one
// dispatch away once we move bbox to GPU.
var<workgroup> wg_min: array<vec3<f32>, 64>;
var<workgroup> wg_max: array<vec3<f32>, 64>;

@compute @workgroup_size(64)
fn bbox_reduce(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    // No-op stub; oct_bbox is populated by the host in v1.
    if (gid.x == 0u && lid.x == 0u) {
        // touch storage so binding stays live
        oct_bbox[0] = oct_bbox[0];
    }
}

// ---- 2. Morton-key assign --------------------------------------------------
//
// quantizes pos to a 10-bit-per-axis grid and interleaves bits into a 30-bit
// Z-order key. Stub for v1 — host sorts by morton key on the CPU.
@compute @workgroup_size(64)
fn morton_assign(@builtin(global_invocation_id) gid: vec3<u32>) {
    // intentionally empty; host-side morton sort in v1.
    let _ = gid.x;
}

// ---- 3. Octree build -------------------------------------------------------
//
// Bottom-up linear octree from morton-sorted leaves. Stub for v1.
@compute @workgroup_size(64)
fn octree_build(@builtin(global_invocation_id) gid: vec3<u32>) {
    let _ = gid.x;
}

// ---- 4. COM aggregate ------------------------------------------------------
//
// Top-down per-subtree: each thread owns one subtree root and walks its
// descendants single-writer. Avoids the f32-atomics gap in WGSL. Stub for
// v1 (host computes COM during the CPU build).
@compute @workgroup_size(64)
fn com_aggregate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let _ = gid.x;
}
