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

// Determinism (timeline P2): positions are DOUBLE-BUFFERED. Every thread reads
// the START-OF-STEP positions from `positions_in` (read-only) and writes only
// its own node to `positions_out`. Reading and writing the SAME read_write
// buffer (the old design) is an intra-dispatch read-after-write race — whether a
// neighbour's position is observed pre- or post-update depends on workgroup
// scheduling, which made the same seed diverge ~1 ULP/step run-to-run (and
// compounded chaotically). Ping-ponging in/out (the SGD / force.wgsl pattern)
// removes the hazard, so the step is order-deterministic AND its physics is a
// clean Jacobi update (all forces evaluated at the start-of-step config).
@group(0) @binding(0) var<storage, read>       positions:     array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> positions_out: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> velocities:   array<vec4<f32>>;
// Per-CSR-entry edge target lengths, parallel to the neighbours region of `csr`
// (binding 12). The target length for the neighbour at csr index `aa` is
// csr_target_lens[aa - csr[0]] (csr[0] = n_nodes+1 = start of that region).
@group(0) @binding(2) var<storage, read>       csr_target_lens: array<f32>;
@group(0) @binding(4) var<uniform>             params:       GeometricParams;
@group(0) @binding(5) var<storage, read>       node_class:   array<u32>;
@group(0) @binding(6) var<storage, read>       node_coord:   array<u32>;
@group(0) @binding(7) var<storage, read>       node_mass:    array<f32>;
@group(0) @binding(8) var<storage, read>       oct_nodes:    array<OctNode>;
@group(0) @binding(9) var<storage, read>       coord_angles: array<f32>;
@group(0) @binding(10) var<storage, read>      class_radius: array<f32>;
@group(0) @binding(11) var<storage, read>      class_affinity: array<f32>;
// Packed CSR adjacency in ONE buffer (built host-side in geometric_gpu.rs):
//   csr[0 ..= n_nodes] = offsets, each PRE-SHIFTED by (n_nodes+1) so it already
//                        points into the neighbours region.
//   csr[off .. ]       = neighbour node ids.
// Neighbours of node v are csr[csr[v] .. csr[v+1]].
@group(0) @binding(12) var<storage, read>      csr:            array<u32>;

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

// Preferred neighbour angle (radians) for a coordination id, clamped to the
// table. Empty table ⇒ 120° (matches the CPU `lookup_angle` default).
fn get_coord_angle(coord_id: u32) -> f32 {
    let len = arrayLength(&coord_angles);
    if (len == 0u) {
        return 2.0943951; // 120° in radians
    }
    var id = coord_id;
    if (id >= len) {
        id = len - 1u;
    }
    return radians(coord_angles[id]);
}

// Force on endpoint `pj` for the bond-angle triple (center `pc`, endpoints
// `pj`,`pk`) relaxing toward `ideal` radians, for `E = ½·k·(θ−ideal)²`. This is
// the negative gradient w.r.t. `pj`, identical to the CPU `apply_angle_pair`
// term — so CPU and GPU minimise the same angle energy.
fn angle_endpoint_force(pc: vec3<f32>, pj: vec3<f32>, pk: vec3<f32>, ideal: f32, k: f32) -> vec3<f32> {
    let a = pj - pc;
    let b = pk - pc;
    let la = max(length(a), 1e-6);
    let lb = max(length(b), 1e-6);
    let ua = a / la;
    let ub = b / lb;
    let cos_t = clamp(dot(ua, ub), -1.0, 1.0);
    let theta = acos(cos_t);
    let sin_t = max(sqrt(1.0 - cos_t * cos_t), 1e-4);
    let coef = k * (theta - ideal);
    return (coef / (sin_t * la)) * (ub - cos_t * ua);
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

    // ---- 2. Attraction (edge-length springs) — via CSR adjacency ---------
    // Walk node i's neighbours directly (O(deg)) instead of scanning the whole
    // edge list (O(E)). Each undirected edge {i,j} appears once in i's CSR list,
    // so the spring force on i is applied exactly once — same as the old
    // unique-edge scan. Target length for the neighbour at csr index `aa` is
    // csr_target_lens[aa - header] (header = csr[0] = neighbours-region start).
    {
        let beg = csr[i];
        let end = csr[i + 1u];
        let header = csr[0u];
        for (var aa: u32 = beg; aa < end; aa = aa + 1u) {
            let other = csr[aa];
            let pos_j = positions[other].xyz;
            let d = pos_j - pos_i;
            let r = max(length(d), 1e-6);
            let t_len = csr_target_lens[aa - header];
            let f = params.edge_stiffness * (r - t_len) / r;
            force = force + d * f;
        }
    }

    // ---- 3. Angle (coordination) constraints -----------------------------
    // Neighbours come from the packed CSR adjacency (binding 12): the neighbours
    // of node v are csr[csr[v] .. csr[v+1]] (offsets are pre-shifted to point
    // into the neighbours region). This replaces the old O(deg²·E) edge-list
    // scans with direct O(deg) reads — no MAX_DEG cap needed. The physics is
    // unchanged: each thread sums the NET angle force on its own node i across
    // every triple it participates in:
    //   • as the CENTER  (c = i): the reaction −(F_j+F_k) for each neighbour
    //     pair {j,k},
    //   • as an ENDPOINT (j = i): the force F_j for the triple (c; i, k) at each
    //     neighbouring center c with another neighbour k.
    // Summed, this is the gradient of the same Σ ½·k·(θ−ideal)² energy the CPU
    // minimises (so the CPU↔GPU equivalence gate's square case agrees).
    if (params.angle_stiffness != 0.0) {
        let ak = params.angle_stiffness;

        // i's neighbour range in CSR.
        let i_beg = csr[i];
        let i_end = csr[i + 1u];

        // ROLE A — i is the center: reaction force for each neighbour pair.
        let ideal_i = get_coord_angle(node_coord[i]);
        for (var aa: u32 = i_beg; aa < i_end; aa = aa + 1u) {
            let nj = csr[aa];
            if (nj == i) { continue; } // skip self-loops (matches CPU)
            let pj = positions[nj].xyz;
            for (var bb: u32 = aa + 1u; bb < i_end; bb = bb + 1u) {
                let nk = csr[bb];
                if (nk == i || nk == nj) { continue; }
                let pk = positions[nk].xyz;
                let fj = angle_endpoint_force(pos_i, pj, pk, ideal_i, ak);
                let fk = angle_endpoint_force(pos_i, pk, pj, ideal_i, ak);
                force = force - (fj + fk);
            }
        }

        // ROLE B — i is an endpoint of each neighbouring center c.
        for (var aa: u32 = i_beg; aa < i_end; aa = aa + 1u) {
            let c = csr[aa];
            if (c == i) { continue; } // self-loop: i is not an endpoint of itself
            let pc = positions[c].xyz;
            let ideal_c = get_coord_angle(node_coord[c]);
            let c_beg = csr[c];
            let c_end = csr[c + 1u];
            for (var bb: u32 = c_beg; bb < c_end; bb = bb + 1u) {
                let k = csr[bb];
                // Triple (center c; endpoints i, k): skip self-loops and i==k
                // (matches the CPU j==c||kn==c||j==kn guards).
                if (k != i && k != c) {
                    force = force + angle_endpoint_force(pc, pos_i, positions[k].xyz, ideal_c, ak);
                }
            }
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
    positions_out[i] = vec4<f32>(pos_i + disp, 0.0);
}
