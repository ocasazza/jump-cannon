// ForceAtlas2 — GPU compute kernel.
//
// brute-force; replace with Barnes-Hut for >50k nodes.
//
// Single entry point. One thread per node:
//   - O(n^2) repulsion against every other node
//   - linear scan over the CSR-style edge list for attraction
//   - origin gravity (linear or "strong")
//   - simple velocity damping + Euler integration
//
// Bindings (group 0):
//   0  positions     read_write   array<vec4<f32>>  (xyz, _pad)
//   1  velocities    read_write   array<vec4<f32>>  (xyz, _pad)
//   2  edges         read         array<vec2<u32>>  (src, tgt)
//   3  edge_weights  read         array<f32>
//   4  params        uniform      Fa2Params
//   5  degrees       read         array<u32>

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
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> positions:    array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> velocities:   array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       edges:        array<vec2<u32>>;
@group(0) @binding(3) var<storage, read>       edge_weights: array<f32>;
@group(0) @binding(4) var<uniform>             params:       Fa2Params;
@group(0) @binding(5) var<storage, read>       degrees:      array<u32>;

@compute @workgroup_size(64)
fn fa2_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos_i = positions[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    var force = vec3<f32>(0.0, 0.0, 0.0);

    // ---- Repulsion (brute-force O(n^2)) -----------------------------------
    for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
        if (j == i) { continue; }
        let pos_j = positions[j].xyz;
        let d = pos_i - pos_j;
        let r2 = max(dot(d, d), 1.0e-4);
        let deg_j = f32(degrees[j]) + 1.0;
        // Coulomb-style (deg_i+1)*(deg_j+1) / r^2 — applied along d.
        let coeff = params.scaling_ratio * deg_i * deg_j / r2;
        force = force + d * coeff;
    }

    // ---- Attraction (linear scan over edges; first-cut, O(m) per node) ----
    // For dense graphs this is the obvious next thing to optimize: switch
    // to a CSR-per-node iteration like gpu_force.rs (edge_offsets/neighbors).
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

    // ---- Gravity ----------------------------------------------------------
    let r0 = max(length(pos_i), 1.0e-3);
    if (params.strong_gravity != 0u) {
        force = force - pos_i * (params.gravity * deg_i);
    } else {
        force = force - (pos_i / r0) * (params.gravity * deg_i);
    }

    // ---- Integrate (simple damping; full FA2 swing/jitter TBD) ------------
    var vel = velocities[i].xyz;
    vel = (vel + force) * 0.5;
    let new_pos = pos_i + vel * params.time_step;

    velocities[i] = vec4<f32>(vel, 0.0);
    positions[i]  = vec4<f32>(new_pos, 0.0);
}
