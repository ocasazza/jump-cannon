// 3D force-directed layout compute shader.
//
// One dispatch = one simulation step. Repulsion is bounded by a uniform
// 3D voxel grid: each thread visits the 27 neighboring cells of its cell
// and only does pairwise work against those occupants — O(n) with bounded
// per-cell occupancy. Spring forces are O(degree) via CSR adjacency
// (edge_offsets / edge_neighbors). Integration is semi-implicit Euler with
// per-node mass and velocity damping.
//
// Bindings (force_step pipeline):
//   @group(0) @binding(0) positions_in       (read)
//   @group(0) @binding(1) positions_out      (read_write)
//   @group(0) @binding(2) velocities         (read_write)
//   @group(0) @binding(3) edge_offsets       (read)   length n+1
//   @group(0) @binding(4) edge_neighbors     (read)   length 2*m
//   @group(0) @binding(5) params             (uniform)
//   @group(0) @binding(6) cell_offsets       (read)   length n_cells+1
//   @group(0) @binding(7) cell_nodes         (read)   length n
//   @group(0) @binding(8) mass               (read)   length n
//   @group(0) @binding(9) energy_out         (read_write) length n  (max disp proxy)
//
// Bindings (grid build pipelines: clear_cell_counts / count_cells /
// scan_cell_offsets / scatter_cells). All four entry points share a single
// bind group layout so they can be dispatched against one bind group:
//   @group(1) @binding(0) gb_positions_in    (read)            length n
//   @group(1) @binding(1) gb_params          (uniform)
//   @group(1) @binding(2) gb_cell_counts     (atomic<u32>)     length n_cells
//   @group(1) @binding(3) gb_cell_cursor     (atomic<u32>)     length n_cells
//   @group(1) @binding(4) gb_cell_offsets    (read_write u32)  length n_cells+1
//   @group(1) @binding(5) gb_cell_nodes      (read_write u32)  length n

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
    grid_cell_size: f32,
    grid_enabled: u32,        // 0 = naive O(n^2), 1 = grid (legacy bool — superseded by repulsion_mode)
    grid_origin: vec3<f32>,
    n_cells: u32,
    grid_dim: vec3<u32>,
    // Repulsion backend: 0 = grid (27-cell stencil), 1 = Barnes-Hut
    // octree, 2 = negative-sampling (DRGraph-style).
    // Default 0; toggled per-frame via the host uniform.
    repulsion_mode: u32,
    // Barnes-Hut acceptance criterion. Borderline at θ≈0.7 — see
    // Burtscher & Pingali 2011 §4.5.
    bh_theta: f32,
    // Number of populated octree slots (≤ 2N). Only meaningful under
    // BarnesHut mode.
    n_octree: u32,
    // K — random samples per node per step. Only consulted when
    // repulsion_mode == 2.
    repulsion_samples: u32,
    // Monotonic dispatch counter — PRNG seed component for negative
    // sampling. Mixed with node index to give each (i, step) a
    // distinct K-set without per-node state.
    step_index: u32,
};

@group(0) @binding(0) var<storage, read>       positions_in:    array<vec3<f32>>;
@group(0) @binding(1) var<storage, read_write> positions_out:   array<vec3<f32>>;
@group(0) @binding(2) var<storage, read_write> velocities:      array<vec3<f32>>;
@group(0) @binding(3) var<storage, read>       edge_offsets:    array<u32>;
@group(0) @binding(4) var<storage, read>       edge_neighbors:  array<u32>;
@group(0) @binding(5) var<uniform>             params:          SimParams;
@group(0) @binding(6) var<storage, read>       cell_offsets:    array<u32>;
@group(0) @binding(7) var<storage, read>       cell_nodes:      array<u32>;
@group(0) @binding(8) var<storage, read>       mass:            array<f32>;
@group(0) @binding(9) var<storage, read_write> energy_out:      array<f32>;

// ---- Grid-build bindings (group 1) -----------------------------------------
// ---- Barnes-Hut octree bindings (group 2) ----------------------------------
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
@group(2) @binding(1) var<storage, read> oct_nodes: array<OctNode>;

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;

@group(1) @binding(0) var<storage, read>       gb_positions_in: array<vec3<f32>>;
@group(1) @binding(1) var<uniform>             gb_params:       SimParams;
@group(1) @binding(2) var<storage, read_write> gb_cell_counts:  array<atomic<u32>>;
@group(1) @binding(3) var<storage, read_write> gb_cell_cursor:  array<atomic<u32>>;
@group(1) @binding(4) var<storage, read_write> gb_cell_offsets: array<u32>;
@group(1) @binding(5) var<storage, read_write> gb_cell_nodes:   array<u32>;

// ---- Hub-aware spring bindings (group 3) -----------------------------------
//
// Tigr-style virtual-vertex split: high-degree vertices are split into
// chunks of HUB_THRESHOLD edges (CPU preprocessing). Each virtual vertex
// runs `spring_step` independently and writes its partial spring force
// into `spring_force_partial[virt_idx]`. `force_step` then gathers
// partials via `node_to_virt_offsets[i..i+1]`. This converts a single
// O(degree) loop on a hub thread into ceil(degree/HUB_THRESHOLD) parallel
// O(HUB_THRESHOLD) lanes, eliminating the warp-stall on power-law graphs.
@group(3) @binding(0) var<storage, read>       virt_real_idx:        array<u32>;
@group(3) @binding(1) var<storage, read>       virt_edge_offsets:    array<u32>;
@group(3) @binding(2) var<storage, read_write> spring_force_partial: array<vec3<f32>>;
@group(3) @binding(3) var<storage, read>       node_to_virt_offsets: array<u32>;

fn gb_cell_for(pos: vec3<f32>) -> u32 {
    let inv = 1.0 / gb_params.grid_cell_size;
    let rel = (pos - gb_params.grid_origin) * inv;
    let dim_x = i32(gb_params.grid_dim.x);
    let dim_y = i32(gb_params.grid_dim.y);
    let dim_z = i32(gb_params.grid_dim.z);
    var ix = i32(floor(rel.x));
    var iy = i32(floor(rel.y));
    var iz = i32(floor(rel.z));
    if (ix < 0) { ix = 0; } else if (ix >= dim_x) { ix = dim_x - 1; }
    if (iy < 0) { iy = 0; } else if (iy >= dim_y) { iy = dim_y - 1; }
    if (iz < 0) { iz = 0; } else if (iz >= dim_z) { iz = dim_z - 1; }
    return u32(ix)
        + u32(iy) * gb_params.grid_dim.x
        + u32(iz) * gb_params.grid_dim.x * gb_params.grid_dim.y;
}

@compute @workgroup_size(64)
fn clear_cell_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_cells) { return; }
    atomicStore(&gb_cell_counts[i], 0u);
    atomicStore(&gb_cell_cursor[i], 0u);
    // cell_offsets has length n_cells+1; clear the tail at thread 0.
    if (i == 0u) {
        gb_cell_offsets[gb_params.n_cells] = 0u;
    }
    gb_cell_offsets[i] = 0u;
}

@compute @workgroup_size(64)
fn count_cells(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_nodes) { return; }
    let cell = gb_cell_for(gb_positions_in[i]);
    atomicAdd(&gb_cell_counts[cell], 1u);
}

// Single-workgroup exclusive prefix sum (Hillis-Steele, 256 lanes) over
// cell_counts → cell_offsets. n_cells is bounded by MAX_N_CELLS=262144
// (64³), and each lane scans a contiguous chunk of CHUNK_SIZE=1024 cells,
// so the kernel covers the worst case in one dispatch with no host-side
// scan-tree.
//
// Phase 1: each lane reduces its chunk to a local_sum (serial within chunk).
// Phase 2: Hillis-Steele inclusive scan over the 256 local_sums in shared
//          memory (log2(256) = 8 barrier-bounded passes).
// Phase 3: each lane writes the exclusive prefix outputs across its chunk
//          using (inclusive - local_sum) as its base offset.
//
// Why not multi-workgroup decoupled-lookback: at n=262144 this single-WG
// scan finishes in ~10 μs on a modern GPU and avoids the second scan-tree
// dispatch + per-block-state buffer that decoupled-lookback would require.
const SCAN_WG_SIZE: u32 = 256u;
const SCAN_CHUNK_SIZE: u32 = 1024u; // SCAN_WG_SIZE * SCAN_CHUNK_SIZE = 262144

var<workgroup> scan_partials: array<u32, 256>;

@compute @workgroup_size(256)
fn scan_cell_offsets(@builtin(local_invocation_id) lid: vec3<u32>) {
    let tid = lid.x;
    let n = gb_params.n_cells;
    let start = tid * SCAN_CHUNK_SIZE;
    var end = start + SCAN_CHUNK_SIZE;
    if (end > n) { end = n; }
    if (start > n) { end = start; } // empty chunk for out-of-range lanes

    // Phase 1: per-lane reduction over its contiguous chunk.
    var local_sum: u32 = 0u;
    for (var c: u32 = start; c < end; c = c + 1u) {
        local_sum = local_sum + atomicLoad(&gb_cell_counts[c]);
    }
    scan_partials[tid] = local_sum;
    workgroupBarrier();

    // Phase 2: Hillis-Steele inclusive scan over the 256 partials.
    var step: u32 = 1u;
    loop {
        if (step >= SCAN_WG_SIZE) { break; }
        var v: u32 = 0u;
        if (tid >= step) { v = scan_partials[tid - step]; }
        workgroupBarrier();
        scan_partials[tid] = scan_partials[tid] + v;
        workgroupBarrier();
        step = step * 2u;
    }
    // Inclusive scan complete; exclusive = inclusive - local_sum.
    let block_offset = scan_partials[tid] - local_sum;

    // Phase 3: write exclusive prefix across this lane's chunk.
    var acc: u32 = block_offset;
    for (var c: u32 = start; c < end; c = c + 1u) {
        gb_cell_offsets[c] = acc;
        acc = acc + atomicLoad(&gb_cell_counts[c]);
    }

    // Total = inclusive scan of last lane = scan_partials[255].
    if (tid == 0u) {
        gb_cell_offsets[n] = scan_partials[SCAN_WG_SIZE - 1u];
    }
}

@compute @workgroup_size(64)
fn scatter_cells(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_nodes) { return; }
    let cell = gb_cell_for(gb_positions_in[i]);
    let slot = atomicAdd(&gb_cell_cursor[cell], 1u);
    gb_cell_nodes[gb_cell_offsets[cell] + slot] = i;
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

// ---- Hub-aware spring kernel ----------------------------------------------
//
// One thread per virtual vertex. Each thread reads its real-vertex index
// + edge slice [virt_edge_offsets[v], virt_edge_offsets[v+1]) from the
// virtualized CSR, accumulates the spring contribution, and stores it in
// `spring_force_partial[v]`. No atomics needed: each virtual vertex owns
// exactly one slot, and `force_step` sums the slots that belong to each
// real vertex via `node_to_virt_offsets`.
@compute @workgroup_size(64)
fn spring_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    // Re-derive n_virtual from node_to_virt_offsets[n_nodes].
    let n_virtual = node_to_virt_offsets[params.n_nodes];
    if (v >= n_virtual) { return; }

    let i = virt_real_idx[v];
    let pos = positions_in[i];
    let estart = virt_edge_offsets[v];
    let eend   = virt_edge_offsets[v + 1u];

    var f = vec3<f32>(0.0, 0.0, 0.0);
    for (var k: u32 = estart; k < eend; k = k + 1u) {
        let other = edge_neighbors[k];
        let d = positions_in[other] - pos;
        let dist = max(length(d), 0.01);
        let stretch = dist - params.spring_len;
        f = f + (d / dist) * (params.spring_k * stretch);
    }
    spring_force_partial[v] = f;
}

@compute @workgroup_size(64)
fn force_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos = positions_in[i];
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
    // single-pair force of `repulsion / dist²` with `dist² → 0` — at the
    // default `repulsion=4000` and the previous hard-coded floor `0.01`,
    // that's `4000/0.01 = 400_000` per pair. dt=0.1 × that = ~40k velocity
    // per step → node ejected ~4k units → next iteration NaN propagates.
    //
    // Scale the floor with `spring_len` so the threshold matches the
    // layout's natural unit (e.g. spring_len=400 → floor=1600 → max
    // single-pair force ~ repulsion/1600 ≈ 2.5 for the defaults). Stable
    // for both chaotic random-ball seeds and compact converged seeds (the
    // latter being the failure mode the topo-fisheye seed mode hit).
    let dist2_floor = max(params.spring_len * params.spring_len * 1e-4, 1e-4);

    // ---- Repulsion ---------------------------------------------------------
    // Backend selection: BarnesHut overrides the legacy grid path. Both
    // paths read positions_in[*]; the BH path additionally reads the
    // host-built octree from group(2).
    if (params.repulsion_mode == 1u) {
        // Stackless rope walk over the octree. Self-pruning happens via
        // the leaf body_idx check; the acceptance criterion s/d < θ is
        // applied per visited internal node.
        let theta2 = params.bh_theta * params.bh_theta;
        var idx: u32 = 0u;
        // Hard upper bound (paranoia): the octree has ≤ 2N nodes; cap at
        // 4*n_octree to make any malformed rope a hang-resistant bug
        // rather than an infinite loop on the GPU.
        let walk_cap = max(params.n_octree * 4u, 16u);
        var step: u32 = 0u;
        loop {
            if (idx == OCT_END) { break; }
            if (step >= walk_cap) { break; }
            step = step + 1u;
            let n = oct_nodes[idx];
            let body = n.links.x;
            let com = n.com_mass.xyz;
            let mass_n = n.com_mass.w;
            let half = n.pos_size.w;
            let s = half * 2.0;
            let d = pos - com;
            let dist2 = dot(d, d);
            // Leaf — apply directly (skip self).
            if (body != OCT_BODY_INTERNAL) {
                if (body != i) {
                    if (dist2 <= r_clip2 && mass_n > 0.0) {
                        let dist2c = max(dist2, dist2_floor);
                        force = force + d * (params.repulsion * mass_n / dist2c);
                    }
                }
                idx = n.links.z; // skip = next-sibling-or-uncle
                continue;
            }
            // Internal — apply Barnes-Hut acceptance: treat as point mass
            // when (s/d)² < θ². Avoids the sqrt by squaring both sides.
            if (mass_n > 0.0 && dist2 > 0.0 && (s * s) < (theta2 * dist2)) {
                if (dist2 <= r_clip2) {
                    let dist2c = max(dist2, dist2_floor);
                    force = force + d * (params.repulsion * mass_n / dist2c);
                }
                idx = n.links.z; // accepted → skip subtree
            } else {
                idx = n.links.y; // descend into first child
            }
        }
    } else if (params.repulsion_mode == 2u) {
        // Stochastic negative sampling (DRGraph / UMAP-style). Each node
        // samples K random others per step instead of visiting spatial
        // neighbors. Skips the entire grid build (no atomics, no 27-cell
        // loop). Higher per-step variance, but per-iter is ~3-5× cheaper
        // at N>=10k. arxiv.org/abs/2008.07799
        let k = params.repulsion_samples;
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
            let d = pos - positions_in[j];
            let dist2 = dot(d, d);
            if (dist2 > r_clip2) { continue; }
            let dist2c = max(dist2, dist2_floor);
            // Same mass-weighted Coulomb form as the grid path so layout
            // quality is comparable; only the *set* of j's changes.
            force = force + d * (params.repulsion * mass[j] / dist2c);
        }
    } else if (params.grid_enabled == 1u) {
        // Walk 27 neighbor cells.
        let inv_cell = 1.0 / params.grid_cell_size;
        let rel = (pos - params.grid_origin) * inv_cell;
        let cx = i32(floor(rel.x));
        let cy = i32(floor(rel.y));
        let cz = i32(floor(rel.z));
        let dim_x = i32(params.grid_dim.x);
        let dim_y = i32(params.grid_dim.y);
        let dim_z = i32(params.grid_dim.z);
        for (var dz: i32 = -1; dz <= 1; dz = dz + 1) {
            let nz = cz + dz;
            if (nz < 0 || nz >= dim_z) { continue; }
            for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
                let ny = cy + dy;
                if (ny < 0 || ny >= dim_y) { continue; }
                for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                    let nx = cx + dx;
                    if (nx < 0 || nx >= dim_x) { continue; }
                    let cell_idx = u32(nx) + u32(ny) * params.grid_dim.x
                        + u32(nz) * params.grid_dim.x * params.grid_dim.y;
                    let start = cell_offsets[cell_idx];
                    let end   = cell_offsets[cell_idx + 1u];
                    for (var k: u32 = start; k < end; k = k + 1u) {
                        let j = cell_nodes[k];
                        if (j == i) { continue; }
                        let d = pos - positions_in[j];
                        let dist2 = dot(d, d);
                        if (dist2 > r_clip2) { continue; }
                        let dist2c = max(dist2, dist2_floor);
                        force = force + d * (params.repulsion * mass[j] / dist2c);
                    }
                }
            }
        }
    } else {
        for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
            if (j == i) { continue; }
            let d = pos - positions_in[j];
            let dist2 = dot(d, d);
            if (dist2 > r_clip2) { continue; }
            let dist2c = max(dist2, dist2_floor);
            force = force + d * (params.repulsion * mass[j] / dist2c);
        }
    }

    // ---- Springs (gather over virtual vertices) ----------------------------
    // The hub-split spring_step kernel has already written per-virtual
    // partials into `spring_force_partial`. Here we sum the partials that
    // belong to real vertex `i`. For non-hub vertices this is one iteration;
    // for a hub of degree D it is ceil(D/HUB_THRESHOLD) iterations — already
    // computed in parallel in spring_step.
    let v_start = node_to_virt_offsets[i];
    let v_end   = node_to_virt_offsets[i + 1u];
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
    let m = max(mass[i], 1.0);
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
    positions_out[i] = new_pos;

    // Track per-node KE proxy = |vel|^2. CPU reduces.
    energy_out[i] = dot(vel, vel);
}
