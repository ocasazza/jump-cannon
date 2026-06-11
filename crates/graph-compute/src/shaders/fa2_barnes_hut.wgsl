// ForceAtlas2 — Barnes-Hut GPU compute kernel.
//
// Drop-in replacement for the repulsion half of `force_atlas2.wgsl`: the
// attraction (linear edge scan), gravity, and the adaptive-speed displacement
// (fa2_force / fa2_apply split — see force_atlas2.wgsl's header for the paper
// citation and the divergence post-mortem) are kept byte-for-byte identical so
// the converged layout matches the brute-force engine — this is a *pure
// speedup*, not a new look (docs/layout-algorithms.md §1, "Visual: identical
// to FA2"). Only the O(n^2) all-pairs repulsion loop is swapped for an
// O(log n) stackless rope walk over a host-built octree.
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
// Bindings (group 0) — superset of force_atlas2.wgsl; the shared bindings keep
// the same slots so the host buffer-build code is shared, binding 6 adds the
// tree and 8/9 the force/stats pair the adaptive-speed split needs:
//   0  positions     read         array<vec4<f32>>  (xyz, _pad) — step input
//   1  old_force     read_write   array<vec4<f32>>  F_{t-1} (xyz, _pad)
//   2  edges         read         array<vec2<u32>>  (src, tgt)
//   3  edge_weights  read         array<f32>
//   4  params        uniform      Fa2Params
//   5  degrees       read         array<u32>
//   6  oct_nodes     read         array<OctNode>    (host-built Barnes-Hut tree)
//   7  positions_out read_write   array<vec4<f32>>  — step output (ping-pong)
//   8  force         read_write   array<vec4<f32>>  F_t (xyz, _pad)
//   9  stats         read_write   array<vec2<f32>>  (mass·swing, mass·traction)

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
    // Global adaptive speed s(G), recomputed by the host between fa2_force and
    // fa2_apply each step (see force_atlas2.wgsl).
    speed: f32,
    // Per-step displacement cap (the paper's k_smax). 0 disables.
    max_displacement: f32,
    _pad0: u32,
    _pad1: u32,
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

// Determinism (timeline P2): positions are DOUBLE-BUFFERED — read start-of-step
// from `positions` (read-only), write only this thread's node to
// `positions_out`. Reading + writing one read_write buffer races neighbour
// reads against self-writes (intra-dispatch read-after-write), which made the
// same seed diverge run-to-run on Metal. Ping-pong (host flips in/out per step)
// removes the hazard and makes the step a clean Jacobi update.
@group(0) @binding(0) var<storage, read>       positions:     array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> positions_out: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> old_force:    array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       edges:        array<vec2<u32>>;
@group(0) @binding(3) var<storage, read>       edge_weights: array<f32>;
@group(0) @binding(4) var<uniform>             params:       Fa2Params;
@group(0) @binding(5) var<storage, read>       degrees:      array<u32>;
@group(0) @binding(6) var<storage, read>       oct_nodes:    array<OctNode>;
@group(0) @binding(8) var<storage, read_write> force:        array<vec4<f32>>;
@group(0) @binding(9) var<storage, read_write> stats:        array<vec2<f32>>;

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;

@compute @workgroup_size(64)
fn fa2_force(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos_i = positions[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    var f = vec3<f32>(0.0, 0.0, 0.0);

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
                    f = f + d * (params.scaling_ratio * deg_i * mass_n / r2c);
                }
                idx = node.links.z; // skip = next-sibling-or-uncle
                continue;
            }
            // Internal — Barnes-Hut acceptance: treat as a single point mass
            // when (s/d)^2 < theta^2 (square both sides to avoid the sqrt).
            if (mass_n > 0.0 && r2 > 0.0 && (s * s) < (theta2 * r2)) {
                let r2c = max(r2, 1.0e-4);
                f = f + d * (params.scaling_ratio * deg_i * mass_n / r2c);
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
        f = f + d * att;
    }

    // ---- Gravity — identical to force_atlas2.wgsl -------------------------
    let r0 = max(length(pos_i), 1.0e-3);
    if (params.strong_gravity != 0u) {
        f = f - pos_i * (params.gravity * deg_i);
    } else {
        f = f - (pos_i / r0) * (params.gravity * deg_i);
    }

    // ---- Swing/traction stats — identical to force_atlas2.wgsl ------------
    let old = old_force[i].xyz;
    force[i] = vec4<f32>(f, 0.0);
    stats[i] = vec2<f32>(
        deg_i * length(old - f),
        deg_i * 0.5 * length(old + f),
    );
}

// Adaptive-speed displacement — identical math to force_atlas2.wgsl's
// fa2_apply, except positions are double-buffered (Jacobi ping-pong, see the
// determinism note above): read positions, write positions_out.
@compute @workgroup_size(64)
fn fa2_apply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let f = force[i].xyz;
    let old = old_force[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    let swinging = deg_i * length(old - f);
    var factor = params.speed / (1.0 + sqrt(params.speed * swinging));

    let df = length(f);
    if (params.max_displacement > 0.0 && df > 1.0e-9) {
        factor = min(factor, params.max_displacement / df);
    }

    let new_pos = positions[i].xyz + f * (factor * params.time_step);

    positions_out[i] = vec4<f32>(new_pos, 0.0);
    old_force[i] = vec4<f32>(f, 0.0);
}
