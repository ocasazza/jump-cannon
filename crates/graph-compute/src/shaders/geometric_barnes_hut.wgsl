struct GeometricParams {
    n_nodes: u32,
    n_edges: u32,
    n_octree: u32,
    class_affinity_dim: u32,
    edge_stiffness: f32,
    angle_stiffness: f32,
    exclusion_strength: f32,
    affinity_strength: f32,
    gravity: f32,
    time_step: f32,
    damping: f32,
    max_step: f32,
    theta: f32,
    default_radius: f32,
    cutoff_scale: f32,
};

struct OctNode {
    pos_size: vec4<f32>,
    com_mass: vec4<f32>,
    links:    vec4<u32>,
};

@group(0) @binding(0) var<storage, read_write> positions:    array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> velocities:   array<vec4<f32>>;
@group(0) @binding(2) var<storage, read>       edges:        array<vec2<u32>>;
@group(0) @binding(3) var<storage, read>       target_lens:  array<f32>;
@group(0) @binding(4) var<uniform>             params:       GeometricParams;
@group(0) @binding(5) var<storage, read>       node_class:   array<u32>;
@group(0) @binding(6) var<storage, read>       node_coord:   array<u32>;
@group(0) @binding(7) var<storage, read>       node_mass:    array<f32>;
@group(0) @binding(8) var<storage, read>       oct_nodes:    array<OctNode>;
@group(0) @binding(9) var<storage, read>       coord_angles: array<f32>;
@group(0) @binding(10) var<storage, read>      class_radius: array<f32>;
@group(0) @binding(11) var<storage, read>      class_affinity: array<f32>;

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;

fn get_radius(class_id: u32) -> f32 {
    let len = arrayLength(&class_radius);
    if (class_id >= len) {
        return params.default_radius;
    }
    return class_radius[class_id];
}

fn get_affinity(class_i: u32, class_j: u32) -> f32 {
    let dim = params.class_affinity_dim;
    if (dim == 0u || class_i >= dim || class_j >= dim) {
        return 0.0;
    }
    return class_affinity[class_i * dim + class_j];
}

@compute @workgroup_size(64)
fn geometric_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos_i = positions[i].xyz;
    let class_i = node_class[i];
    let rad_i = get_radius(class_i);
    
    var force = vec3<f32>(0.0, 0.0, 0.0);

    // ---- 1. Repulsion (Barnes-Hut) ---------------------------------------
    if (params.n_octree > 0u) {
        let theta2 = params.theta * params.theta;
        var idx: u32 = 0u;
        let walk_cap = max(params.n_octree * 4u, 16u);
        var walk: u32 = 0u;
        loop {
            if (idx == OCT_END || walk >= walk_cap) { break; }
            walk = walk + 1u;
            let node = oct_nodes[idx];
            let body = node.links.x;
            let com = node.com_mass.xyz;
            let mass_n = node.com_mass.w;
            let half = node.pos_size.w;
            let s = half * 2.0;
            let d = com - pos_i; 
            let r2 = dot(d, d);

            if (body != OCT_BODY_INTERNAL) {
                if (body != i && mass_n > 0.0) {
                    let r = max(sqrt(r2), 1e-4);
                    let class_j = node_class[body];
                    let rad_j = get_radius(class_j);
                    let sigma = max(rad_i + rad_j, 1e-3);
                    let u = d / r;
                    
                    if (r < sigma) {
                        force = force - u * (params.exclusion_strength * (sigma / r - 1.0));
                    }
                    
                    let aff = get_affinity(class_i, class_j);
                    force = force + u * (aff * params.affinity_strength);
                }
                idx = node.links.z;
                continue;
            }

            if (mass_n > 0.0 && r2 > 0.0 && (s * s) < (theta2 * r2)) {
                // Far-field approximation
                // TODO: Better far-field for affinity
                idx = node.links.z; 
            } else {
                idx = node.links.y;
            }
        }
    }

    // ---- 2. Attraction (Edges) -------------------------------------------
    for (var e: u32 = 0u; e < params.n_edges; e = e + 1u) {
        let edge = edges[e];
        var other: u32 = 0u;
        var matched = false;
        if (edge.x == i) { other = edge.y; matched = true; }
        else if (edge.y == i) { other = edge.x; matched = true; }
        
        if (matched) {
            let pos_j = positions[other].xyz;
            let d = pos_j - pos_i;
            let r = max(length(d), 1e-6);
            let t_len = target_lens[e];
            let f = params.edge_stiffness * (r - t_len) / r;
            force = force + d * f;
        }
    }

    // ---- 4. Gravity ------------------------------------------------------
    force = force - pos_i * (params.gravity * node_mass[i]);

    // ---- Integrate -------------------------------------------------------
    var vel = velocities[i].xyz;
    let m = max(node_mass[i], 1e-3);
    vel = (vel + (force / m) * params.time_step) * params.damping;
    
    var disp = vel * params.time_step;
    if (params.max_step > 0.0 && length(disp) > params.max_step) {
        disp = normalize(disp) * params.max_step;
        vel = disp / params.time_step;
    }
    
    velocities[i] = vec4<f32>(vel, 0.0);
    positions[i] = vec4<f32>(pos_i + disp, 0.0);
}
