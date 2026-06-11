// ForceAtlas2 — GPU compute kernel.
//
// brute-force; replace with Barnes-Hut for >50k nodes.
//
// Two entry points, one thread per node, split so the host can run the paper's
// ADAPTIVE GLOBAL SPEED reduction between them (Jacomy, Venturini, Heymann,
// Bastian 2014, PLOS ONE 9(6):e98679 §"Adapting the speed to the convergence",
// matching Gephi's reference ForceAtlas2.java):
//
//   fa2_force — O(n^2) repulsion + linear edge-scan attraction + gravity.
//     Writes the per-node force F_t and the per-node swing/traction stats
//       swinging  = mass · |F_{t-1} − F_t|      (erratic movement)
//       traction  = mass · |F_{t-1} + F_t| / 2  (useful movement)
//     with mass = deg+1. The host sums these (fixed order ⇒ deterministic) and
//     runs Gephi's jitter-tolerance / speed-efficiency state machine to produce
//     the global speed s(G) for this step.
//
//   fa2_apply — per-node displacement
//       factor = s(G) / (1 + sqrt(s(G) · swinging))            [Gephi]
//       factor = min(factor, k_smax / |F|)                     [paper k_smax]
//       pos   += F · factor · time_step
//     then F_t becomes F_{t-1} for the next step.
//
// The old integrator (`vel = (vel + force) * 0.5; pos += vel·dt`) accumulated
// momentum with no force-magnitude feedback: at vault scale (~10k nodes) the
// O(n²) repulsion grows with n and the spring overshoot condition k·dt² ≫ 2
// made positions grow ~37×/step until NaN. The paper's mechanism exists
// precisely to stop that — displacement is force times an adaptive factor, no
// velocity state at all.
//
// Bindings (group 0):
//   0  positions     read_write   array<vec4<f32>>  (xyz, _pad)
//   1  old_force     read_write   array<vec4<f32>>  F_{t-1} (xyz, _pad)
//   2  edges         read         array<vec2<u32>>  (src, tgt)
//   3  edge_weights  read         array<f32>
//   4  params        uniform      Fa2Params
//   5  degrees       read         array<u32>
//   6  force         read_write   array<vec4<f32>>  F_t (xyz, _pad)
//   7  stats         read_write   array<vec2<f32>>  (mass·swing, mass·traction)

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
    // Global adaptive speed s(G), recomputed by the host between fa2_force and
    // fa2_apply each step (Gephi's `speed`). Replaces the old _pad0.
    speed: f32,
    // Per-step displacement cap — the paper's k_smax (= 10 in Gephi/the paper):
    // s(n) ≤ k_smax / |F(n)| ⇒ |Δpos| ≤ k_smax. 0 disables. Replaces _pad1.
    max_displacement: f32,
};

@group(0) @binding(0) var<storage, read_write> positions:    array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> old_force:    array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       edges:        array<vec2<u32>>;
@group(0) @binding(3) var<storage, read>       edge_weights: array<f32>;
@group(0) @binding(4) var<uniform>             params:       Fa2Params;
@group(0) @binding(5) var<storage, read>       degrees:      array<u32>;
@group(0) @binding(6) var<storage, read_write> force:        array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> stats:        array<vec2<f32>>;

@compute @workgroup_size(64)
fn fa2_force(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos_i = positions[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    var f = vec3<f32>(0.0, 0.0, 0.0);

    // ---- Repulsion (brute-force O(n^2)) -----------------------------------
    for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
        if (j == i) { continue; }
        let pos_j = positions[j].xyz;
        let d = pos_i - pos_j;
        let r2 = max(dot(d, d), 1.0e-4);
        let deg_j = f32(degrees[j]) + 1.0;
        // Coulomb-style (deg_i+1)*(deg_j+1) / r^2 — applied along d.
        let coeff = params.scaling_ratio * deg_i * deg_j / r2;
        f = f + d * coeff;
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
        f = f + d * att;
    }

    // ---- Gravity ----------------------------------------------------------
    let r0 = max(length(pos_i), 1.0e-3);
    if (params.strong_gravity != 0u) {
        f = f - pos_i * (params.gravity * deg_i);
    } else {
        f = f - (pos_i / r0) * (params.gravity * deg_i);
    }

    // ---- Swing/traction stats for the host's global-speed reduction --------
    let old = old_force[i].xyz;
    force[i] = vec4<f32>(f, 0.0);
    stats[i] = vec2<f32>(
        deg_i * length(old - f),
        deg_i * 0.5 * length(old + f),
    );
}

@compute @workgroup_size(64)
fn fa2_apply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let f = force[i].xyz;
    let old = old_force[i].xyz;
    let deg_i = f32(degrees[i]) + 1.0;

    // Per-node adaptive speed (Gephi ForceAtlas2.java, adjustSizes=false):
    // a node that swings is slowed down individually.
    let swinging = deg_i * length(old - f);
    var factor = params.speed / (1.0 + sqrt(params.speed * swinging));

    // Paper §"speed limit": s(n) ≤ k_smax/|F(n)| caps |Δpos| at k_smax,
    // catching the one-step transient the (lagless in Gephi, but still
    // reduction-based) global speed cannot.
    let df = length(f);
    if (params.max_displacement > 0.0 && df > 1.0e-9) {
        factor = min(factor, params.max_displacement / df);
    }

    let new_pos = positions[i].xyz + f * (factor * params.time_step);

    positions[i] = vec4<f32>(new_pos, 0.0);
    old_force[i] = vec4<f32>(f, 0.0);
}
