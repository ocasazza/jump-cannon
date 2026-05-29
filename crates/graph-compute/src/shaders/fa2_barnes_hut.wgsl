// ForceAtlas2 — Barnes-Hut GPU compute kernel.
//
// Drop-in replacement for the repulsion half of `force_atlas2.wgsl`: the
// attraction (linear edge scan), gravity, and Euler integration are kept
// byte-for-byte identical so the converged layout matches the brute-force
// engine — this is a *pure speedup*, not a new look (docs/layout-algorithms.md
// §1, "Visual: identical to FA2"). Only the O(n^2) all-pairs repulsion loop is
// swapped for an O(log n) stackless rope walk over a host-built octree.
//
// Octree contract follows Burtscher & Pingali's tree-based Barnes-Hut n-body
// scheme (2011) and the WGSL buffer/traversal layout pioneered by GraphWaGu
// (harp-lab, IEEE PacificVis 2022): the only direct wgpu+WGSL precedent for our
// stack. The per-node `OctNode` layout and the next/skip "rope" mirror the
// host-built tree in `graph-layouts/.../shaders/octree.wgsl` +
// `gpu_force.rs::OctreeBuild`, ported here so this engine is self-contained.
//
// References:
//   - Burtscher & Pingali, "An Efficient CUDA Implementation of the Tree-based
//     Barnes-Hut n-Body Algorithm" (2011).
//   - Tom Bednall / Sidharth Kumar et al., "GraphWaGu" (IEEE PacificVis 2022) —
//     WebGPU/WGSL force-directed layout on CSR buffers w/ WGSL Barnes-Hut.
//
// Bindings (group 0) — superset of force_atlas2.wgsl; bindings 0..5 are
// identical so the host buffer-build code is shared, binding 6 adds the tree:
//   0  positions     read_write   array<vec4<f32>>  (xyz, _pad)
//   1  velocities    read_write   array<vec4<f32>>  (xyz, _pad)
//   2  edges         read         array<vec2<u32>>  (src, tgt)
//   3  edge_weights  read         array<f32>
//   4  params        uniform      Fa2Params
//   5  degrees       read         array<u32>
//   6  oct_nodes     read         array<OctNode>    (host-built Barnes-Hut tree)

struct Fa2Params {
    n_nodes: u32,
    n_edges: u32,
    gravity: f32,
    scaling_ratio: f32,
    edge_weight_influence: f32,
    jitter_tolerance: f32,
    time_step: f32,
    strong_gravity: u32,
    lin_log_mode: u32,
    prevent_overlap: u32,
    // Barnes-Hut acceptance criterion (treat a cell as a point mass when
    // s/d < theta). Burtscher & Pingali §4.5: ~0.5..1.0, 0.7 a common sweet
    // spot. Replaces the first padding lane of the brute-force params.
    theta: f32,
    // Number of populated octree slots (<= 2N+8). 0 => empty tree => the walk
    // exits immediately and repulsion is skipped this step.
    n_octree: u32,
};

// Per-octree-node layout — must match `OctNodeRaw` on the Rust side:
//   pos_size: vec4 = (center.xyz, half_extent)
//   com_mass: vec4 = (com.xyz, total_mass)
//   links:    vec4<u32> = (body_idx_or_FFFFFFFF, next_idx, skip_idx, child_count)
struct OctNode {
    pos_size: vec4<f32>,
    com_mass: vec4<f32>,
    links:    vec4<u32>,
};

@group(0) @binding(0) var<storage, read_write> positions:    array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> velocities:   array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       edges:        array<vec2<u32>>;
@group(0) @binding(3) var<storage, read>       edge_weights: array<f32>;
@group(0) @binding(4) var<uniform>             params:       Fa2Params;
@group(0) @binding(5) var<storage, read>       degrees:      array<u32>;
@group(0) @binding(6) var<storage, read>       oct_nodes:    array<OctNode>;

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;

@compute @workgroup_size(64)
fn fa2_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos_i = positions[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    var force = vec3<f32>(0.0, 0.0, 0.0);

    // ---- Repulsion (Barnes-Hut octree, stackless rope walk) ---------------
    // The COM `mass_n` stored in the tree is the degree-weighted mass
    // (sum of (deg+1) over the subtree's bodies), so the accepted-cell force
    // is exactly the brute-force sum `scaling_ratio * deg_i * (deg_j+1) / r^2`
    // aggregated — i.e. the same FA2 repulsion law, summed over cells instead
    // of individual nodes. See the host build in the Rust engine.
    if (params.n_octree > 0u) {
        let theta2 = params.theta * params.theta;
        var idx: u32 = 0u;
        // Paranoia cap: a well-formed tree terminates via OCT_END, but bound
        // the loop so any malformed rope is a hang-resistant bug rather than
        // an infinite GPU loop.
        let walk_cap = max(params.n_octree * 4u, 16u);
        var walk: u32 = 0u;
        loop {
            if (idx == OCT_END) { break; }
            if (walk >= walk_cap) { break; }
            walk = walk + 1u;
            let node = oct_nodes[idx];
            let body = node.links.x;
            let com = node.com_mass.xyz;
            let mass_n = node.com_mass.w;   // degree-weighted subtree mass
            let half = node.pos_size.w;
            let s = half * 2.0;             // cell side length
            // FA2 repulsion is directed *away* from the other body, i.e.
            // along (pos_i - com), matching force_atlas2.wgsl's `d = pos_i - pos_j`.
            let d = pos_i - com;
            let r2 = dot(d, d);

            if (body != OCT_BODY_INTERNAL) {
                // Leaf — apply directly, skipping self.
                if (body != i && mass_n > 0.0) {
                    let r2c = max(r2, 1.0e-4);
                    force = force + d * (params.scaling_ratio * deg_i * mass_n / r2c);
                }
                idx = node.links.z; // skip = next-sibling-or-uncle
                continue;
            }
            // Internal — Barnes-Hut acceptance: treat as a single point mass
            // when (s/d)^2 < theta^2 (square both sides to avoid the sqrt).
            if (mass_n > 0.0 && r2 > 0.0 && (s * s) < (theta2 * r2)) {
                let r2c = max(r2, 1.0e-4);
                force = force + d * (params.scaling_ratio * deg_i * mass_n / r2c);
                idx = node.links.z; // accepted => skip subtree
            } else {
                idx = node.links.y; // descend into first child
            }
        }
    }

    // ---- Attraction (linear scan over edges) — identical to force_atlas2.wgsl
    let ewi = params.edge_weight_influence;
    for (var e: u32 = 0u; e < params.n_edges; e = e + 1u) {
        let edge = edges[e];
        var other: u32 = 0u;
        var touched = false;
        if (edge.x == i) {
            other = edge.y;
            touched = true;
        } else if (edge.y == i) {
            other = edge.x;
            touched = true;
        }
        if (!touched) { continue; }
        let pos_o = positions[other].xyz;
        let d = pos_o - pos_i;
        let r = max(length(d), 1.0e-3);
        let w = pow(max(edge_weights[e], 1.0e-6), ewi);
        var att = w;
        if (params.lin_log_mode != 0u) {
            att = w * log(1.0 + r) / r;
        }
        force = force + d * att;
    }

    // ---- Gravity — identical to force_atlas2.wgsl -------------------------
    let r0 = max(length(pos_i), 1.0e-3);
    if (params.strong_gravity != 0u) {
        force = force - pos_i * (params.gravity * deg_i);
    } else {
        force = force - (pos_i / r0) * (params.gravity * deg_i);
    }

    // ---- Integrate — identical to force_atlas2.wgsl ----------------------
    var vel = velocities[i].xyz;
    vel = (vel + force) * 0.5;
    let new_pos = pos_i + vel * params.time_step;

    velocities[i] = vec4<f32>(vel, 0.0);
    positions[i]  = vec4<f32>(new_pos, 0.0);
}
