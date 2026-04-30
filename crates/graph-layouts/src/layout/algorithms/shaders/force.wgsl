// 3D force-directed layout compute shader.
//
// One dispatch = one simulation step. Repulsion is naive O(n^2); spring forces
// are O(degree) per node thanks to a CSR-style adjacency
// (edge_offsets / edge_neighbors). Integration is semi-implicit Euler with
// velocity damping (effectively velocity Verlet w/o the half-step trick).
//
// Bindings:
//   @group(0) @binding(0) positions_in       (read)
//   @group(0) @binding(1) positions_out      (read_write)
//   @group(0) @binding(2) velocities         (read_write)
//   @group(0) @binding(3) edge_offsets       (read)   length n+1
//   @group(0) @binding(4) edge_neighbors     (read)   length 2*m
//   @group(0) @binding(5) params             (uniform)

struct SimParams {
    repulsion: f32,
    spring_k: f32,
    spring_len: f32,
    gravity: f32,
    damping: f32,
    dt: f32,
    cursor_radius: f32,
    cursor_strength: f32,
    cursor_pos: vec3<f32>,
    n_nodes: u32,
    n_edges: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read>       positions_in:    array<vec3<f32>>;
@group(0) @binding(1) var<storage, read_write> positions_out:   array<vec3<f32>>;
@group(0) @binding(2) var<storage, read_write> velocities:      array<vec3<f32>>;
@group(0) @binding(3) var<storage, read>       edge_offsets:    array<u32>;
@group(0) @binding(4) var<storage, read>       edge_neighbors:  array<u32>;
@group(0) @binding(5) var<uniform>             params:          SimParams;

@compute @workgroup_size(64)
fn force_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos = positions_in[i];
    var vel = velocities[i];
    var force = vec3<f32>(0.0, 0.0, 0.0);

    // ---- Repulsion (O(n^2)) -------------------------------------------------
    // Coulomb-ish: F = k * d / |d|^2  (so 1/|d| falloff in force magnitude).
    for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
        if (j == i) {
            continue;
        }
        let d = pos - positions_in[j];
        let dist2 = max(dot(d, d), 0.01);
        force = force + d * (params.repulsion / dist2);
    }

    // ---- Springs (O(degree)) -----------------------------------------------
    let start = edge_offsets[i];
    let end   = edge_offsets[i + 1u];
    for (var k: u32 = start; k < end; k = k + 1u) {
        let other = edge_neighbors[k];
        let d = positions_in[other] - pos;
        let dist = max(length(d), 0.01);
        let stretch = dist - params.spring_len;
        force = force + (d / dist) * (params.spring_k * stretch);
    }

    // ---- Gravity towards origin --------------------------------------------
    force = force - pos * params.gravity;

    // ---- Cursor force (radial, falloff to 0 at radius) ---------------------
    if (params.cursor_radius > 0.0) {
        let cd = pos - params.cursor_pos;
        let cdist = max(length(cd), 0.01);
        if (cdist < params.cursor_radius) {
            let falloff = 1.0 - (cdist / params.cursor_radius);
            force = force + (cd / cdist) * (params.cursor_strength * falloff);
        }
    }

    // ---- Integrate ----------------------------------------------------------
    vel = (vel + force * params.dt) * params.damping;
    let new_pos = pos + vel * params.dt;

    velocities[i] = vel;
    positions_out[i] = new_pos;
}
