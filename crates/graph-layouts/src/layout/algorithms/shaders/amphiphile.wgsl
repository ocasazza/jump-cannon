// Amphiphile self-assembly: coarse-grained particles of three species —
// head (0), tail (1), solvent (2) — under species-pair potentials, so
// micelles, bilayer sheets, tubes and closed vesicles emerge from a random
// start without authored structure.
//
// Pair law on particle i from j, r = |x_i - x_j|, σ = params.sigma,
// r_c = params.cutoff, w = r_c - σ, ε = epsilon[s_i][s_j] (zero or negative
// means repulsive-only):
//   core  (r < σ):         F = core_k · (1 - r/σ)             pushes apart
//   well  (σ ≤ r ≤ r_c):   F = -ε · sin(π (r - σ) / w)        pulls together
// The well is the force of the Cooke–Deserno cos² attraction
// U = -(2 ε w / π) cos²(π (r - σ) / (2 w)); it is zero at σ and at r_c and
// peaks at ε, so every pair force is bounded.
//
// Bonds (optional): the CSR edge list as springs F = -bond_k (r - bond_len).
// Gravity pulls toward the origin so an open box stays bounded. Brownian
// noise is a per-particle PCG stream scaled by temperature.
//
// Bindings (two kernels share one layout):
//   @group(0) @binding(0) positions   (read_write)  vec4: xyz + renderer .w
//   @group(0) @binding(1) velocities  (read_write)  vec4: xyz, .w unused
//   @group(0) @binding(2) forces      (read_write)  vec4
//   @group(0) @binding(3) species     (read)        u32 per particle
//   @group(0) @binding(4) edge_offsets (read)       length n+1
//   @group(0) @binding(5) edge_neighbors (read)     length 2m
//   @group(0) @binding(6) rng         (read_write)  u32 per particle
//   @group(0) @binding(7) energy_out  (read_write)  |v|² per particle
//   @group(0) @binding(8) params      (uniform)
// Eight storage buffers: inside WebGPU's per-stage minimum of 8.

struct Params {
    epsilon0: vec4<f32>,   // ε[0][0..2], pad
    epsilon1: vec4<f32>,   // ε[1][0..2], pad
    epsilon2: vec4<f32>,   // ε[2][0..2], pad
    dt: f32,
    damping: f32,
    temperature: f32,
    cutoff: f32,
    sigma: f32,
    core_k: f32,
    bond_k: f32,
    bond_len: f32,
    gravity: f32,
    n_nodes: u32,
    step_index: u32,
    v_max: f32,
};

@group(0) @binding(0) var<storage, read_write> positions:      array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> velocities:     array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> forces:         array<vec4<f32>>;
@group(0) @binding(3) var<storage, read>       species:        array<u32>;
@group(0) @binding(4) var<storage, read>       edge_offsets:   array<u32>;
@group(0) @binding(5) var<storage, read>       edge_neighbors: array<u32>;
@group(0) @binding(6) var<storage, read_write> rng:            array<u32>;
@group(0) @binding(7) var<storage, read_write> energy_out:     array<f32>;
@group(0) @binding(8) var<uniform>             params:         Params;

const PI: f32 = 3.14159265358979;

fn linear_index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.x + gid.y * (nwg.x * 64u);
}

fn epsilon(a: u32, b: u32) -> f32 {
    var row: vec4<f32>;
    if (a == 0u) { row = params.epsilon0; }
    else if (a == 1u) { row = params.epsilon1; }
    else { row = params.epsilon2; }
    if (b == 0u) { return row.x; }
    if (b == 1u) { return row.y; }
    return row.z;
}

fn pcg(state: ptr<function, u32>) -> f32 {
    let old = *state;
    *state = old * 747796405u + 2891336453u;
    let word = ((old >> ((old >> 28u) + 4u)) ^ old) * 277803737u;
    let out = (word >> 22u) ^ word;
    return f32(out) / 4294967296.0;
}

// Approximate standard normal from two uniforms (Box–Muller).
fn gaussian(state: ptr<function, u32>) -> f32 {
    let u1 = max(pcg(state), 1e-7);
    let u2 = pcg(state);
    return sqrt(-2.0 * log(u1)) * cos(2.0 * PI * u2);
}

@compute @workgroup_size(64)
fn force_step(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n_nodes) { return; }
    let pos = positions[i].xyz;
    let si = species[i];
    let sigma = max(params.sigma, 1e-3);
    let cutoff = max(params.cutoff, sigma + 1e-3);
    let w = cutoff - sigma;
    let cutoff2 = cutoff * cutoff;

    var force = vec3<f32>(0.0, 0.0, 0.0);
    for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
        if (j == i) { continue; }
        let d = pos - positions[j].xyz;
        let dist2 = dot(d, d);
        if (dist2 > cutoff2 || dist2 < 1e-8) { continue; }
        let r = sqrt(dist2);
        let dir = d / r;
        var mag = 0.0;
        if (r < sigma) {
            mag = params.core_k * (1.0 - r / sigma);
        } else {
            let eps = epsilon(si, species[j]);
            if (eps > 0.0) {
                mag = -eps * sin(PI * (r - sigma) / w);
            }
        }
        force = force + dir * mag;
    }

    if (params.bond_k > 0.0) {
        let start = edge_offsets[i];
        let end = edge_offsets[i + 1u];
        for (var k: u32 = start; k < end; k = k + 1u) {
            let other = edge_neighbors[k];
            let d = positions[other].xyz - pos;
            let r = max(length(d), 1e-4);
            force = force + (d / r) * (params.bond_k * (r - params.bond_len));
        }
    }

    force = force - pos * params.gravity;

    if (params.temperature > 0.0) {
        var state = rng[i] ^ (params.step_index * 0x9E3779B9u);
        let noise = vec3<f32>(gaussian(&state), gaussian(&state), gaussian(&state));
        force = force + noise * params.temperature;
        rng[i] = state;
    }

    forces[i] = vec4<f32>(force, 0.0);
}

@compute @workgroup_size(64)
fn integrate(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n_nodes) { return; }
    let p = positions[i];
    var vel = (velocities[i].xyz + forces[i].xyz * params.dt) * params.damping;
    let speed = length(vel);
    if (speed > params.v_max) {
        vel = vel * (params.v_max / speed);
    }
    if (!all(vel == vel)) {
        vel = vec3<f32>(0.0, 0.0, 0.0);
    }
    let next = p.xyz + vel * params.dt;
    positions[i] = vec4<f32>(next, p.w);
    velocities[i] = vec4<f32>(vel, 0.0);
    energy_out[i] = dot(vel, vel);
}
