// 3D force-directed layout compute shader.
//
// One dispatch = one simulation step. Repulsion has three backends
// selected per-frame through `params.repulsion_mode`:
//   0 = Exact            every node visits every other node, O(n²)
//   1 = Barnes-Hut       stackless rope walk over a host-built octree
//   2 = NegativeSampling K random partners per node per step, O(n·K)
// Spring forces are O(degree) via CSR adjacency (edge_offsets /
// edge_neighbors), computed by `spring_step` over Tigr virtual vertices.
// Integration is semi-implicit Euler with per-node mass and velocity
// damping.
//
// Two force laws (`params.force_model`):
//   0 = spring-electrical  Hooke springs + Coulomb `repulsion · m / d²`
//   1 = t-FDP              Student-t forces (Zhong et al., TVCG 2023). With
//                          r = d / spring_len:
//                            attraction  α (r + β r / (1 + r²))
//                            repulsion   r / (1 + r²)^γ
//                          Under NegativeSampling the repulsion is weighted
//                          by (deg_i + deg_j) / 2 and scaled by tfdp_k — the
//                          expectation SNAP-tFDP (arXiv:2608.01907, eq. 6)
//                          proves its edge-centric sampler optimises.
//
// Bindings (force_step + spring_step share one pipeline layout):
//   @group(0) @binding(0) positions_in       (read)       vec4: xyz + mass in .w
//   @group(0) @binding(1) positions_out      (read_write)
//   @group(0) @binding(2) velocities         (read_write)
//   @group(0) @binding(3) edge_offsets       (read)       length n+1
//   @group(0) @binding(4) edge_neighbors     (read)       length 2*m
//   @group(0) @binding(5) params             (uniform)
//   @group(0) @binding(6) energy_out         (read_write) length n  (|vel|²)
//   @group(1) @binding(1) oct_nodes          (read)       Barnes-Hut octree
//   @group(2) @binding(0..2)                              hub-aware spring CSR
//
// Dispatch shape: every kernel is 64 lanes wide and 1-D in meaning, but
// the host spills into the Y dimension once ceil(n/64) would exceed
// WebGPU's maxComputeWorkgroupsPerDimension (65535). Kernels MUST recover
// their index through `linear_index`, never `gid.x` alone.

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
    repulsion_radius: f32,
    // Repulsion backend: 0 = exact, 1 = Barnes-Hut, 2 = negative sampling.
    repulsion_mode: u32,
    // Barnes-Hut acceptance criterion. Borderline at θ≈0.7 — see
    // Burtscher & Pingali 2011 §4.5.
    bh_theta: f32,

    // Number of populated octree slots (≤ 2N). Only meaningful under
    // Barnes-Hut.
    n_octree: u32,
    // K — random samples per node per step. Only consulted when
    // repulsion_mode == 2.
    repulsion_samples: u32,
    // Monotonic dispatch counter — PRNG seed component for negative
    // sampling. Mixed with node index to give each (i, step) a
    // distinct K-set without per-node state.
    step_index: u32,
    // Force law: 0 = spring-electrical, 1 = t-FDP.
    force_model: u32,

    tfdp_alpha: f32,
    tfdp_beta: f32,
    tfdp_gamma: f32,
    tfdp_k: f32,
};

// Positions are stored as vec4 — xyz is the world-space position, w is
// the per-node mass. Packing mass into the .w slot drops one storage-
// binding slot vs a separate `mass` buffer. Every position read uses
// `.xyz`; mass reads use `.w`. Writes to `positions_out` must preserve
// the .w (see force_step's final store).
@group(0) @binding(0) var<storage, read>       positions_in:    array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> positions_out:   array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> velocities:      array<vec3<f32>>;
@group(0) @binding(3) var<storage, read>       edge_offsets:    array<u32>;
@group(0) @binding(4) var<storage, read>       edge_neighbors:  array<u32>;
@group(0) @binding(5) var<uniform>             params:          SimParams;
@group(0) @binding(6) var<storage, read_write> energy_out:      array<f32>;

// ---- Barnes-Hut octree bindings (group 1) ----------------------------------
//
// Layout matches `OctNodeRaw` on the Rust side:
//   pos_size: vec4 = (center.xyz, half_extent)
//   com_mass: vec4 = (com.xyz, total_mass)
//   links:    vec4<u32> = (body_idx_or_FFFFFFFF, next_idx, skip_idx, child_count)
//
// Stackless rope traversal: at each visited node, if it's a leaf or s/d<θ
// passes the acceptance criterion, accumulate the COM contribution and jump
// to `meta.z` (skip_idx, the next-sibling-or-uncle in DFS order). Otherwise
// descend by jumping to `meta.y` (next_idx, the first child). Sentinel
// 0xFFFFFFFFu ends the walk. This pattern eliminates per-thread stacks and
// — paired with the stochastic acceptance below (Petrescu 2025) — keeps
// warps coherent.
struct OctNode {
    pos_size: vec4<f32>,
    com_mass: vec4<f32>,
    links:    vec4<u32>,
};
@group(1) @binding(1) var<storage, read> oct_nodes: array<OctNode>;

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;

// ---- Hub-aware spring bindings (group 2) -----------------------------------
//
// Tigr-style virtual-vertex split: high-degree vertices are split into
// chunks of HUB_THRESHOLD edges (CPU preprocessing). Each virtual vertex
// runs `spring_step` independently and writes its partial spring force
// into `spring_force_partial[virt_idx]`. `force_step` then gathers
// partials via `node_to_virt_offsets[i..i+1]`. This converts a single
// O(degree) loop on a hub thread into ceil(degree/HUB_THRESHOLD) parallel
// O(HUB_THRESHOLD) lanes, eliminating the warp-stall on power-law graphs.
// `virt_csr` packs `node_to_virt_offsets` (length n_nodes+1) followed
// by `virt_real_idx` (length n_virtual) into one buffer. This saves one
// storage-binding slot per stage so the force_step pipeline fits the
// per-stage cap of 10 (Chrome WebGPU runtime limit).
//
// Access patterns:
//   node_to_virt_offsets[i] → virt_csr[i]
//   virt_real_idx[v]        → virt_csr[(params.n_nodes + 1u) + v]
@group(2) @binding(0) var<storage, read>       virt_csr:             array<u32>;
@group(2) @binding(1) var<storage, read>       virt_edge_offsets:    array<u32>;
@group(2) @binding(2) var<storage, read_write> spring_force_partial: array<vec3<f32>>;

const WORKGROUP_SIZE: u32 = 64u;

// Linear lane index for a dispatch that may have spilled into Y. Matches
// `dispatch_1d` on the Rust side: groups are laid out row-major with
// `num_workgroups.x` groups per row.
fn linear_index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.x + gid.y * nwg.x * WORKGROUP_SIZE;
}

// PCG output hash (pcg-random.org). One mul + xorshift — stateless,
// deterministic, well-distributed. Used by the negative-sampling
// repulsion path so consecutive (node_idx, iter, step_index) triples
// don't correlate.
fn pcg_hash(state: u32) -> u32 {
    let s = state * 747796405u + 2891336453u;
    let word = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (word >> 22u) ^ word;
}

fn degree_of(i: u32) -> f32 {
    return f32(edge_offsets[i + 1u] - edge_offsets[i]);
}

// ---- Force laws --------------------------------------------------------------
//
// Every helper returns the force on `i` given the offset `d = pos_i -
// pos_j` (so a positive scalar along `d` pushes i away from j) and the
// squared distance (already floored by the caller).

// Coulomb repulsion `repulsion · w / d²` along d.
fn coulomb_repulsion(d: vec3<f32>, dist2: f32, w: f32) -> vec3<f32> {
    return d * (params.repulsion * w / dist2);
}

// t-FDP repulsion, unit-normalised by spring_len and lifted back to
// world units: |F| = spring_k · spring_len · w · r / (1 + r²)^γ.
fn tfdp_repulsion(d: vec3<f32>, dist2: f32, w: f32) -> vec3<f32> {
    let inv_len = 1.0 / max(params.spring_len, 1e-6);
    let r2 = dist2 * inv_len * inv_len;
    let r = sqrt(r2);
    let mag = r / pow(1.0 + r2, params.tfdp_gamma);
    // d / dist = unit vector; dist = r · spring_len.
    return d * (params.spring_k * params.spring_len * w * mag / max(r * params.spring_len, 1e-6));
}

fn repulsion_force(d: vec3<f32>, dist2: f32, w: f32) -> vec3<f32> {
    if (params.force_model == 1u) {
        return tfdp_repulsion(d, dist2, w);
    }
    return coulomb_repulsion(d, dist2, w);
}

// Attraction on `i` from an edge to `other`, given `d = pos_other - pos_i`.
fn attraction_force(d: vec3<f32>) -> vec3<f32> {
    let dist = max(length(d), 0.01);
    if (params.force_model == 1u) {
        let r = dist / max(params.spring_len, 1e-6);
        let mag = params.tfdp_alpha * (r + params.tfdp_beta * r / (1.0 + r * r));
        return (d / dist) * (params.spring_k * params.spring_len * mag);
    }
    let stretch = dist - params.spring_len;
    return (d / dist) * (params.spring_k * stretch);
}

// ---- Hub-aware spring kernel ----------------------------------------------
//
// One thread per virtual vertex. Each thread reads its real-vertex index
// + edge slice [virt_edge_offsets[v], virt_edge_offsets[v+1]) from the
// virtualized CSR, accumulates the attraction, and stores it in
// `spring_force_partial[v]`. No atomics needed: each virtual vertex owns
// exactly one slot, and `force_step` sums the slots that belong to each
// real vertex via `node_to_virt_offsets`.
@compute @workgroup_size(64)
fn spring_step(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let v = linear_index(gid, nwg);
    // Re-derive n_virtual from node_to_virt_offsets[n_nodes].
    // node_to_virt_offsets occupies the first n_nodes+1 entries of virt_csr.
    let n_virtual = virt_csr[params.n_nodes];
    if (v >= n_virtual) { return; }

    // virt_real_idx lives in the tail of virt_csr (after the n_nodes+1
    // node_to_virt_offsets entries).
    let i = virt_csr[params.n_nodes + 1u + v];
    let pos = positions_in[i].xyz;
    let estart = virt_edge_offsets[v];
    let eend   = virt_edge_offsets[v + 1u];

    var f = vec3<f32>(0.0, 0.0, 0.0);
    for (var k: u32 = estart; k < eend; k = k + 1u) {
        let other = edge_neighbors[k];
        f = f + attraction_force(positions_in[other].xyz - pos);
    }
    spring_force_partial[v] = f;
}

@compute @workgroup_size(64)
fn force_step(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n_nodes) {
        return;
    }

    let pos_full = positions_in[i];
    let pos = pos_full.xyz;
    let self_mass = pos_full.w;
    var vel = velocities[i];
    var force = vec3<f32>(0.0, 0.0, 0.0);

    let r_clip = params.repulsion_radius;
    // Use a "very large" finite value as the no-clip sentinel. f32::MAX
    // (3.4028235e+38) overflows naga's WGSL constant parser, so use a value
    // far larger than any plausible distance² instead.
    let r_clip2 = select(1.0e+18, r_clip * r_clip, r_clip > 0.0);

    // Repulsion-distance² floor.
    //
    // Without this, a coincident or near-coincident pair generates a
    // single-pair Coulomb force of `repulsion / dist²` with `dist² → 0` —
    // at the default `repulsion=4000` and a hard-coded floor of `0.01`,
    // that's `4000/0.01 = 400_000` per pair. dt=0.1 × that = ~40k velocity
    // per step → node ejected ~4k units → next iteration NaN propagates.
    //
    // Scale the floor with `spring_len` so the threshold matches the
    // layout's natural unit (e.g. spring_len=400 → floor=1600 → max
    // single-pair force ~ repulsion/1600 ≈ 2.5 for the defaults). Stable
    // for both chaotic random-ball seeds and compact converged seeds (the
    // latter being the failure mode the topo-fisheye seed mode hit).
    // The t-force is bounded by construction, but the floor also keeps
    // the direction vector well-defined, so it applies to both laws.
    let dist2_floor = max(params.spring_len * params.spring_len * 1e-4, 1e-4);

    // ---- Repulsion ---------------------------------------------------------
    // Backend selection. All paths read positions_in[*]; the BH path
    // additionally reads the host-built octree from group(1).
    if (params.repulsion_mode == 1u) {
        // Stackless rope walk over the GPU-built octree (shaders/octree.wgsl).
        // Self-pruning happens via the leaf body-index check (single-body
        // leaves) or cell containment (multi-body leaves); the acceptance
        // criterion s/d < θ is applied per visited internal node.
        let theta2 = params.bh_theta * params.bh_theta;
        var idx: u32 = 0u;
        // Hard upper bound (paranoia). The rope always terminates at OCT_END
        // within the emitted-node count; `params.n_octree` carries the octree
        // node *capacity* (a static build-time bound), so 4× it is a safe
        // anti-hang cap independent of the exact GPU-computed node count.
        let walk_cap = max(params.n_octree * 4u, 16u);
        var step: u32 = 0u;
        loop {
            if (idx == OCT_END) { break; }
            if (step >= walk_cap) { break; }
            step = step + 1u;
            let node = oct_nodes[idx];
            let body = node.links.x;
            let cnt = node.links.w;
            let center = node.pos_size.xyz;
            let half = node.pos_size.w;
            let com = node.com_mass.xyz;
            let mass_n = node.com_mass.w;
            if (body != OCT_BODY_INTERNAL) {
                // Leaf. links.w disambiguates single-body (== 1) from a
                // multi-body max-depth leaf (> 1, bodies coincident to the
                // Morton grid). The GPU build stores the real body index in
                // links.x for single-body leaves and the sorted-range start
                // for multi-body leaves.
                if (cnt <= 1u) {
                    if (body != i) {
                        let d = pos - com;
                        let dist2 = dot(d, d);
                        if (dist2 <= r_clip2 && mass_n > 0.0) {
                            force = force + repulsion_force(d, max(dist2, dist2_floor), mass_n);
                        }
                    }
                } else {
                    // Multi-body leaf: remove this body's own contribution
                    // when it falls inside the leaf cell (exact membership,
                    // since the cell is body i's own Morton cell). Guard the
                    // adjusted mass against going non-positive.
                    var m = mass_n;
                    var c = com;
                    let inside = all(abs(pos - center) <= vec3<f32>(half * 1.001 + 1e-4));
                    if (inside) {
                        m = mass_n - self_mass;
                        if (m > 1e-6) {
                            c = (com * mass_n - pos * self_mass) / m;
                        }
                    }
                    if (m > 1e-6) {
                        let d = pos - c;
                        let dist2 = dot(d, d);
                        if (dist2 <= r_clip2) {
                            force = force + repulsion_force(d, max(dist2, dist2_floor), m);
                        }
                    }
                }
                idx = node.links.z; // skip = next-sibling-or-uncle
                continue;
            }
            // Internal — Barnes-Hut acceptance: treat as a point mass when
            // (s/d)² < θ². Squares both sides to avoid the sqrt.
            let s = half * 2.0;
            let d = pos - com;
            let dist2 = dot(d, d);
            if (mass_n > 0.0 && dist2 > 0.0 && (s * s) < (theta2 * dist2)) {
                if (dist2 <= r_clip2) {
                    force = force + repulsion_force(d, max(dist2, dist2_floor), mass_n);
                }
                idx = node.links.z; // accepted → skip subtree
            } else {
                idx = node.links.y; // descend into first child
            }
        }
    } else if (params.repulsion_mode == 2u) {
        // Stochastic negative sampling. Each node samples K random others
        // per step instead of visiting spatial neighbors. No spatial
        // structure at all, so per-step cost is O(K) regardless of how
        // clustered the layout is. DRGraph: arxiv.org/abs/2008.07799.
        //
        // Under t-FDP this is the SNAP-tFDP estimator (arXiv:2608.01907
        // eq. 6): the expected repulsion on i is
        //   k · mean_j[ (deg_i + deg_j) / 2 · F^r(i, j) ]
        // which a K-sample Monte-Carlo mean reproduces exactly in
        // expectation. Coulomb keeps the mass weighting so its layouts
        // stay comparable with the other backends.
        let k = params.repulsion_samples;
        let tfdp = params.force_model == 1u;
        let deg_i = degree_of(i);
        let sample_scale = select(1.0, params.tfdp_k / f32(k), tfdp);
        // Mix node index with step counter so the same node samples a
        // different K-set every step — avoids systematic bias toward any
        // particular pair across the run.
        let base = i * 0x9E3779B9u + params.step_index * 0x85EBCA6Bu;
        for (var iter: u32 = 0u; iter < k; iter = iter + 1u) {
            let h = pcg_hash(base + iter);
            // Modulo bias is fine — n_nodes is small relative to 2^32 and
            // unbiased sampling would cost a rejection loop for no
            // measurable layout-quality difference.
            let j = h % params.n_nodes;
            if (j == i) { continue; }
            let p_j = positions_in[j];
            let d = pos - p_j.xyz;
            let dist2 = dot(d, d);
            if (dist2 > r_clip2) { continue; }
            let dist2c = max(dist2, dist2_floor);
            let w = select(p_j.w, 0.5 * (deg_i + degree_of(j)), tfdp);
            force = force + repulsion_force(d, dist2c, w) * sample_scale;
        }
    } else {
        // Exact O(n²) reference. Fine for small graphs (< few thousand
        // nodes); Barnes-Hut is the default for larger ones.
        for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
            if (j == i) { continue; }
            let p_j = positions_in[j];
            let d = pos - p_j.xyz;
            let dist2 = dot(d, d);
            if (dist2 > r_clip2) { continue; }
            let dist2c = max(dist2, dist2_floor);
            // Mass packed into .w of positions buffer.
            force = force + repulsion_force(d, dist2c, p_j.w);
        }
    }

    // ---- Springs (gather over virtual vertices) ----------------------------
    // The hub-split spring_step kernel has already written per-virtual
    // partials into `spring_force_partial`. Here we sum the partials that
    // belong to real vertex `i`. For non-hub vertices this is one iteration;
    // for a hub of degree D it is ceil(D/HUB_THRESHOLD) iterations — already
    // computed in parallel in spring_step.
    // node_to_virt_offsets lives in the first n_nodes+1 entries of virt_csr.
    let v_start = virt_csr[i];
    let v_end   = virt_csr[i + 1u];
    for (var v: u32 = v_start; v < v_end; v = v + 1u) {
        force = force + spring_force_partial[v];
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

    // ---- Integrate (per-node mass) -----------------------------------------
    // Mass packed into positions_in[i].w (captured earlier as `self_mass`).
    let m = max(self_mass, 1.0);
    let accel = force / m;
    vel = (vel + accel * params.dt) * params.damping;

    // Velocity clamp — belt-and-suspenders to the dist² floor above. The
    // floor caps any *single-pair* repulsion, but a compact cluster of N
    // close pairs still sums to O(N · max-pair-force) of acceleration per
    // step. Without this cap the cluster shotguns outward by thousands of
    // units per step (user saw this as "screen turns black on
    // energy_threshold=0"). Bound the per-step displacement at one
    // spring-length: `|vel * dt| ≤ spring_len` ⟹ `|vel| ≤ spring_len/dt`.
    // Layout still moves freely — equilibrium positions are independent of
    // the cap; only the speed at which the integrator approaches them is
    // bounded.
    let v_max = params.spring_len / max(params.dt, 1e-6);
    let v_mag = length(vel);
    if (v_mag > v_max) {
        vel = vel * (v_max / v_mag);
    }

    // NaN/Inf guard — if anything upstream produced a non-finite value
    // (degenerate normalize on a zero vector, etc.), drop velocity to
    // zero rather than carry NaN forward and poison every subsequent
    // step through cell sharing.
    if (!all(vel == vel)) {  // wgsl: !(v == v) is the canonical NaN check
        vel = vec3<f32>(0.0, 0.0, 0.0);
    }

    let new_pos = pos + vel * params.dt;

    velocities[i] = vel;
    // Preserve the .w mass slot — without this the next frame's force_step
    // reads positions_in[i].w == 0, which divides by zero in the mass
    // term and stalls every node tagged as a hub.
    positions_out[i] = vec4<f32>(new_pos, self_mass);

    // Track per-node KE proxy = |vel|^2. CPU reduces.
    energy_out[i] = dot(vel, vel);
}
